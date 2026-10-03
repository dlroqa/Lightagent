//! The session model and its store.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use lightagent_core::paths;
use lightagent_core::{AgentEvent, ProfileHandle};
use serde::{Deserialize, Serialize};

use crate::error::StoreError;

/// Bytes of randomness in an id. 128 bits: a collision is not a concern.
const ID_BYTES: usize = 16;

/// A session id: 32 lowercase hex characters, generated and never accepted from
/// a caller.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Mint a new id. Infallible: OS entropy when available, a
    /// clock-plus-counter fallback when not.
    pub fn generate() -> Self {
        let mut bytes = [0u8; ID_BYTES];
        if getrandom::fill(&mut bytes).is_err() {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            bytes.copy_from_slice(&nanos.to_le_bytes()[..ID_BYTES]);
        }
        Self(hex(&bytes))
    }

    /// Parse an id, accepting only the generated shape.
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        let ok = value.len() == ID_BYTES * 2
            && value
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(StoreError::MalformedId(value.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// One message in a session's transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    /// When this message was added to the transcript. Older session files did
    /// not record this, so keep it optional when reading existing history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<SystemTime>,
}

impl StoredMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            created_at: Some(SystemTime::now()),
        }
    }
}

/// A record of one tool call within a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolHistoryEntry {
    /// Stable call id, linking the request to its result.
    #[serde(default)]
    pub id: String,
    pub tool: String,
    #[serde(default)]
    pub arguments_preview: String,
    /// A bounded excerpt of the result for later context and inspection.
    #[serde(default)]
    pub result_excerpt: String,
    /// File path or URL supplied to the tool, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    /// `"ok"` or `"error"`.
    pub outcome: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// Metadata for one agent run within a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub started_at: SystemTime,
    #[serde(default)]
    pub ended_at: Option<SystemTime>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub tools: Vec<ToolHistoryEntry>,
}

/// A persisted conversation with its run and tool history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub profile: String,
    /// ACP workspace root, when this session was opened by an editor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: String,
    /// Whether this conversation is pinned above ordinary recent chats.
    #[serde(default)]
    pub pinned: bool,
    /// Whether this conversation has been archived by the user.
    #[serde(default)]
    pub archived: bool,
    /// An optional user-assigned project grouping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    #[serde(default)]
    pub messages: Vec<StoredMessage>,
    /// An explicit terminal-chat choice to relax approvals for this session.
    #[serde(default)]
    pub approvals_unrestricted: bool,
    #[serde(default)]
    pub runs: Vec<RunRecord>,
}

impl Session {
    /// A fresh, empty session for `profile`.
    pub fn new(profile: impl Into<String>, title: impl Into<String>) -> Self {
        let now = SystemTime::now();
        Self {
            id: SessionId::generate(),
            profile: profile.into(),
            cwd: None,
            title: title.into(),
            pinned: false,
            archived: false,
            project: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            approvals_unrestricted: false,
            runs: Vec::new(),
        }
    }

    /// Append a message and stamp the update time.
    pub fn push_message(&mut self, message: StoredMessage) {
        self.messages.push(message);
        self.updated_at = SystemTime::now();
    }

    /// Append a run record and stamp the update time.
    pub fn push_run(&mut self, run: RunRecord) {
        self.runs.push(run);
        self.updated_at = SystemTime::now();
    }

    /// Stamp a user-visible metadata change.
    pub fn touch(&mut self) {
        self.updated_at = SystemTime::now();
    }

    /// Record a managed run's assistant answer and tool history.
    pub fn record_run_events(&mut self, events: &[AgentEvent], stop_reason: &str) {
        let mut run_id = String::new();
        let mut content = String::new();
        let mut names: HashMap<String, String> = HashMap::new();
        let mut arguments: HashMap<String, String> = HashMap::new();
        let mut tools = Vec::new();

        for event in events {
            match event {
                AgentEvent::RunStarted { run, .. } => run_id = run.as_str().to_owned(),
                AgentEvent::Content { text } => content.push_str(text),
                AgentEvent::ToolCallRequested { call } => {
                    arguments.insert(call.id.clone(), call.arguments.clone());
                }
                AgentEvent::ToolCallStarted { id, name } => {
                    names.insert(id.clone(), name.clone());
                }
                AgentEvent::ToolCallCompleted { id, outcome } => tools.push(ToolHistoryEntry {
                    id: id.clone(),
                    tool: names.get(id).cloned().unwrap_or_else(|| id.clone()),
                    arguments_preview: arguments
                        .get(id)
                        .map(|value| value.chars().take(120).collect())
                        .unwrap_or_default(),
                    result_excerpt: outcome.content.chars().take(2_000).collect(),
                    source: arguments
                        .get(id)
                        .and_then(|value| source_from_arguments(value)),
                    truncated: outcome.content.chars().count() > 2_000,
                    outcome: if outcome.is_error { "error" } else { "ok" }.to_owned(),
                    duration_ms: None,
                }),
                _ => {}
            }
        }

        if !content.is_empty() {
            self.push_message(StoredMessage::new("assistant", content));
        }
        let now = SystemTime::now();
        self.push_run(RunRecord {
            run_id,
            started_at: now,
            ended_at: Some(now),
            stop_reason: Some(stop_reason.to_owned()),
            tools,
        });
    }
}

fn source_from_arguments(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    ["path", "file", "url", "source"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

/// A light view of a session for a listing, without its transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub profile: String,
    pub title: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub updated_at: SystemTime,
    pub message_count: usize,
    pub run_count: usize,
}

/// A session summary accompanied by the first matching transcript excerpt.
///
/// Search results deliberately omit full transcripts: the sidebar needs enough
/// context to identify a conversation without sending every saved message to
/// the browser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchResult {
    #[serde(flatten)]
    pub session: SessionSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

impl SessionSummary {
    /// Build a transcript-free view of a session.
    pub fn of(session: &Session) -> Self {
        Self {
            id: session.id.clone(),
            profile: session.profile.clone(),
            title: session.title.clone(),
            pinned: session.pinned,
            archived: session.archived,
            project: session.project.clone(),
            updated_at: session.updated_at,
            message_count: session.messages.len(),
            run_count: session.runs.len(),
        }
    }
}

/// Reads and writes sessions under one directory (a profile's `sessions/`).
#[derive(Clone, Debug)]
pub struct SessionStore {
    directory: PathBuf,
    keep_history: bool,
}

impl SessionStore {
    /// A store rooted at `directory`, keeping history.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            keep_history: true,
        }
    }

    /// The store for a profile's `sessions/` directory.
    pub fn at_profile(handle: &ProfileHandle) -> Self {
        Self::new(handle.sessions_dir())
    }

    /// Set whether writes are persisted. With history off, `save` is a no-op and
    /// reads still work.
    pub fn keep_history(mut self, keep: bool) -> Self {
        self.keep_history = keep;
        self
    }

    /// Whether this store persists writes.
    pub fn is_history_kept(&self) -> bool {
        self.keep_history
    }

    fn path_for(&self, id: &SessionId) -> PathBuf {
        self.directory.join(format!("{}.json", id.as_str()))
    }

    /// Save an uploaded attachment next to its session with owner-only access.
    pub fn save_attachment(
        &self,
        id: &SessionId,
        filename: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        let directory = self
            .directory
            .parent()
            .unwrap_or(&self.directory)
            .join("attachments")
            .join(id.as_str());
        paths::create_private_dir(&directory).map_err(|err| StoreError::Directory {
            path: directory.clone(),
            reason: err.to_string(),
        })?;
        let path = directory.join(filename);
        paths::write_private(&path, bytes).map_err(|err| StoreError::Unwritable {
            id: id.as_str().to_owned(),
            reason: err.to_string(),
        })?;
        Ok(path)
    }

    /// Persist a session atomically and owner-only. A no-op when history is off.
    pub fn save(&self, session: &Session) -> Result<(), StoreError> {
        if !self.keep_history {
            return Ok(());
        }
        paths::create_private_dir(&self.directory).map_err(|err| StoreError::Directory {
            path: self.directory.clone(),
            reason: err.to_string(),
        })?;
        let mut bytes =
            serde_json::to_vec_pretty(session).map_err(|err| StoreError::Unwritable {
                id: session.id.as_str().to_owned(),
                reason: err.to_string(),
            })?;
        bytes.push(b'\n');
        paths::write_private(&self.path_for(&session.id), &bytes).map_err(|err| {
            StoreError::Unwritable {
                id: session.id.as_str().to_owned(),
                reason: err.to_string(),
            }
        })
    }

    /// Load a session by id.
    pub fn load(&self, id: &SessionId) -> Result<Session, StoreError> {
        let bytes = match std::fs::read(self.path_for(id)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(id.as_str().to_owned()));
            }
            Err(err) => {
                return Err(StoreError::Unreadable {
                    id: id.as_str().to_owned(),
                    reason: err.to_string(),
                });
            }
        };
        serde_json::from_slice(&bytes).map_err(|err| StoreError::Unreadable {
            id: id.as_str().to_owned(),
            reason: err.to_string(),
        })
    }

    /// Delete a session. Returns whether a file was removed.
    pub fn delete(&self, id: &SessionId) -> Result<bool, StoreError> {
        match std::fs::remove_file(self.path_for(id)) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(StoreError::Unwritable {
                id: id.as_str().to_owned(),
                reason: err.to_string(),
            }),
        }
    }

    /// List sessions newest-first, isolating any one damaged record.
    pub fn list(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(StoreError::Directory {
                    path: self.directory.clone(),
                    reason: err.to_string(),
                });
            }
        };

        // Order by mtime before opening anything, so a large store does not
        // parse every file to show a page of the newest.
        let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            candidates.push((modified, path));
        }
        candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

        let mut summaries = Vec::new();
        for (_, path) in candidates {
            let Ok(bytes) = std::fs::read(&path) else {
                continue; // an entry that vanished between listing and reading
            };
            match serde_json::from_slice::<Session>(&bytes) {
                Ok(session) => summaries.push(SessionSummary::of(&session)),
                Err(_) => continue, // one damaged record costs one record
            }
        }
        // Re-sort on the recorded time: a backup restore rewrites every mtime at
        // once, and the order the user remembers is the one in the file.
        summaries.sort_by_key(|summary| {
            (
                std::cmp::Reverse(summary.pinned),
                std::cmp::Reverse(summary.updated_at),
            )
        });
        Ok(summaries)
    }

    /// Search session metadata and saved messages, newest-first.
    ///
    /// The returned snippets are bounded and normalized for display. This keeps
    /// history private by default while allowing the conversation picker to
    /// search both user prompts and agent responses.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionSearchResult>, StoreError> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(StoreError::Directory {
                    path: self.directory.clone(),
                    reason: err.to_string(),
                });
            }
        };

        let mut results = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(session) = serde_json::from_slice::<Session>(&bytes) else {
                continue;
            };
            let metadata_matches = [
                session.title.as_str(),
                session.profile.as_str(),
                session.project.as_deref().unwrap_or_default(),
            ]
            .into_iter()
            .any(|value| value.to_lowercase().contains(&needle));
            let message_match = session.messages.iter().find_map(|message| {
                message_match_excerpt(&message.content, &needle)
                    .map(|snippet| (message.role.clone(), snippet))
            });
            if !metadata_matches && message_match.is_none() {
                continue;
            }
            let (matched_role, snippet) = message_match
                .map(|(role, snippet)| (Some(role), Some(snippet)))
                .unwrap_or((None, None));
            results.push(SessionSearchResult {
                session: SessionSummary::of(&session),
                matched_role,
                snippet,
            });
        }
        results.sort_by_key(|result| {
            (
                std::cmp::Reverse(result.session.pinned),
                std::cmp::Reverse(result.session.updated_at),
            )
        });
        results.truncate(limit);
        Ok(results)
    }
}

fn message_match_excerpt(content: &str, needle: &str) -> Option<String> {
    let match_offset = content.char_indices().find_map(|(offset, _)| {
        content[offset..]
            .to_lowercase()
            .starts_with(needle)
            .then_some(offset)
    })?;
    let start = content[..match_offset].chars().count().saturating_sub(48);
    let excerpt: String = content.chars().skip(start).take(180).collect();
    let normalized = excerpt.split_whitespace().collect::<Vec<_>>().join(" ");
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if content.chars().count() > start + 180 {
        "…"
    } else {
        ""
    };
    Some(format!("{prefix}{normalized}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightagent_core::{ToolCall, ToolOutcome};

    fn scratch_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "lightagent-store-{}",
            SessionId::generate().as_str()
        ))
    }

    #[test]
    fn ids_are_generated_and_only_the_generated_shape_parses() {
        let id = SessionId::generate();
        assert_eq!(id.as_str().len(), 32);
        assert!(SessionId::parse(id.as_str()).is_ok());
        assert!(matches!(
            SessionId::parse("../etc"),
            Err(StoreError::MalformedId(_))
        ));
        assert!(matches!(
            SessionId::parse("ABCDEF"),
            Err(StoreError::MalformedId(_))
        ));
    }

    #[test]
    fn a_session_round_trips_and_survives_a_new_store() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let mut session = Session::new("default", "First chat");
        session.push_message(StoredMessage::new("user", "hi"));
        session.approvals_unrestricted = true;
        session.push_run(RunRecord {
            run_id: "run-1".into(),
            started_at: SystemTime::now(),
            ended_at: Some(SystemTime::now()),
            stop_reason: Some("end_turn".into()),
            tools: vec![ToolHistoryEntry {
                id: "call-1".into(),
                tool: "datetime.now".into(),
                arguments_preview: "{}".into(),
                result_excerpt: "2026-09-12".into(),
                source: None,
                truncated: false,
                outcome: "ok".into(),
                duration_ms: Some(2),
            }],
        });
        store.save(&session).unwrap();

        // A brand-new store instance is the "after restart" case.
        let reopened = SessionStore::new(&dir);
        let loaded = reopened.load(&session.id).unwrap();
        assert_eq!(loaded, session);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_result_and_source_are_saved_with_the_session() {
        let mut session = Session::new("default", "evidence");
        session.record_run_events(
            &[
                AgentEvent::ToolCallRequested {
                    call: ToolCall {
                        id: "call-1".into(),
                        name: "fs.read".into(),
                        arguments: r#"{"path":"notes.md"}"#.into(),
                    },
                },
                AgentEvent::ToolCallStarted {
                    id: "call-1".into(),
                    name: "fs.read".into(),
                },
                AgentEvent::ToolCallCompleted {
                    id: "call-1".into(),
                    outcome: ToolOutcome::ok("The deployment checklist is here."),
                },
            ],
            "EndTurn",
        );
        let tool = &session.runs[0].tools[0];
        assert_eq!(tool.id, "call-1");
        assert_eq!(tool.source.as_deref(), Some("notes.md"));
        assert_eq!(tool.result_excerpt, "The deployment checklist is here.");
        assert!(!tool.truncated);
    }

    #[test]
    fn older_tool_records_remain_readable() {
        let tool: ToolHistoryEntry = serde_json::from_str(
            r#"{"tool":"fs.read","arguments_preview":"notes.md","outcome":"ok"}"#,
        )
        .unwrap();
        assert!(tool.id.is_empty());
        assert!(tool.result_excerpt.is_empty());
        assert_eq!(tool.source, None);
    }

    #[test]
    fn message_timestamps_are_added_without_breaking_existing_history() {
        assert!(StoredMessage::new("user", "hello").created_at.is_some());
        let message: StoredMessage =
            serde_json::from_str(r#"{"role":"user","content":"earlier prompt"}"#).unwrap();
        assert_eq!(message.created_at, None);
    }

    #[test]
    fn older_sessions_default_to_unpinned_unarchived_without_a_project() {
        let session: Session = serde_json::from_str(
            r#"{"id":"00000000000000000000000000000000","profile":"default","title":"Old chat","created_at":{"secs_since_epoch":0,"nanos_since_epoch":0},"updated_at":{"secs_since_epoch":0,"nanos_since_epoch":0}}"#,
        )
        .unwrap();
        assert!(!session.pinned);
        assert!(!session.archived);
        assert_eq!(session.project, None);
    }

    #[test]
    fn a_missing_session_is_not_found_not_malformed() {
        let store = SessionStore::new(scratch_dir());
        let id = SessionId::generate();
        assert!(matches!(store.load(&id), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn one_damaged_record_costs_one_record() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let good = Session::new("default", "Good");
        store.save(&good).unwrap();
        // A corrupt file alongside a good one.
        paths::create_private_dir(&dir).unwrap();
        std::fs::write(
            dir.join("00000000000000000000000000000000.json"),
            b"{ not json",
        )
        .unwrap();

        let listed = store.list().unwrap();
        assert_eq!(
            listed.len(),
            1,
            "the damaged record is skipped, the good one is kept"
        );
        assert_eq!(listed[0].id, good.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_finds_user_and_assistant_messages_with_context() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let mut user_match = Session::new("default", "Planning");
        user_match.push_message(StoredMessage::new(
            "user",
            "Can we schedule the lighthouse migration for Friday?",
        ));
        store.save(&user_match).unwrap();
        let mut assistant_match = Session::new("default", "Status");
        assistant_match.push_message(StoredMessage::new(
            "assistant",
            "The lighthouse migration is ready for review.",
        ));
        store.save(&assistant_match).unwrap();

        let results = store.search("Lighthouse", 20).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|result| {
            result.session.id == user_match.id
                && result.matched_role.as_deref() == Some("user")
                && result
                    .snippet
                    .as_deref()
                    .is_some_and(|snippet| snippet.contains("lighthouse"))
        }));
        assert!(results.iter().any(|result| {
            result.session.id == assistant_match.id
                && result.matched_role.as_deref() == Some("assistant")
                && result
                    .snippet
                    .as_deref()
                    .is_some_and(|snippet| snippet.contains("lighthouse"))
        }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_off_refuses_writes_but_reads_still_work() {
        let dir = scratch_dir();
        let keeping = SessionStore::new(&dir);
        let session = Session::new("default", "Saved");
        keeping.save(&session).unwrap();

        let off = SessionStore::new(&dir).keep_history(false);
        let mut later = Session::new("default", "Not saved");
        later.push_message(StoredMessage::new("user", "hello"));
        off.save(&later).unwrap(); // a no-op

        assert!(matches!(off.load(&later.id), Err(StoreError::NotFound(_))));
        // The earlier session is still readable through the history-off store.
        assert_eq!(off.load(&session.id).unwrap().id, session.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_reports_what_it_did() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let session = Session::new("default", "Temp");
        store.save(&session).unwrap();
        assert!(store.delete(&session.id).unwrap());
        assert!(!store.delete(&session.id).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_saved_session_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let session = Session::new("default", "Private");
        store.save(&session).unwrap();
        let mode = std::fs::metadata(store.path_for(&session.id))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a transcript must not be world-readable");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
