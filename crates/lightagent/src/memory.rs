//! `lightagent memory` — write, recall and manage durable memories, and the
//! `memory.write`/`memory.search` tools and per-request recall wired into a run.
//!
//! Memory is per-profile: the active profile's memories live at
//! `<profile>/memory/memories.jsonl`. The CLI edits them directly; a run is given
//! the tools over the same file and, unless disabled, a small relevant-memory
//! selection is added to each request.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lightagent_core::{Config, ConfigStore, LightagentPaths, ProfileStore};
use lightagent_memory::{
    MemoryReflect, MemorySearch, MemorySource, MemoryStore, MemoryWrite, SessionLookup,
    memory_path, retain,
};
use lightagent_rag::HashingEmbedder;
use lightagent_store::{Session, SessionId, SessionStore};
use lightagent_tools::Tool;

/// Durable recall and source lookup tools for a run.
pub(crate) fn memory_tools(profile_dir: &Path, config: &Config) -> Vec<Arc<dyn Tool>> {
    let path = memory_path(profile_dir);
    let search = MemorySearch::new(path.clone(), config.memory.top_k);
    let search = match crate::rag::semantic_embedder(config) {
        Some(semantic) => search.with_semantic(semantic),
        None => search,
    };
    vec![
        Arc::new(MemoryWrite::new(path.clone())),
        Arc::new(search),
        Arc::new(MemoryReflect::new(path)),
        Arc::new(SessionLookup::new(SessionStore::new(
            profile_dir.join("sessions"),
        ))),
    ]
}

/// Retain only explicit, durable user statements. The source points back to
/// the saved transcript when one exists; unclear statements remain there.
pub(crate) fn capture(
    profile_dir: &Path,
    config: &Config,
    message: &str,
    source: Option<MemorySource>,
) -> Result<usize, String> {
    if !config.memory.auto_capture {
        return Ok(0);
    }
    let mut store = MemoryStore::open(memory_path(profile_dir)).map_err(|e| e.to_string())?;
    retain(&mut store, message, source, now_secs()).map_err(|e| e.to_string())
}

/// Select a small set of memories for this request's prompt.
pub(crate) async fn relevant_catalog(
    profile_dir: &Path,
    config: &Config,
    query: &str,
) -> Result<String, String> {
    if config.memory.inject_recent == 0 {
        return Ok(String::new());
    }
    let store = MemoryStore::open(memory_path(profile_dir)).map_err(|error| error.to_string())?;
    let count = config.memory.inject_recent.min(config.memory.top_k).min(3);
    let semantic = crate::rag::semantic_embedder(config);
    let relevant = store
        .relevant_catalog_hybrid(query, count, 600, semantic.as_deref())
        .await;
    if !relevant.is_empty() {
        return Ok(relevant);
    }
    let fallback = store
        .recent(store.len())
        .into_iter()
        .find(|memory| memory.kind == "preference");
    Ok(fallback
        .map(|memory| {
            format!(
                "Recent durable memory [{}]: {}",
                memory.id,
                memory.text.chars().take(350).collect::<String>()
            )
        })
        .unwrap_or_default())
}

use lightagent_api::ops::{MemoryRequest, OpCategory, OpError};

/// The active profile's memory file and saved sessions, and the config.
struct Active {
    path: PathBuf,
    sessions: SessionStore,
    config: Config,
}

fn internal(error: impl std::fmt::Display) -> OpError {
    OpError::new(OpCategory::Internal, error.to_string())
}

fn active_at(paths: &LightagentPaths) -> Result<Active, OpError> {
    let config = ConfigStore::at(paths).load().map_err(internal)?;
    let store = ProfileStore::new(paths.root());
    let active = store.active().map_err(internal)?.ok_or_else(|| {
        OpError::new(
            OpCategory::Invalid,
            "no active profile — run `lightagent init` first",
        )
    })?;
    let handle = store.handle(&active);
    Ok(Active {
        path: memory_path(handle.dir()),
        sessions: SessionStore::at_profile(&handle),
        config,
    })
}

fn cli_active() -> Result<Active, String> {
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    active_at(&paths).map_err(|error| error.to_string())
}

fn open(active: &Active) -> Result<MemoryStore, OpError> {
    MemoryStore::open(&active.path).map_err(internal)
}

fn saved_session(active: &Active, raw_id: &str) -> Result<Session, OpError> {
    let id = SessionId::parse(raw_id)
        .map_err(|error| OpError::new(OpCategory::Invalid, error.to_string()))?;
    active
        .sessions
        .load(&id)
        .map_err(|error| OpError::new(OpCategory::NotFound, error.to_string()))
}

fn require_text(text: &str) -> Result<(), OpError> {
    if text.trim().is_empty() {
        return Err(OpError::new(OpCategory::Invalid, "a memory needs text"));
    }
    Ok(())
}

fn op_candidates(active: &Active, session_id: &str) -> Result<Vec<(usize, String)>, OpError> {
    let session = saved_session(active, session_id)?;
    Ok(session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            if message.role != "user" {
                return false;
            }
            let text = message.content.to_lowercase();
            [
                "prefer",
                "remember",
                "we decided",
                "always",
                "never",
                "my ",
                "our project",
                "uses ",
            ]
            .iter()
            .any(|marker| text.contains(marker))
        })
        .map(|(index, message)| (index + 1, message.content.clone()))
        .collect())
}

fn op_promote(
    active: &Active,
    session_id: &str,
    message_number: usize,
    text: Option<&str>,
    kind: Option<&str>,
    tags: Vec<String>,
) -> Result<String, OpError> {
    let session = saved_session(active, session_id)?;
    let message = session
        .messages
        .get(message_number.saturating_sub(1))
        .filter(|message| message_number > 0 && message.role == "user")
        .ok_or_else(|| {
            OpError::new(
                OpCategory::Invalid,
                "choose a user message number from the saved session",
            )
        })?;
    let fact = text.unwrap_or(&message.content);
    require_text(fact)?;
    let mut store = open(active)?;
    store
        .write_sourced(
            fact,
            kind.unwrap_or("fact"),
            tags,
            Some(MemorySource {
                session_id: session_id.to_owned(),
                message_index: message_number,
            }),
            &HashingEmbedder,
            now_secs(),
        )
        .map_err(internal)
}

fn op_update(active: &Active, id: &str, text: &str) -> Result<bool, OpError> {
    require_text(text)?;
    let mut store = open(active)?;
    store
        .update(id, text, &HashingEmbedder, now_secs())
        .map_err(internal)
}

fn op_add(
    active: &Active,
    text: &str,
    kind: Option<&str>,
    tags: Vec<String>,
) -> Result<String, OpError> {
    require_text(text)?;
    let mut store = open(active)?;
    store
        .write(
            text,
            kind.unwrap_or("fact"),
            tags,
            &HashingEmbedder,
            now_secs(),
        )
        .map_err(internal)
}

fn op_list(active: &Active) -> Result<Vec<lightagent_memory::Memory>, OpError> {
    Ok(open(active)?.all().to_vec())
}

fn op_reflect(active: &Active, topic: Option<&str>) -> Result<String, OpError> {
    Ok(open(active)?.knowledge_page(topic, 32_000))
}

async fn op_search(
    active: &Active,
    query: &str,
    top_k: Option<usize>,
) -> Result<Vec<lightagent_memory::Memory>, OpError> {
    let store = open(active)?;
    let k = top_k.unwrap_or(active.config.memory.top_k).max(1);
    let semantic = crate::rag::semantic_embedder(&active.config);
    Ok(store
        .search_hybrid(query, &HashingEmbedder, semantic.as_deref(), k)
        .await
        .into_iter()
        .cloned()
        .collect())
}

fn op_forget(active: &Active, id: &str) -> Result<bool, OpError> {
    open(active)?.forget(id).map_err(internal)
}

fn op_clear(active: &Active) -> Result<(), OpError> {
    open(active)?.clear().map_err(internal)
}

fn list_value(memories: &[lightagent_memory::Memory]) -> serde_json::Value {
    serde_json::json!({
        "memories": memories.iter().map(|m| serde_json::json!({
            "id": m.id, "kind": m.kind, "tags": m.tags, "created_at": m.created_at,
            "updated_at": m.updated_at, "source": m.source, "text": m.text,
        })).collect::<Vec<_>>(),
    })
}

fn search_value(query: &str, hits: &[lightagent_memory::Memory]) -> serde_json::Value {
    serde_json::json!({
        "query": query,
        "memories": hits.iter().map(|m| serde_json::json!({
            "id": m.id, "kind": m.kind, "source": m.source, "text": m.text,
        })).collect::<Vec<_>>(),
    })
}

fn candidates_value(session_id: &str, candidates: &[(usize, String)]) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "candidates": candidates.iter().map(|(index, text)| serde_json::json!({"message": index, "text": text})).collect::<Vec<_>>(),
    })
}

/// One memory operation for the HTTP API (`RunFactory::memory`), on the
/// active profile's memories, answered with the JSON
/// `lightagent memory … --json` prints.
pub(crate) async fn perform(
    paths: &LightagentPaths,
    request: MemoryRequest,
) -> Result<serde_json::Value, OpError> {
    let active = active_at(paths)?;
    Ok(match request {
        MemoryRequest::List => list_value(&op_list(&active)?),
        MemoryRequest::Add { text, kind, tags } => {
            serde_json::json!({ "id": op_add(&active, &text, kind.as_deref(), tags)? })
        }
        MemoryRequest::Update { id, text } => {
            serde_json::json!({ "updated": op_update(&active, &id, &text)? })
        }
        MemoryRequest::Forget(id) => serde_json::json!({ "removed": op_forget(&active, &id)? }),
        MemoryRequest::Clear => {
            op_clear(&active)?;
            serde_json::json!({ "cleared": true })
        }
        MemoryRequest::Search { query, top_k } => {
            search_value(&query, &op_search(&active, &query, top_k).await?)
        }
        MemoryRequest::Reflect(topic) => serde_json::json!({
            "topic": topic,
            "knowledge": op_reflect(&active, topic.as_deref())?,
        }),
        MemoryRequest::Candidates(session) => {
            candidates_value(&session, &op_candidates(&active, &session)?)
        }
        MemoryRequest::Promote {
            session,
            message,
            text,
            kind,
            tags,
        } => serde_json::json!({
            "id": op_promote(&active, &session, message, text.as_deref(), kind.as_deref(), tags)?
        }),
    })
}

/// Show user statements that look durable; nothing is saved until `promote`.
pub fn candidates(session_id: String, json: bool) -> Result<(), String> {
    let active = cli_active()?;
    let candidates = op_candidates(&active, &session_id).map_err(|error| error.to_string())?;
    if json {
        println!("{}", candidates_value(&session_id, &candidates));
    } else if candidates.is_empty() {
        println!("No suggested facts. Any user message can still be promoted by its number.");
    } else {
        for (index, text) in candidates {
            println!("{index}: {}", text.replace('\n', " "));
        }
    }
    Ok(())
}

/// Save exactly one user-selected message, optionally edited, with provenance.
pub fn promote(
    session_id: String,
    message_number: usize,
    text: Option<String>,
    kind: Option<String>,
    tags: Vec<String>,
    json: bool,
) -> Result<(), String> {
    let active = cli_active()?;
    let id = op_promote(
        &active,
        &session_id,
        message_number,
        text.as_deref(),
        kind.as_deref(),
        tags,
    )
    .map_err(|error| error.to_string())?;
    if json {
        println!("{}", serde_json::json!({"id": id}));
    } else {
        println!("remembered ({id})");
    }
    Ok(())
}

pub fn update(id: String, text: String, json: bool) -> Result<(), String> {
    let active = cli_active()?;
    let updated = op_update(&active, &id, &text).map_err(|error| error.to_string())?;
    if json {
        println!("{}", serde_json::json!({"updated": updated}));
    } else if updated {
        println!("updated {id}");
    } else {
        println!("no memory with id {id}");
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// `memory add <text>` — remember a fact.
pub fn add(
    text: String,
    kind: Option<String>,
    tags: Vec<String>,
    json: bool,
) -> Result<(), String> {
    let active = cli_active()?;
    let id = op_add(&active, &text, kind.as_deref(), tags).map_err(|error| error.to_string())?;
    if json {
        println!("{}", serde_json::json!({ "id": id }));
    } else {
        println!("remembered ({id})");
    }
    Ok(())
}

/// `memory list` — every memory.
pub fn list(json: bool) -> Result<(), String> {
    let active = cli_active()?;
    let memories = op_list(&active).map_err(|error| error.to_string())?;
    if json {
        println!("{:#}", list_value(&memories));
        return Ok(());
    }
    if memories.is_empty() {
        println!("No memories. Add one with `lightagent memory add <text>`.");
        return Ok(());
    }
    for memory in &memories {
        let source = memory
            .source
            .as_ref()
            .map(|source| format!("  from {}#{}", source.session_id, source.message_index))
            .unwrap_or_default();
        println!("{}  ({})  {}{source}", memory.id, memory.kind, memory.text);
    }
    Ok(())
}

/// Render the structured working knowledge derived from the bank.
pub fn reflect(topic: Option<String>, json: bool) -> Result<(), String> {
    let active = cli_active()?;
    let page = op_reflect(&active, topic.as_deref()).map_err(|error| error.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "topic": topic, "knowledge": page })
        );
    } else {
        println!("{page}");
    }
    Ok(())
}

/// `memory search <query>` — the most relevant memories.
pub async fn search(query: String, top_k: Option<usize>, json: bool) -> Result<(), String> {
    let active = cli_active()?;
    let hits = op_search(&active, &query, top_k)
        .await
        .map_err(|error| error.to_string())?;
    if json {
        println!("{:#}", search_value(&query, &hits));
        return Ok(());
    }
    if hits.is_empty() {
        println!("No relevant memories.");
        return Ok(());
    }
    for memory in &hits {
        let source = memory
            .source
            .as_ref()
            .map(|source| format!(" [from {}#{}]", source.session_id, source.message_index))
            .unwrap_or_default();
        println!("({}) {}{source}", memory.kind, memory.text);
    }
    Ok(())
}

/// `memory forget <id>` — remove one memory.
pub fn forget(id: String, json: bool) -> Result<(), String> {
    let active = cli_active()?;
    let removed = op_forget(&active, &id).map_err(|error| error.to_string())?;
    if json {
        println!("{}", serde_json::json!({ "removed": removed }));
    } else if removed {
        println!("forgot {id}");
    } else {
        println!("no memory with id {id}");
    }
    Ok(())
}

/// `memory clear` — forget everything.
pub fn clear(json: bool) -> Result<(), String> {
    let active = cli_active()?;
    op_clear(&active).map_err(|error| error.to_string())?;
    if json {
        println!("{}", serde_json::json!({ "cleared": true }));
    } else {
        println!("Memory cleared.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn automatic_fact_is_recalled_in_a_later_session() {
        let dir = std::env::temp_dir().join(format!(
            "lightagent-memory-cycle-{}",
            lightagent_core::RunId::new().as_str()
        ));
        let config = Config::default();
        capture(
            &dir,
            &config,
            "I prefer concise answers.",
            Some(MemorySource {
                session_id: "first-session".to_owned(),
                message_index: 1,
            }),
        )
        .unwrap();
        let recalled = relevant_catalog(&dir, &config, "How long should your answers be?")
            .await
            .unwrap();
        assert!(recalled.contains("I prefer concise answers"));
        let page = MemoryStore::open(memory_path(&dir))
            .unwrap()
            .knowledge_page(Some("preference"), 8_000);
        assert!(page.contains("## Preferences"));
        assert!(page.contains("first-session#1"));

        let mut disabled = config;
        disabled.memory.auto_capture = false;
        assert_eq!(
            capture(&dir, &disabled, "We decided to use SQLite.", None).unwrap(),
            0
        );
        assert_eq!(MemoryStore::open(memory_path(&dir)).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
