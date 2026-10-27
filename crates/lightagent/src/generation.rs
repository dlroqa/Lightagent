//! A run's output and context budget, from the operator's `generation`
//! settings and what the provider reports about the model it serves.

use lightagent_core::Config;
use lightagent_provider_lightweight::LightweightProvider;
use lightagent_tools::LeadGeneration;

/// The budget for a run on `provider`: the settings for the model the
/// provider resolves, with the window it reports serving that model with.
/// Without a reported window only a configured one is budgeted against; with
/// neither, the run's requests are what they were before budgets existed,
/// apart from any allowance or reasoning label the operator set.
pub(crate) async fn for_run(config: &Config, provider: &LightweightProvider) -> LeadGeneration {
    let model = provider.resolve_model().await.unwrap_or_default();
    let reported = if model.is_empty() {
        None
    } else {
        provider.context_window(&model).await
    };
    let budget = config.generation.budget_for(&model, reported);
    LeadGeneration { model, budget }
}
