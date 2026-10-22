//! The coder's change workflow: `patch.propose`, `patch.apply` and
//! `repo.commit`.
//!
//! A coder never writes the repository directly. It proposes a patch: whole
//! new file contents, checked against the write scope the lead gave it and
//! the confined workspace, recorded with the exact content each file had
//! (its base) and the workspace's commit when it is a git checkout. The
//! result is a reviewable unified diff under a patch id.
//!
//! The lead turns a patch into a change, each step under its own approval
//! (both are [`RiskClass::Mutating`]):
//! - `patch.apply` writes it into the workspace only while every file still
//!   has its base content and the checkout is still at the base commit, so a
//!   stale base or a concurrent edit is refused, never overwritten;
//! - `repo.commit` commits it to an operator-allowlisted branch through the
//!   GitHub API, as a fast-forward of exactly the base commit, so it can
//!   never replace anyone else's work. The commit is then built and verified
//!   only by GitHub Actions, through the workflow broker, at its exact SHA.
//!
//! A patch id names one immutable change for the run's lifetime: ids are
//! never reused, and the approval prompt shows the patch's files, base and a
//! content identifier. A patch remembers the worker that proposed it; its
//! assignment and ownership, and the run's authorization, are checked again
//! immediately before anything is written or the branch is moved, so a
//! worker withdrawn after proposing can no longer have its change used.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lightagent_core::{RiskClass, Scope, ToolOutcome};
use reqwest::Method;
use serde_json::{Value, json};

use crate::builtins::actions::{api, api_error, ref_allowed, valid_branch, valid_sha};
use crate::context::ToolCtx;
use crate::definition::{Tool, ToolDefinition};

/// The most files one patch may change.
const MAX_FILES: usize = 20;
/// The longest commit message accepted.
const MAX_MESSAGE: usize = 2_000;

/// One file of a patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchFile {
    /// Workspace-relative, which is repository-relative in a checkout.
    pub path: String,
    /// The content the file had when the patch was proposed; `None` for a
    /// new file.
    pub base: Option<String>,
    pub content: String,
}

/// A proposed change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub id: String,
    pub summary: String,
    /// The workspace's commit at proposal, when it is a git checkout.
    pub base_commit: Option<String>,
    pub files: Vec<PatchFile>,
}

/// The worker that proposed a patch: its label, and the check that its
/// assignment and ownership still hold (none for a built-in or the lead).
#[derive(Clone)]
pub struct Proposer {
    pub label: String,
    pub validity: Option<crate::worker::WorkerValidity>,
}

impl std::fmt::Debug for Proposer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proposer")
            .field("label", &self.label)
            .field("revocable", &self.validity.is_some())
            .finish()
    }
}

impl Proposer {
    /// Whether the proposer's patch may still be used.
    fn check(&self) -> Result<(), String> {
        match &self.validity {
            Some(valid) => valid().map_err(|_| {
                format!(
                    "the worker that proposed this patch ({}) is no longer authorized, so it was \
                     not used",
                    self.label
                )
            }),
            None => Ok(()),
        }
    }
}

/// The patches proposed in one run, shared by the lead and its workers.
#[derive(Clone, Debug, Default)]
pub struct PatchStore {
    inner: Arc<Mutex<BTreeMap<String, (Patch, Option<Proposer>)>>>,
}

impl PatchStore {
    /// Record `patch`, refusing an id already in use: an id names one
    /// change for the run's lifetime.
    fn insert(&self, patch: Patch, proposer: Option<Proposer>) -> bool {
        match self.inner.lock() {
            Ok(mut inner) if !inner.contains_key(&patch.id) => {
                inner.insert(patch.id.clone(), (patch, proposer));
                true
            }
            _ => false,
        }
    }

    /// The patch `id`, if it was proposed in this run.
    pub fn get(&self, id: &str) -> Option<Patch> {
        Some(self.inner.lock().ok()?.get(id)?.0.clone())
    }

    /// Whether patch `id` may still be used: its proposer is still
    /// authorized. Unknown ids are refused.
    pub fn still_valid(&self, id: &str) -> Result<(), String> {
        let proposer = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| "the patch store is unavailable".to_owned())?;
            let (_, proposer) = inner
                .get(id)
                .ok_or_else(|| format!("no patch {id:?} was proposed in this run"))?;
            proposer.clone()
        };
        proposer.map_or(Ok(()), |proposer| proposer.check())
    }

    /// What approving a change to patch `id` writes, for an approval prompt:
    /// its files, its base and an identifier of its content.
    pub fn describe(&self, id: &str) -> Option<String> {
        let patch = self.get(id)?;
        let files: Vec<&str> = patch.files.iter().map(|file| file.path.as_str()).collect();
        Some(format!(
            "patch={id} base={} files={} content={}",
            patch.base_commit.as_deref().unwrap_or("none"),
            files.join(","),
            content_id(&patch)
        ))
    }
}

/// An identifier of a patch's content, so an approval names what it
/// approves. Not a security hash: the store never changes a recorded patch.
fn content_id(patch: &Patch) -> String {
    let mut text = String::new();
    for file in &patch.files {
        text.push_str(&file.path);
        text.push('\0');
        text.push_str(&file.content);
        text.push('\0');
    }
    fingerprint(&text)
}

/// The patch tools' context: the run's store, the paths a worker may
/// change (`None` for the lead, which only applies or commits patches whose
/// scope was checked when they were proposed), and the worker proposing
/// (`None` for the lead).
#[derive(Clone, Debug, Default)]
pub struct PatchContext {
    pub store: PatchStore,
    pub scope: Option<Vec<String>>,
    pub proposer: Option<Proposer>,
}

/// Whether `path` is inside one of `patterns`: an exact path, or `dir/**`.
pub fn in_scope(patterns: &[String], path: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| match pattern.strip_suffix("/**") {
            Some(dir) => path.starts_with(&format!("{dir}/")),
            None => pattern == path,
        })
}

/// The commit a git checkout rooted at `root` is at, read from its files
/// (nothing is executed). `None` when `root` is not a checkout.
pub fn head_commit(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let (git_dir, common_dir) = if dot_git.is_dir() {
        (dot_git.clone(), dot_git)
    } else {
        // A linked worktree: `.git` names its git dir, whose `commondir`
        // names the shared one holding the refs.
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        let git_dir = root.join(pointer.trim().strip_prefix("gitdir:")?.trim());
        let common = std::fs::read_to_string(git_dir.join("commondir"))
            .map(|common| git_dir.join(common.trim()))
            .unwrap_or_else(|_| git_dir.clone());
        (git_dir, common)
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return valid_sha(head).then(|| head.to_ascii_lowercase());
    };
    if let Ok(sha) = std::fs::read_to_string(common_dir.join(reference)) {
        let sha = sha.trim();
        return valid_sha(sha).then(|| sha.to_ascii_lowercase());
    }
    let packed = std::fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(_, name)| *name == reference)
        .map(|(sha, _)| sha.to_ascii_lowercase())
        .filter(|sha| valid_sha(sha))
}

/// A unified diff of one file: one hunk around the changed lines, with up to
/// three lines of context. Exact, if not minimal.
pub fn unified_diff(path: &str, old: Option<&str>, new: &str) -> String {
    let before: Vec<&str> = old.map(|text| text.lines().collect()).unwrap_or_default();
    let after: Vec<&str> = new.lines().collect();
    let prefix = before
        .iter()
        .zip(&after)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = prefix.saturating_sub(3);
    let old_end = (before.len() - suffix + 3).min(before.len());
    let new_end = (after.len() - suffix + 3).min(after.len());
    let from = if old.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_owned()
    };
    let mut diff = format!(
        "--- {from}\n+++ b/{path}\n@@ -{},{} +{},{} @@\n",
        start + 1,
        old_end - start,
        start + 1,
        new_end - start
    );
    for line in &before[start..prefix] {
        diff.push_str(&format!(" {line}\n"));
    }
    for line in &before[prefix..before.len() - suffix] {
        diff.push_str(&format!("-{line}\n"));
    }
    for line in &after[prefix..after.len() - suffix] {
        diff.push_str(&format!("+{line}\n"));
    }
    for line in &after[after.len() - suffix..new_end] {
        diff.push_str(&format!(" {line}\n"));
    }
    diff
}

/// A short fingerprint of a file's content (FNV-1a, 64 bits), to name a
/// base in a result. Not a security hash: bases are compared byte for byte.
pub fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn new_id() -> String {
    let id: String = lightagent_core::RunId::new()
        .as_str()
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(16)
        .collect();
    format!("pt-{id}")
}

/// The current content of a workspace file, `None` when it does not exist.
fn current(ctx: &ToolCtx, path: &str) -> Result<Option<String>, String> {
    let Some(ws) = &ctx.workspace else {
        return Err("no workspace is available for this run".to_owned());
    };
    match ws.workspace.resolve_existing(path) {
        Ok(resolved) if resolved.is_file() => std::fs::read_to_string(resolved)
            .map(Some)
            .map_err(|error| format!("could not read {path:?}: {error}")),
        Ok(_) => Err(format!("{path:?} is not a file")),
        Err(_) => {
            // Missing, as long as the path itself is confined.
            ws.workspace.resolve_new(path)?;
            Ok(None)
        }
    }
}

/// `patch.propose`.
pub struct PatchPropose {
    definition: ToolDefinition,
}

impl PatchPropose {
    pub const NAME: &'static str = "patch.propose";

    pub fn new() -> Self {
        let parameters = json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string", "description": "What the change does and why." },
                "changes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Workspace-relative path." },
                            "content": { "type": "string", "description": "The file's complete new content." }
                        },
                        "required": ["path", "content"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["summary", "changes"],
            "additionalProperties": false,
        });
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Propose a change as complete new file contents; nothing is written. Returns a patch id and a unified diff the lead can review, apply or commit.",
                parameters,
                RiskClass::Observe,
                vec![Scope::new("fs:read")],
            ),
        }
    }
}

impl Default for PatchPropose {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PatchPropose {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        match propose(args, ctx) {
            Ok(report) => ToolOutcome::ok(report.to_string()),
            Err(error) => ToolOutcome::error(error),
        }
    }
}

fn propose(args: &Value, ctx: &ToolCtx) -> Result<Value, String> {
    let Some(patches) = &ctx.patches else {
        return Err("patches are not available for this run".to_owned());
    };
    let Some(ws) = &ctx.workspace else {
        return Err("no workspace is available for this run".to_owned());
    };
    if let Some(scope) = &patches.scope
        && scope.is_empty()
    {
        return Err(
            "no write scope was given for this task, so no change can be proposed".to_owned(),
        );
    }
    let changes = args
        .get("changes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if changes.is_empty() || changes.len() > MAX_FILES {
        return Err(format!("a patch changes between 1 and {MAX_FILES} files"));
    }
    let mut files = Vec::new();
    for change in &changes {
        let path = change["path"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("./");
        let content = change["content"].as_str().unwrap_or_default();
        if content.len() > ws.policy.max_file_bytes {
            return Err(format!(
                "{path:?} is over the {}-byte limit",
                ws.policy.max_file_bytes
            ));
        }
        if let Some(scope) = &patches.scope
            && !in_scope(scope, path)
        {
            return Err(format!(
                "{path:?} is outside this task's write scope {scope:?}"
            ));
        }
        if files.iter().any(|file: &PatchFile| file.path == path) {
            return Err(format!("{path:?} appears twice"));
        }
        let base = current(ctx, path)?;
        files.push(PatchFile {
            path: path.to_owned(),
            base,
            content: content.to_owned(),
        });
    }
    let patch = Patch {
        id: new_id(),
        summary: args["summary"].as_str().unwrap_or_default().to_owned(),
        base_commit: head_commit(ws.workspace.root()),
        files,
    };
    let report = json!({
        "patch_id": patch.id,
        "base_commit": patch.base_commit,
        "files": patch.files.iter().map(|file| json!({
            "path": file.path,
            "change": if file.base.is_some() { "modify" } else { "create" },
            "base_fingerprint": file.base.as_deref().map(fingerprint),
            "diff": unified_diff(&file.path, file.base.as_deref(), &file.content),
        })).collect::<Vec<_>>(),
        "note": "Nothing was written. The lead applies or commits this patch under its own approval.",
    });
    if !patches.store.insert(patch, patches.proposer.clone()) {
        return Err("the patch could not be recorded".to_owned());
    }
    Ok(report)
}

/// The patch `id`, or why not.
fn patch(ctx: &ToolCtx, args: &Value) -> Result<Patch, String> {
    let Some(patches) = &ctx.patches else {
        return Err("patches are not available for this run".to_owned());
    };
    let id = args["patch_id"].as_str().unwrap_or_default();
    patches
        .store
        .get(id)
        .ok_or_else(|| format!("no patch {id:?} was proposed in this run"))
}

/// Everything that must still hold the moment before a patch changes
/// anything: the run's authorization and the proposing worker's.
fn still_authorized(ctx: &ToolCtx, patch: &Patch) -> Result<(), String> {
    ctx.authorized()?;
    match &ctx.patches {
        Some(patches) => patches.store.still_valid(&patch.id),
        None => Err("patches are not available for this run".to_owned()),
    }
}

/// Refuse a patch whose base is gone: a file changed since it was proposed,
/// or the checkout moved to another commit.
fn check_base(ctx: &ToolCtx, patch: &Patch) -> Result<(), String> {
    let conflicts: Vec<&str> = patch
        .files
        .iter()
        .filter(|file| current(ctx, &file.path).ok().as_ref() != Some(&file.base))
        .map(|file| file.path.as_str())
        .collect();
    if !conflicts.is_empty() {
        return Err(format!(
            "these files changed since the patch was proposed, so it was not applied: {conflicts:?}"
        ));
    }
    if let (Some(base), Some(ws)) = (&patch.base_commit, &ctx.workspace) {
        let now = head_commit(ws.workspace.root());
        if now.as_deref() != Some(base.as_str()) {
            return Err(format!(
                "the checkout moved from {base} to {}, so the patch's base is stale",
                now.as_deref().unwrap_or("an unknown commit")
            ));
        }
    }
    Ok(())
}

fn patch_parameters(extra: Value) -> Value {
    let mut properties = json!({ "patch_id": { "type": "string" } });
    let mut required = vec!["patch_id".to_owned()];
    if let Some(extra) = extra.as_object() {
        for (name, schema) in extra {
            properties[name] = schema.clone();
            required.push(name.clone());
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// `patch.apply`.
pub struct PatchApply {
    definition: ToolDefinition,
}

impl PatchApply {
    pub const NAME: &'static str = "patch.apply";

    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Write a proposed patch into the workspace, only if every file still has its base content.",
                patch_parameters(json!({})),
                RiskClass::Mutating,
                vec![Scope::new("fs:write")],
            ),
        }
    }
}

impl Default for PatchApply {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PatchApply {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        if let Err(reason) = ctx.authorized() {
            return ToolOutcome::error(reason);
        }
        let patch = match patch(ctx, args) {
            Ok(patch) => patch,
            Err(error) => return ToolOutcome::error(error),
        };
        if let Err(error) = check_base(ctx, &patch) {
            return ToolOutcome::error(error);
        }
        // Checked last, immediately before the first write.
        if let Err(error) = still_authorized(ctx, &patch) {
            return ToolOutcome::error(error);
        }
        let Some(ws) = &ctx.workspace else {
            return ToolOutcome::error("no workspace is available for this run");
        };
        for file in &patch.files {
            let written = ws.workspace.resolve_new(&file.path).and_then(|path| {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                std::fs::write(&path, &file.content).map_err(|error| error.to_string())
            });
            if let Err(error) = written {
                return ToolOutcome::error(format!("could not write {:?}: {error}", file.path));
            }
        }
        let paths: Vec<&str> = patch.files.iter().map(|file| file.path.as_str()).collect();
        ToolOutcome::ok(format!("applied {} to {paths:?}", patch.id))
    }
}

/// `repo.commit`.
pub struct RepoCommit {
    definition: ToolDefinition,
}

impl RepoCommit {
    pub const NAME: &'static str = "repo.commit";

    pub fn new() -> Self {
        let parameters = patch_parameters(json!({
            "branch": { "type": "string", "description": "An allowlisted branch." },
            "message": { "type": "string" }
        }));
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Commit a proposed patch to an allowlisted branch as a fast-forward of exactly its base commit. Verify the new commit with actions.dispatch.",
                parameters,
                RiskClass::Mutating,
                vec![Scope::new("repo:write")],
            ),
        }
    }
}

impl Default for RepoCommit {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for RepoCommit {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        if let Err(reason) = ctx.authorized() {
            return ToolOutcome::error(reason);
        }
        match commit(args, ctx).await {
            Ok(report) => ToolOutcome::ok(report.to_string()),
            Err(error) => ToolOutcome::error(error),
        }
    }
}

async fn commit(args: &Value, ctx: &ToolCtx) -> Result<Value, String> {
    let Some(actions) = &ctx.actions else {
        return Err("no repository broker is configured for this run".to_owned());
    };
    let patch = patch(ctx, args)?;
    still_authorized(ctx, &patch)?;
    let branch = args["branch"].as_str().unwrap_or_default();
    let message = args["message"].as_str().unwrap_or_default();
    if !valid_branch(branch) || !ref_allowed(&actions.policy.commit_refs, branch) {
        return Err(format!(
            "the branch '{branch}' is not allowlisted for commits"
        ));
    }
    if message.trim().is_empty() || message.len() > MAX_MESSAGE {
        return Err(format!("a commit message is 1 to {MAX_MESSAGE} bytes"));
    }
    let Some(base) = patch.base_commit.clone() else {
        return Err("the patch has no base commit: its workspace is not a git checkout".to_owned());
    };
    let repository = &actions.policy.repository;
    let head = api(
        actions,
        Method::GET,
        &format!("/repos/{repository}/git/ref/heads/{branch}"),
        None,
    )
    .await?;
    if !head.status.is_success() {
        return Err(api_error("reading the branch", &head));
    }
    let current = head.body.pointer("/object/sha").and_then(Value::as_str);
    if current != Some(base.as_str()) {
        return Err(format!(
            "the branch '{branch}' is at {}, not the patch's base {base}: refusing a commit that \
             would not be a fast-forward of exactly what the patch was made from",
            current.unwrap_or("an unknown commit")
        ));
    }
    let base_commit = api(
        actions,
        Method::GET,
        &format!("/repos/{repository}/git/commits/{base}"),
        None,
    )
    .await?;
    let Some(base_tree) = base_commit
        .body
        .pointer("/tree/sha")
        .and_then(Value::as_str)
    else {
        return Err(api_error("reading the base commit", &base_commit));
    };
    let mut entries = Vec::new();
    for file in &patch.files {
        let body = json!({ "content": file.content, "encoding": "utf-8" });
        let blob = api(
            actions,
            Method::POST,
            &format!("/repos/{repository}/git/blobs"),
            Some(&body),
        )
        .await?;
        let Some(sha) = blob.body["sha"].as_str() else {
            return Err(api_error("storing a file", &blob));
        };
        entries.push(json!({ "path": file.path, "mode": "100644", "type": "blob", "sha": sha }));
    }
    let tree_body = json!({ "base_tree": base_tree, "tree": entries });
    let tree = api(
        actions,
        Method::POST,
        &format!("/repos/{repository}/git/trees"),
        Some(&tree_body),
    )
    .await?;
    let Some(tree_sha) = tree.body["sha"].as_str() else {
        return Err(api_error("building the tree", &tree));
    };
    let commit_body = json!({ "message": message, "tree": tree_sha, "parents": [base] });
    let created = api(
        actions,
        Method::POST,
        &format!("/repos/{repository}/git/commits"),
        Some(&commit_body),
    )
    .await?;
    let Some(sha) = created.body["sha"].as_str().map(str::to_owned) else {
        return Err(api_error("creating the commit", &created));
    };
    // The objects above are unreferenced until the branch moves: check the
    // authority again immediately before moving it.
    still_authorized(ctx, &patch)?;
    // Fast-forward only: a branch that moved meanwhile is refused here.
    let update = json!({ "sha": sha, "force": false });
    let moved = api(
        actions,
        Method::PATCH,
        &format!("/repos/{repository}/git/refs/heads/{branch}"),
        Some(&update),
    )
    .await?;
    if !moved.status.is_success() {
        return Err(api_error("moving the branch", &moved));
    }
    Ok(json!({
        "commit": sha,
        "branch": branch,
        "base": base,
        "patch_id": patch.id,
        "files": patch.files.iter().map(|file| file.path.clone()).collect::<Vec<_>>(),
        "note": "Not verified yet: dispatch an allowlisted workflow on this exact commit.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_exact_or_a_directory() {
        let scope = vec!["src/lib.rs".to_owned(), "tests/**".to_owned()];
        assert!(in_scope(&scope, "src/lib.rs"));
        assert!(in_scope(&scope, "tests/a/b.rs"));
        assert!(!in_scope(&scope, "src/main.rs"));
        assert!(!in_scope(&scope, "tests"));
        assert!(!in_scope(&scope, "testsuite/x.rs"));
    }

    #[test]
    fn a_diff_shows_only_the_changed_lines_in_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\n";
        let diff = unified_diff("x.txt", Some(old), new);
        assert_eq!(
            diff,
            "--- a/x.txt\n+++ b/x.txt\n@@ -2,7 +2,7 @@\n b\n c\n d\n-e\n+E\n f\n g\n h\n"
        );
        let created = unified_diff("new.txt", None, "x\n");
        assert_eq!(
            created,
            "--- /dev/null\n+++ b/new.txt\n@@ -1,0 +1,1 @@\n+x\n"
        );
    }

    #[test]
    fn the_head_commit_is_read_from_the_checkout() {
        let root = std::env::temp_dir().join(format!(
            "lightagent-head-{}",
            lightagent_core::RunId::new().as_str()
        ));
        let sha = "0123456789abcdef0123456789abcdef01234567";
        std::fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join(".git/refs/heads/main"), format!("{sha}\n")).unwrap();
        assert_eq!(head_commit(&root).as_deref(), Some(sha));
        std::fs::remove_file(root.join(".git/refs/heads/main")).unwrap();
        std::fs::write(
            root.join(".git/packed-refs"),
            format!("# pack-refs\n{sha} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(head_commit(&root).as_deref(), Some(sha));
        std::fs::write(root.join(".git/HEAD"), format!("{sha}\n")).unwrap();
        assert_eq!(head_commit(&root).as_deref(), Some(sha));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(head_commit(&root), None);
    }
}
