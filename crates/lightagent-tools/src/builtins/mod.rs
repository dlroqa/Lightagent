//! The built-in tools.
//!
//! [`DateTimeNow`] is the minimal read-only tool that proves the whole loop;
//! [`AgentDelegate`] starts a bounded worker run under another profile through
//! the same core loop; [`WebFetch`] and [`WebSearch`] reach the network for
//! read-only data under a guard, when web access is enabled; [`FsRead`],
//! [`FsList`], [`FsWrite`] and [`TerminalRun`] work within a confined workspace,
//! when filesystem/terminal access is enabled; [`SkillRead`] loads a skill's
//! instructions on demand; the `actions.*` tools dispatch and read
//! operator-allowlisted GitHub Actions workflows, when a broker is configured.

pub mod actions;
pub mod datetime;
pub mod delegate;
pub mod fs;
pub mod open_terminal;
pub mod research;
pub mod skill;
pub mod terminal;
pub mod web;
pub mod worker_approval;

pub use actions::{ActionsCancel, ActionsDispatch, ActionsLogs, ActionsStatus};
pub use datetime::DateTimeNow;
pub use delegate::AgentDelegate;
pub use fs::{FsList, FsRead, FsWrite};
pub use open_terminal::OpenTerminalRun;
pub use research::AgentResearch;
pub use skill::SkillRead;
pub use terminal::TerminalRun;
pub use worker_approval::{AgentDeclineWorker, AgentResumeWorker};
pub use web::{
    WebFetch, WebSearch, WebSearchHit, fetch_text, fetch_text_authorized, search_results,
};
