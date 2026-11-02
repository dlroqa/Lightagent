//! The session model and its store.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

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
    /// The principal that created the session in multi-user mode, written by
    /// the server. `None` for every session created otherwise (the terminal,
    /// ACP, the single-user API, an operator): such a session is
    /// operator-only in multi-user mode and is never assigned later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
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
            owner: None,
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
    /// See [`Session::owner`]. Absent for sessions without an owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
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
            owner: session.owner.clone(),
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

/// How long a writer waits for another writer's lock before reporting
/// [`StoreError::Busy`].
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// The store's write lock, in its directory. Not a `.json` file, so a listing
/// or a search never reads it as a session.
const LOCK_FILE: &str = ".lock";

/// Where run leases live, one empty file per session that ran a turn. Not a
/// `.json` file, so a listing or a search never reads it as a session.
const RUNS_DIR: &str = ".runs";

/// How long taking a run lease retries before the session is busy: a margin
/// for a lease its holder is releasing at that moment. A turn that holds the
/// lease is never waited for beyond this. A released lease does not stay
/// held while a child that another thread is starting keeps a copy of the
/// lock file: [`RunLease`] unlocks before it closes.
const RUN_LEASE_GRACE: Duration = Duration::from_millis(500);

/// The right to run one turn on a session, held across processes: an
/// exclusive OS lock on the session's lease file. Dropping it, or the end of
/// the holder's process (a crash included), releases it. It is separate from
/// the store's short write lock, which is never held while a turn runs.
#[derive(Debug)]
pub struct RunLease {
    file: std::fs::File,
}

impl Drop for RunLease {
    // Unlock, then close. The lock belongs to the open file, which a child
    // that another thread is starting shares until its exec closes its copy;
    // closing only this handle would keep the lease held until then.
    // Unlocking releases it at once, whoever else still has the file open.
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The stop reason recorded for a turn whose process ended before it could
/// record the turn itself.
pub const INTERRUPTED: &str = "Interrupted";

/// A turn in flight, written when an API turn starts and removed once the
/// turn is recorded. A journal that outlives the process holding the
/// session's run lease marks an interrupted turn.
#[derive(Debug, Serialize, Deserialize)]
struct RunJournal {
    run_id: String,
    started_at: SystemTime,
    /// The turn would have replaced the session's latest answer.
    #[serde(default)]
    replacing: bool,
}

/// Why [`SessionStore::update`] wrote nothing.
#[derive(Debug)]
pub enum SessionUpdateError<E> {
    /// The change itself refused, with its own error.
    Change(E),
    /// The store refused: a busy lock, or a session that is missing or could
    /// not be read or written.
    Store(StoreError),
}

impl SessionUpdateError<Infallible> {
    /// The store's error: a change that cannot fail has no other.
    pub fn into_store(self) -> StoreError {
        match self {
            Self::Store(error) => error,
            Self::Change(never) => match never {},
        }
    }
}

impl<E: std::fmt::Display> std::fmt::Display for SessionUpdateError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Change(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
        }
    }
}

/// Reads and writes sessions under one directory (a profile's `sessions/`).
///
/// Every write takes the store's lock, which every process takes, and
/// changes a session as it is saved at that moment, so no writer loses
/// another's change: see [`SessionStore::update`].
#[derive(Clone, Debug)]
pub struct SessionStore {
    directory: PathBuf,
    keep_history: bool,
    lock_wait: Duration,
}

impl SessionStore {
    /// A store rooted at `directory`, keeping history.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            keep_history: true,
            lock_wait: LOCK_WAIT,
        }
    }

    /// The same store, waiting at most `wait` for another writer's lock.
    pub fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
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

    /// Write `session` whole, atomically and owner-only, under the store's
    /// lock. A no-op when history is off. It replaces whatever is saved, so it
    /// is for a session not saved yet: a writer that changes a saved session
    /// uses [`SessionStore::update`] or [`SessionStore::apply`], which keep
    /// every other writer's change.
    pub fn save(&self, session: &Session) -> Result<(), StoreError> {
        if !self.keep_history {
            return Ok(());
        }
        let _lock = self.lock()?;
        self.write(session)
    }

    /// The one way to change a saved session. Under the store's lock, read
    /// session `id` as it is saved now, apply `change`, and write it
    /// atomically. Nothing is written unless every step succeeds, so a
    /// refused change, a failed write or an unreadable file leaves the saved
    /// session exactly as it was. No other writer's change is lost, because
    /// each applies to what the last one wrote, and a missing session stays
    /// missing. Returns `change`'s value and the session as saved. With
    /// history off, nothing is written.
    pub fn update<T, E>(
        &self,
        id: &SessionId,
        change: impl FnOnce(&mut Session) -> Result<T, E>,
    ) -> Result<(T, Session), SessionUpdateError<E>> {
        let _lock = if self.keep_history {
            Some(self.lock().map_err(SessionUpdateError::Store)?)
        } else {
            None
        };
        let mut session = self.load(id).map_err(SessionUpdateError::Store)?;
        let value = change(&mut session).map_err(SessionUpdateError::Change)?;
        if self.keep_history {
            self.write(&session).map_err(SessionUpdateError::Store)?;
        }
        Ok((value, session))
    }

    /// For a client holding a session open, such as the terminal chat or an
    /// ACP editor: apply `change` to the session as saved now (see
    /// [`SessionStore::update`]) and make `session` that saved state, so the
    /// client also sees what other writers changed. A session not saved yet,
    /// or deleted meanwhile, is saved from `session` with the change, since
    /// it holds the client's whole conversation. On any other error the store
    /// is unchanged and `session` takes the change alone, as the client's
    /// view.
    pub fn apply<T>(
        &self,
        session: &mut Session,
        change: impl Fn(&mut Session) -> T,
    ) -> Result<T, StoreError> {
        match self.update(&session.id, |saved| Ok::<T, Infallible>(change(saved))) {
            Ok((value, saved)) => {
                *session = saved;
                Ok(value)
            }
            Err(error) => match error.into_store() {
                StoreError::NotFound(_) => {
                    let value = change(session);
                    self.save(session)?;
                    Ok(value)
                }
                error => {
                    change(session);
                    Err(error)
                }
            },
        }
    }

    /// Hold the store's write lock: an exclusive OS lock on [`LOCK_FILE`] in
    /// the store's directory, which every process takes. Waits up to the
    /// store's lock wait. One lock for the whole store rather than one per
    /// session, so a deletion and a later writer are ordered as well, and no
    /// lock file outlives a deleted session.
    fn lock(&self) -> Result<std::fs::File, StoreError> {
        paths::create_private_dir(&self.directory).map_err(|err| StoreError::Directory {
            path: self.directory.clone(),
            reason: err.to_string(),
        })?;
        let path = self.directory.join(LOCK_FILE);
        paths::lock_exclusive(&path, self.lock_wait).map_err(|error| match error {
            paths::LockError::Busy => StoreError::Busy {
                path: self.directory.clone(),
            },
            paths::LockError::Io(err) => StoreError::Directory {
                path: path.clone(),
                reason: err.to_string(),
            },
        })
    }

    /// Take session `id`'s run lease. A turn running on it in any process
    /// that shares this store (the desktop runtime, another server, a
    /// terminal chat, an editor over ACP) refuses this one with
    /// [`StoreError::Busy`] after at most [`RUN_LEASE_GRACE`]; it never
    /// queues behind that turn.
    pub fn lease_run(&self, id: &SessionId) -> Result<RunLease, StoreError> {
        let directory = self.directory.join(RUNS_DIR);
        paths::create_private_dir(&directory).map_err(|err| StoreError::Directory {
            path: directory.clone(),
            reason: err.to_string(),
        })?;
        let path = directory.join(format!("{}.lock", id.as_str()));
        match paths::lock_exclusive(&path, RUN_LEASE_GRACE) {
            Ok(file) => Ok(RunLease { file }),
            Err(paths::LockError::Busy) => Err(StoreError::Busy { path: directory }),
            Err(paths::LockError::Io(err)) => Err(StoreError::Directory {
                path,
                reason: err.to_string(),
            }),
        }
    }

    fn journal_path(&self, id: &SessionId) -> PathBuf {
        self.directory
            .join(RUNS_DIR)
            .join(format!("{}.run", id.as_str()))
    }

    /// Note that session `id` has turn `run_id` in flight, so that a crash
    /// before the turn is recorded is found later
    /// ([`Self::recover_interrupted`]). The caller holds `id`'s run lease.
    pub fn journal_run(
        &self,
        id: &SessionId,
        run_id: &str,
        replacing: bool,
    ) -> Result<(), StoreError> {
        let journal = RunJournal {
            run_id: run_id.to_owned(),
            started_at: SystemTime::now(),
            replacing,
        };
        let unwritable = |reason: String| StoreError::Unwritable {
            id: id.as_str().to_owned(),
            reason,
        };
        let bytes = serde_json::to_vec(&journal).map_err(|err| unwritable(err.to_string()))?;
        paths::write_private(&self.journal_path(id), &bytes)
            .map_err(|err| unwritable(err.to_string()))
    }

    /// Session `id`'s turn was recorded: forget its journal.
    pub fn clear_run_journal(&self, id: &SessionId) {
        let _ = std::fs::remove_file(self.journal_path(id));
    }

    /// Record session `id`'s interrupted turn, if a journal outlived the
    /// process that wrote it. An appended turn gets a run record with stop
    /// reason [`INTERRUPTED`] and no answer; an interrupted regeneration
    /// leaves the saved answer and its record as they were. Then the journal
    /// goes. Nothing is run again. The caller holds `id`'s run lease, so no
    /// live turn owns the journal. Returns whether there was one.
    pub fn recover_interrupted(
        &self,
        id: &SessionId,
        _lease: &RunLease,
    ) -> Result<bool, StoreError> {
        let path = self.journal_path(id);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(StoreError::Unreadable {
                    id: id.as_str().to_owned(),
                    reason: err.to_string(),
                });
            }
        };
        // An unreadable journal still marks an interruption; only its
        // details are lost.
        let journal: Option<RunJournal> = serde_json::from_slice(&bytes).ok();
        if !journal.as_ref().is_some_and(|journal| journal.replacing) {
            let record = RunRecord {
                run_id: journal
                    .as_ref()
                    .map(|journal| journal.run_id.clone())
                    .unwrap_or_default(),
                started_at: journal
                    .as_ref()
                    .map_or_else(SystemTime::now, |journal| journal.started_at),
                ended_at: Some(SystemTime::now()),
                stop_reason: Some(INTERRUPTED.to_owned()),
                tools: Vec::new(),
            };
            let recorded = self.update(id, |session| {
                session.runs.push(record);
                session.touch();
                Ok::<(), Infallible>(())
            });
            match recorded {
                Ok(_) | Err(SessionUpdateError::Store(StoreError::NotFound(_))) => {}
                Err(SessionUpdateError::Store(error)) => return Err(error),
                Err(SessionUpdateError::Change(never)) => match never {},
            }
        }
        let _ = std::fs::remove_file(&path);
        Ok(true)
    }

    /// Record every interrupted turn in this store
    /// ([`Self::recover_interrupted`]). A session whose lease a live process
    /// holds is left alone. Returns the sessions recorded.
    pub fn recover_all_interrupted(&self) -> Vec<SessionId> {
        let Ok(entries) = std::fs::read_dir(self.directory.join(RUNS_DIR)) else {
            return Vec::new();
        };
        let mut recovered = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".run")) else {
                continue;
            };
            let Ok(id) = SessionId::parse(stem) else {
                continue;
            };
            let Ok(lease) = self.lease_run(&id) else {
                continue;
            };
            if self.recover_interrupted(&id, &lease).unwrap_or(false) {
                recovered.push(id);
            }
        }
        recovered
    }

    /// Write `session` atomically and owner-only; the caller holds the lock.
    fn write(&self, session: &Session) -> Result<(), StoreError> {
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

    /// Delete a session, under the store's lock. Returns whether a file was
    /// removed. A writer that comes later finds the session missing and
    /// writes nothing.
    pub fn delete(&self, id: &SessionId) -> Result<bool, StoreError> {
        let _lock = self.lock()?;
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
    fn a_run_lease_admits_one_turn_per_session_and_frees_on_drop() {
        let store = SessionStore::new(scratch_dir());
        let (one, two) = (SessionId::generate(), SessionId::generate());
        let held = store.lease_run(&one).expect("lease");
        assert!(matches!(
            store.lease_run(&one),
            Err(StoreError::Busy { .. })
        ));
        // Another session runs at the same time.
        let other = store.lease_run(&two).expect("another session");
        // The write lock is separate: a save goes through while a turn runs.
        store
            .save(&Session::new("profile", "title"))
            .expect("a save while a turn runs");
        drop(held);
        let again = store.lease_run(&one).expect("freed on drop");
        drop((again, other));
    }

    #[test]
    fn an_interrupted_turn_is_recorded_once_and_never_run_again() {
        let store = SessionStore::new(scratch_dir());
        let mut session = Session::new("profile", "title");
        session.push_message(StoredMessage::new("user", "hello"));
        let id = session.id.clone();
        store.save(&session).expect("save");
        let lease = store.lease_run(&id).expect("lease");
        store.journal_run(&id, "run-1", false).expect("journal");
        // A turn still running in a live process is left alone.
        assert!(store.recover_all_interrupted().is_empty());
        // Its process ends without recording the turn.
        drop(lease);
        assert_eq!(store.recover_all_interrupted(), vec![id.clone()]);
        let saved = store.load(&id).expect("load");
        let run = saved.runs.last().expect("a run record");
        assert_eq!(run.stop_reason.as_deref(), Some(INTERRUPTED));
        assert_eq!(run.run_id, "run-1");
        assert_eq!(saved.messages.len(), 1, "no answer, and nothing run again");
        // Recorded once.
        assert!(store.recover_all_interrupted().is_empty());
        assert_eq!(store.load(&id).expect("load").runs.len(), 1);
    }

    #[test]
    fn an_interrupted_regeneration_keeps_the_saved_answer() {
        use lightagent_core::{AgentEvent, RunId, StopReason};
        let store = SessionStore::new(scratch_dir());
        let mut session = Session::new("profile", "title");
        session.push_message(StoredMessage::new("user", "hello"));
        session.record_run_events(
            &[
                AgentEvent::RunStarted {
                    run: RunId::new(),
                    parent: None,
                },
                AgentEvent::Content {
                    text: "The answer.".into(),
                },
                AgentEvent::RunCompleted {
                    reason: StopReason::EndTurn,
                },
            ],
            "EndTurn",
        );
        let id = session.id.clone();
        store.save(&session).expect("save");
        let before = store.load(&id).expect("load");
        let lease = store.lease_run(&id).expect("lease");
        store.journal_run(&id, "run-2", true).expect("journal");
        drop(lease);
        assert_eq!(store.recover_all_interrupted(), vec![id.clone()]);
        assert_eq!(store.load(&id).expect("load"), before);
        assert!(store.recover_all_interrupted().is_empty());
    }

    #[test]
    fn a_recorded_turn_leaves_no_journal() {
        let store = SessionStore::new(scratch_dir());
        let session = Session::new("profile", "title");
        let id = session.id.clone();
        store.save(&session).expect("save");
        let lease = store.lease_run(&id).expect("lease");
        store.journal_run(&id, "run-3", false).expect("journal");
        store.clear_run_journal(&id);
        assert!(!store.recover_interrupted(&id, &lease).expect("recover"));
        drop(lease);
        assert!(store.recover_all_interrupted().is_empty());
        assert!(store.load(&id).expect("load").runs.is_empty());
    }

    #[test]
    fn a_continued_answer_is_saved_once_with_its_final_stop_reason() {
        use lightagent_core::{AgentEvent, RunId, StopReason};
        let events = vec![
            AgentEvent::RunStarted {
                run: RunId::new(),
                parent: None,
            },
            AgentEvent::Content {
                text: "The bridge opened in".into(),
            },
            AgentEvent::TurnCompleted { usage: None },
            AgentEvent::Content {
                text: " 1932.".into(),
            },
            AgentEvent::TurnCompleted { usage: None },
            AgentEvent::RunCompleted {
                reason: StopReason::EndTurn,
            },
        ];
        let mut session = Session::new("default", "chat");
        session.record_run_events(&events, "EndTurn");
        let answers: Vec<&str> = session
            .messages
            .iter()
            .filter(|message| message.role == "assistant")
            .map(|message| message.content.as_str())
            .collect();
        assert_eq!(answers, ["The bridge opened in 1932."]);
        assert_eq!(session.runs[0].stop_reason.as_deref(), Some("EndTurn"));
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

    fn rename(store: &SessionStore, id: &SessionId, title: &str) -> Session {
        store
            .update(id, |session| {
                session.title = title.to_owned();
                Ok::<(), Infallible>(())
            })
            .unwrap()
            .1
    }

    #[test]
    fn an_update_applies_to_the_session_as_saved_now() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let session = Session::new("default", "chat");
        store.save(&session).unwrap();
        // One writer loaded the session before another renamed it: its
        // change applies to the renamed session, so neither change is lost.
        let stale = store.load(&session.id).unwrap();
        rename(&store, &session.id, "renamed");
        let ((), saved) = store
            .update(&stale.id, |current| {
                current.push_message(StoredMessage::new("assistant", "answer"));
                Ok::<(), Infallible>(())
            })
            .unwrap();
        assert_eq!(saved.title, "renamed");
        assert_eq!(saved.messages.len(), 1);
        assert_eq!(store.load(&session.id).unwrap(), saved);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_change_a_failed_write_or_an_unreadable_file_writes_nothing() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let session = Session::new("default", "chat");
        store.save(&session).unwrap();
        let path = store.path_for(&session.id);
        let before = std::fs::read(&path).unwrap();

        let refused = store.update(&session.id, |current| {
            current.title = "changed".to_owned();
            Err::<(), _>("no")
        });
        assert!(
            matches!(refused, Err(SessionUpdateError::Change("no"))),
            "{refused:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // The atomic write's temporary file cannot be created: the saved
        // session is kept whole.
        let temporary = dir.join(format!("{}.json.tmp", session.id.as_str()));
        std::fs::create_dir(&temporary).unwrap();
        let failed = store.update(&session.id, |current| {
            current.title = "changed".to_owned();
            Ok::<(), Infallible>(())
        });
        assert!(
            matches!(
                failed,
                Err(SessionUpdateError::Store(StoreError::Unwritable { .. }))
            ),
            "{failed:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir(&temporary).unwrap();

        // A damaged file is reported, never replaced by a fresh session.
        std::fs::write(&path, b"{ not json").unwrap();
        let unreadable = store.update(&session.id, |_| Ok::<(), Infallible>(()));
        assert!(
            matches!(
                unreadable,
                Err(SessionUpdateError::Store(StoreError::Unreadable { .. }))
            ),
            "{unreadable:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_waiting_writer_keeps_the_holders_change_and_a_long_hold_is_busy() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let session = Session::new("default", "chat");
        store.save(&session).unwrap();
        let held = store.lock().unwrap();
        let waiting = {
            let store = store.clone();
            let id = session.id.clone();
            std::thread::spawn(move || rename(&store, &id, "renamed"))
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !waiting.is_finished(),
            "the writer did not wait for the lock"
        );
        // The holder writes while the other waits; the waiting writer keeps it.
        let mut answered = store.load(&session.id).unwrap();
        answered.push_message(StoredMessage::new("assistant", "kept"));
        store.write(&answered).unwrap();
        drop(held);
        let saved = waiting.join().unwrap();
        assert_eq!((saved.title.as_str(), saved.messages.len()), ("renamed", 1));

        let impatient = store.clone().with_lock_wait(Duration::from_millis(50));
        let held = store.lock().unwrap();
        let busy = impatient.update(&session.id, |_| Ok::<(), Infallible>(()));
        assert!(
            matches!(
                busy,
                Err(SessionUpdateError::Store(StoreError::Busy { .. }))
            ),
            "{busy:?}"
        );
        assert!(matches!(
            impatient.delete(&session.id),
            Err(StoreError::Busy { .. })
        ));
        drop(held);
        assert_eq!(store.load(&session.id).unwrap(), saved);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_session_is_not_recreated_by_a_later_update() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let session = Session::new("default", "chat");
        store.save(&session).unwrap();
        assert!(store.delete(&session.id).unwrap());
        let late = store.update(&session.id, |current| {
            current.title = "late".to_owned();
            Ok::<(), Infallible>(())
        });
        assert!(
            matches!(
                late,
                Err(SessionUpdateError::Store(StoreError::NotFound(_)))
            ),
            "{late:?}"
        );
        assert!(matches!(
            store.load(&session.id),
            Err(StoreError::NotFound(_))
        ));
        // The lock file is the store's own, never listed as a session.
        assert!(dir.join(LOCK_FILE).is_file());
        assert!(store.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_client_holding_a_session_open_keeps_its_conversation_and_sees_other_writers() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let mut open = Session::new("default", "chat session");
        // Not saved yet: the first change saves the client's session.
        let count = store
            .apply(&mut open, |session| {
                session.push_message(StoredMessage::new("user", "first"));
                session.messages.len()
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(store.load(&open.id).unwrap(), open);
        // Another writer renames it; the client's next change keeps the
        // rename, and the client sees it.
        rename(&store, &open.id, "renamed");
        store
            .apply(&mut open, |session| {
                session.push_message(StoredMessage::new("assistant", "answer"))
            })
            .unwrap();
        assert_eq!(open.title, "renamed");
        assert_eq!(open.messages.len(), 2);
        assert_eq!(store.load(&open.id).unwrap(), open);
        // Deleted meanwhile: the client's conversation is saved again, whole.
        store.delete(&open.id).unwrap();
        store
            .apply(&mut open, |session| {
                session.push_message(StoredMessage::new("user", "again"))
            })
            .unwrap();
        assert_eq!(store.load(&open.id).unwrap().messages.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_history_off_an_update_writes_nothing() {
        let dir = scratch_dir();
        let keeping = SessionStore::new(&dir);
        let session = Session::new("default", "chat");
        keeping.save(&session).unwrap();
        let off = SessionStore::new(&dir).keep_history(false);
        let ((), changed) = off
            .update(&session.id, |current| {
                current.title = "changed".to_owned();
                Ok::<(), Infallible>(())
            })
            .unwrap();
        assert_eq!(changed.title, "changed");
        assert_eq!(keeping.load(&session.id).unwrap(), session);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other process of the cross-process test: this test binary, run
    /// again with `LIGHTAGENT_TEST_SESSION_DIR` and `LIGHTAGENT_TEST_SESSION_ID`
    /// set. It marks that it started, then renames that session through the
    /// store. Run any other way it does nothing.
    #[test]
    #[ignore = "run only as the writer process of the cross-process test"]
    fn session_writer_process() {
        let (Ok(dir), Ok(id)) = (
            std::env::var("LIGHTAGENT_TEST_SESSION_DIR"),
            std::env::var("LIGHTAGENT_TEST_SESSION_ID"),
        ) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let id = SessionId::parse(&id).unwrap();
        std::fs::write(dir.join("writer.started"), b"").unwrap();
        rename(&SessionStore::new(&dir), &id, "renamed elsewhere");
    }

    #[test]
    fn a_writer_in_another_process_waits_for_the_lock_and_keeps_the_holders_change() {
        let dir = scratch_dir();
        let store = SessionStore::new(&dir);
        let session = Session::new("default", "chat");
        store.save(&session).unwrap();
        let held = store.lock().unwrap();
        let mut writer = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session::tests::session_writer_process",
                "--ignored",
            ])
            .env("LIGHTAGENT_TEST_SESSION_DIR", &dir)
            .env("LIGHTAGENT_TEST_SESSION_ID", session.id.as_str())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let started = dir.join("writer.started");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !started.exists() {
            assert!(
                writer.try_wait().unwrap().is_none(),
                "the writer process ended before writing"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "the writer process never started"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            writer.try_wait().unwrap().is_none(),
            "the writer wrote while another process held the lock"
        );
        // Another writer's change, made inside the lock: the waiting process
        // must keep it.
        let mut answered = store.load(&session.id).unwrap();
        answered.push_message(StoredMessage::new("assistant", "kept"));
        store.write(&answered).unwrap();
        drop(held);
        assert!(writer.wait().unwrap().success());
        let saved = store.load(&session.id).unwrap();
        assert_eq!(saved.title, "renamed elsewhere");
        assert_eq!(
            saved
                .messages
                .last()
                .map(|message| message.content.as_str()),
            Some("kept")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
