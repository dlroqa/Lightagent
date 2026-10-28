//! A run's output and context budget, from the operator's `generation`
//! settings and what the provider reports about the model it serves.
//!
//! Every entry point resolves through [`resolve`]: the terminal before each
//! turn, the API and ACP before a run (through
//! `RunFactory::history_window`) and when the run starts, and a run's workers
//! through the lead's budget. Nothing is cached: each resolution asks the
//! provider again, so a different endpoint, a different model or a reload at
//! another window is seen by the next run and never served from a stale
//! answer.

use lightagent_core::{Config, HistoryWindow};
use lightagent_provider_lightweight::LightweightProvider;
use lightagent_tools::LeadGeneration;

/// What [`resolve`] found: the run's budget, and the window its session
/// history is packed against.
pub(crate) struct Resolved {
    pub lead: LeadGeneration,
    pub history: HistoryWindow,
}

/// The budget for a run on `provider`: the settings for the model the
/// provider resolves, with the window it reports serving that model with.
/// A configured `generation.context_window` can only narrow a reported
/// window. Without a reported window only a configured one is budgeted
/// against, and history falls back to `runtime.n_ctx` (the window a placement
/// asks for, not one verified to be loaded) and then to 4096 tokens.
pub(crate) async fn resolve(config: &Config, provider: &LightweightProvider) -> Resolved {
    let model = provider.resolve_model().await.unwrap_or_default();
    let reported = if model.is_empty() {
        None
    } else {
        provider.context_window(&model).await
    };
    let budget = config.generation.budget_for(&model, reported);
    let history = HistoryWindow::resolve(budget.context_window, config.runtime.n_ctx);
    Resolved {
        lead: LeadGeneration {
            model,
            budget,
            settings: Some(std::sync::Arc::new(config.generation.clone())),
        },
        history,
    }
}
