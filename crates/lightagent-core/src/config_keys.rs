//! Config keys by dotted name: the reading and parsing behind
//! `lightagent config get|set`, shared by the CLI and the HTTP API so both
//! accept and refuse exactly the same keys and values. Secret keys read as
//! their redacted reference, never as a value.

use crate::config::{Config, SecretRef};

/// Read a small, fixed set of dotted keys.
pub fn get_key(config: &Config, key: &str) -> Option<String> {
    match key {
        "inference.provider" => Some(config.inference.provider.clone()),
        "inference.base_url" => Some(config.inference.base_url.clone()),
        "inference.model" => Some(config.inference.model.clone().unwrap_or_default()),
        "inference.api_key" => Some(
            config
                .inference
                .api_key
                .as_ref()
                .map(SecretRef::redacted)
                .unwrap_or_default(),
        ),
        "inference.device" => Some(config.inference.device.clone()),
        key if key.starts_with("inference.model_catalog.") => config
            .inference
            .model_catalog
            .get(&key["inference.model_catalog.".len()..])
            .cloned(),
        key if key.starts_with("inference.model_alias.") => config
            .inference
            .model_aliases
            .get(&key["inference.model_alias.".len()..])
            .cloned(),
        "agent.max_turns" => Some(config.agent.max_turns.to_string()),
        "agent.max_tool_calls" => Some(config.agent.max_tool_calls.to_string()),
        // Empty means no time limit.
        "agent.wall_clock_secs" => Some(
            config
                .agent
                .wall_clock_secs
                .map(|secs| secs.to_string())
                .unwrap_or_default(),
        ),
        "extensions.enabled" => Some(config.extensions.enabled.to_string()),
        "memory.auto_capture" => Some(config.memory.auto_capture.to_string()),
        "memory.inject_recent" => Some(config.memory.inject_recent.to_string()),
        "memory.top_k" => Some(config.memory.top_k.to_string()),
        "web.enabled" => Some(config.web.enabled.to_string()),
        "web.search.endpoint" => Some(config.web.search.endpoint.clone().unwrap_or_default()),
        "web.search.query_param" => Some(config.web.search.query_param.clone()),
        "web.search.max_results" => Some(config.web.search.max_results.to_string()),
        "web.max_fetch_bytes" => Some(config.web.max_fetch_bytes.to_string()),
        "web.timeout_secs" => Some(config.web.timeout_secs.to_string()),
        "rag.realtime_enabled" => Some(config.rag.realtime_enabled.to_string()),
        "tools.profile_policy" => Some(config.tools.profile_policy.as_str().to_owned()),
        "platform.jev.enabled" => Some(config.platform.jev.endpoint.enabled.to_string()),
        "platform.jev.base_url" => Some(
            config
                .platform
                .jev
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.jev.model" => Some(config.platform.jev.model.clone()),
        "platform.jev.confidence_threshold" => {
            Some(config.platform.jev.confidence_threshold.to_string())
        }
        "platform.jev.allowed_models" => Some(config.platform.jev.allowed_models.join(",")),
        "platform.jev.allowed_profiles" => Some(config.platform.jev.allowed_profiles.join(",")),
        "platform.jev.timeout_secs" => Some(config.platform.jev.timeout_secs.to_string()),
        "platform.jev.routing_input" => Some(config.platform.jev.routing_input.as_str().to_owned()),
        "platform.jev.routing_permission" => {
            Some(config.platform.jev.routing_permission.as_str().to_owned())
        }
        "platform.jev.routing_allowed_profiles" => {
            Some(config.platform.jev.routing_allowed_profiles.join(","))
        }
        "platform.jev.classification.workflow_guidance" => Some(
            config
                .platform
                .jev
                .classification
                .workflow_guidance
                .to_string(),
        ),
        "platform.jev.classification.mode" => {
            Some(config.platform.jev.classification.mode.as_str().to_owned())
        }
        "subagents.selection" => Some(config.subagents.selection.as_str().to_owned()),
        "inference.tool_names" => Some(config.inference.tool_names.as_str().to_owned()),
        "subagents.selection_profiles" => Some(config.subagents.selection_profiles.join(",")),
        "platform.jev.classification.allowed_profiles" => Some(
            config
                .platform
                .jev
                .classification
                .allowed_profiles
                .join(","),
        ),
        "platform.jev.classification.max_input_chars" => Some(
            config
                .platform
                .jev
                .classification
                .max_input_chars
                .to_string(),
        ),
        "platform.jev.classification.default_threshold" => Some(
            config
                .platform
                .jev
                .classification
                .default_threshold
                .to_string(),
        ),
        "platform.jev.classification.version_thresholds" => Some(
            config
                .platform
                .jev
                .classification
                .version_thresholds
                .iter()
                .map(|(version, threshold)| format!("{version}={threshold}"))
                .collect::<Vec<_>>()
                .join(","),
        ),
        "platform.qdrant.enabled" => Some(config.platform.qdrant.endpoint.enabled.to_string()),
        "platform.qdrant.base_url" => Some(
            config
                .platform
                .qdrant
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.qdrant.collection" => Some(config.platform.qdrant.collection.clone()),
        "platform.qdrant.timeout_secs" => Some(config.platform.qdrant.timeout_secs.to_string()),
        "platform.infinity.enabled" => Some(config.platform.infinity.endpoint.enabled.to_string()),
        "platform.infinity.base_url" => Some(
            config
                .platform
                .infinity
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.infinity.embedding_model" => {
            Some(config.platform.infinity.embedding_model.clone())
        }
        "platform.infinity.rerank_model" => Some(config.platform.infinity.rerank_model.clone()),
        "platform.infinity.timeout_secs" => Some(config.platform.infinity.timeout_secs.to_string()),
        "platform.open_terminal.enabled" => {
            Some(config.platform.open_terminal.endpoint.enabled.to_string())
        }
        "platform.open_terminal.base_url" => Some(
            config
                .platform
                .open_terminal
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.open_terminal.request_timeout_secs" => Some(
            config
                .platform
                .open_terminal
                .request_timeout_secs
                .to_string(),
        ),
        "platform.open_terminal.execution_timeout_secs" => Some(
            config
                .platform
                .open_terminal
                .execution_timeout_secs
                .to_string(),
        ),
        "platform.open_terminal.poll_interval_ms" => {
            Some(config.platform.open_terminal.poll_interval_ms.to_string())
        }
        "platform.open_terminal.max_output_bytes" => {
            Some(config.platform.open_terminal.max_output_bytes.to_string())
        }
        "tui.show_reasoning" => Some(config.tui.show_reasoning.to_string()),
        "runtime.preferred_device" => Some(config.runtime.preferred_device.clone()),
        "runtime.allow_cpu_fallback" => Some(config.runtime.allow_cpu_fallback.to_string()),
        "runtime.n_ctx" => config.runtime.n_ctx.map(|v| v.to_string()),
        "runtime.threads" => config.runtime.threads.map(|v| v.to_string()),
        "runtime.kv_type" => config.runtime.kv_type.clone(),
        "runtime.load_mode" => config.runtime.load_mode.clone(),
        "runtime.ubatch" => config.runtime.ubatch.map(|v| v.to_string()),
        "generation.max_output_tokens" => {
            config.generation.max_output_tokens.map(|v| v.to_string())
        }
        "generation.reserve_answer_tokens" => config
            .generation
            .reserve_answer_tokens
            .map(|v| v.to_string()),
        "generation.context_window" => config.generation.context_window.map(|v| v.to_string()),
        "generation.reasoning_effort" => config.generation.reasoning_effort.clone(),
        "generation.unknown_capacity_turn_tokens" => config
            .generation
            .unknown_capacity_turn_tokens
            .map(|v| v.to_string()),
        "generation.max_run_output_tokens" => config
            .generation
            .max_run_output_tokens
            .map(|v| v.to_string()),
        "generation.continuations" => config.generation.continuations.map(|v| v.to_string()),
        "generation.adaptive_allocation" => {
            Some(config.generation.adaptive_allocation.as_str().to_owned())
        }
        _ => None,
    }
}

/// Write a small, fixed set of dotted keys.
pub fn set_key(config: &mut Config, key: &str, value: &str) -> Result<(), String> {
    match key {
        "inference.base_url" => config.inference.base_url = value.to_string(),
        "inference.model" => config.inference.model = Some(value.to_string()),
        // The value is an environment-variable name, never the secret itself.
        // An empty value clears the reference.
        "inference.api_key" => {
            config.inference.api_key = parse_opt_string(value).map(SecretRef::env);
        }
        "inference.device" => config.inference.device = value.to_string(),
        key if key.starts_with("inference.model_catalog.") => {
            let model_id = key["inference.model_catalog.".len()..].trim();
            if model_id.is_empty() {
                return Err("model catalog entry needs a model id".to_owned());
            }
            let label = value.trim();
            if label.is_empty() {
                config.inference.model_catalog.remove(model_id);
            } else {
                config
                    .inference
                    .model_catalog
                    .insert(model_id.to_owned(), label.to_owned());
            }
        }
        key if key.starts_with("inference.model_alias.") => {
            let model_id = key["inference.model_alias.".len()..].trim();
            if model_id.is_empty() {
                return Err("model alias needs a model id".to_owned());
            }
            let label = value.trim();
            if label.is_empty() {
                config.inference.model_aliases.remove(model_id);
            } else {
                config
                    .inference
                    .model_aliases
                    .insert(model_id.to_owned(), label.to_owned());
            }
        }
        "agent.max_turns" => {
            config.agent.max_turns = parse_u32(value, "agent.max_turns")?;
        }
        "agent.max_tool_calls" => {
            config.agent.max_tool_calls = parse_u32(value, "agent.max_tool_calls")?;
        }
        // An empty value (or `none`) removes the time limit.
        "agent.wall_clock_secs" => {
            config.agent.wall_clock_secs = match value.trim() {
                "" | "none" => None,
                _ => Some(parse_u64(value, "agent.wall_clock_secs")?),
            };
        }
        "extensions.enabled" => {
            config.extensions.enabled = parse_bool(value)?;
        }
        "memory.auto_capture" => config.memory.auto_capture = parse_bool(value)?,
        "memory.inject_recent" => {
            config.memory.inject_recent = parse_usize(value, "memory.inject_recent")?;
        }
        "memory.top_k" => {
            let top_k = parse_usize(value, "memory.top_k")?;
            if top_k == 0 {
                return Err("memory.top_k must be at least 1".to_owned());
            }
            config.memory.top_k = top_k;
        }
        "web.enabled" => config.web.enabled = parse_bool(value)?,
        "web.search.endpoint" => config.web.search.endpoint = parse_opt_string(value),
        "web.search.query_param" => config.web.search.query_param = value.trim().to_owned(),
        "web.search.max_results" => {
            config.web.search.max_results = parse_usize(value, "web.search.max_results")?;
        }
        "web.max_fetch_bytes" => {
            config.web.max_fetch_bytes = parse_usize(value, "web.max_fetch_bytes")?;
        }
        "web.timeout_secs" => {
            config.web.timeout_secs = parse_u64(value, "web.timeout_secs")?;
        }
        "rag.realtime_enabled" => config.rag.realtime_enabled = parse_bool(value)?,
        "tools.profile_policy" => config.tools.profile_policy = value.parse()?,
        "platform.jev.enabled" => config.platform.jev.endpoint.enabled = parse_bool(value)?,
        "platform.jev.base_url" => config.platform.jev.endpoint.base_url = parse_opt_string(value),
        "platform.jev.model" => config.platform.jev.model = value.trim().to_owned(),
        "platform.jev.confidence_threshold" => {
            config.platform.jev.confidence_threshold = value
                .trim()
                .parse()
                .map_err(|_| "platform.jev.confidence_threshold must be a number".to_owned())?;
        }
        "platform.jev.allowed_models" => {
            config.platform.jev.allowed_models = value
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.timeout_secs" => {
            config.platform.jev.timeout_secs = parse_u64(value, "platform.jev.timeout_secs")?;
        }
        "platform.jev.routing_input" => config.platform.jev.routing_input = value.parse()?,
        "platform.jev.routing_permission" => {
            config.platform.jev.routing_permission = value.parse()?;
        }
        "platform.jev.routing_allowed_profiles" => {
            config.platform.jev.routing_allowed_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.classification.workflow_guidance" => {
            config.platform.jev.classification.workflow_guidance = parse_bool(value)?;
        }
        "platform.jev.classification.mode" => {
            config.platform.jev.classification.mode = value.parse()?;
        }
        "subagents.selection" => {
            config.subagents.selection = value.parse()?;
        }
        "inference.tool_names" => {
            config.inference.tool_names = value.parse()?;
        }
        "subagents.selection_profiles" => {
            config.subagents.selection_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.classification.allowed_profiles" => {
            config.platform.jev.classification.allowed_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.classification.max_input_chars" => {
            config.platform.jev.classification.max_input_chars = parse_usize(value, key)?;
        }
        "platform.jev.classification.default_threshold" => {
            config.platform.jev.classification.default_threshold = value
                .trim()
                .parse()
                .map_err(|_| format!("{key} must be a number"))?;
        }
        "platform.jev.classification.version_thresholds" => {
            config.platform.jev.classification.version_thresholds =
                parse_version_thresholds(value)?;
        }
        "platform.jev.allowed_profiles" => {
            config.platform.jev.allowed_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.qdrant.enabled" => config.platform.qdrant.endpoint.enabled = parse_bool(value)?,
        "platform.qdrant.base_url" => {
            config.platform.qdrant.endpoint.base_url = parse_opt_string(value)
        }
        "platform.qdrant.collection" => config.platform.qdrant.collection = value.trim().to_owned(),
        "platform.qdrant.timeout_secs" => {
            config.platform.qdrant.timeout_secs = parse_u64(value, key)?
        }
        "platform.infinity.enabled" => {
            config.platform.infinity.endpoint.enabled = parse_bool(value)?
        }
        "platform.infinity.base_url" => {
            config.platform.infinity.endpoint.base_url = parse_opt_string(value)
        }
        "platform.infinity.embedding_model" => {
            config.platform.infinity.embedding_model = value.trim().to_owned()
        }
        "platform.infinity.rerank_model" => {
            config.platform.infinity.rerank_model = value.trim().to_owned()
        }
        "platform.infinity.timeout_secs" => {
            config.platform.infinity.timeout_secs = parse_u64(value, key)?
        }
        "platform.open_terminal.enabled" => {
            config.platform.open_terminal.endpoint.enabled = parse_bool(value)?
        }
        "platform.open_terminal.base_url" => {
            config.platform.open_terminal.endpoint.base_url = parse_opt_string(value)
        }
        "platform.open_terminal.request_timeout_secs" => {
            config.platform.open_terminal.request_timeout_secs = parse_u64(value, key)?
        }
        "platform.open_terminal.execution_timeout_secs" => {
            config.platform.open_terminal.execution_timeout_secs = parse_u64(value, key)?
        }
        "platform.open_terminal.poll_interval_ms" => {
            config.platform.open_terminal.poll_interval_ms = parse_u64(value, key)?
        }
        "platform.open_terminal.max_output_bytes" => {
            config.platform.open_terminal.max_output_bytes = value
                .parse()
                .map_err(|_| format!("{key} must be a nonnegative integer"))?
        }
        "tui.show_reasoning" => config.tui.show_reasoning = parse_bool(value)?,
        "runtime.preferred_device" => config.runtime.preferred_device = value.to_string(),
        "runtime.allow_cpu_fallback" => {
            config.runtime.allow_cpu_fallback = parse_bool(value)?;
        }
        "runtime.n_ctx" => config.runtime.n_ctx = parse_opt_u32(value)?,
        "runtime.threads" => config.runtime.threads = parse_opt_u32(value)?,
        "runtime.ubatch" => config.runtime.ubatch = parse_opt_u32(value)?,
        "runtime.kv_type" => config.runtime.kv_type = parse_opt_string(value),
        "runtime.load_mode" => config.runtime.load_mode = parse_opt_string(value),
        "generation.max_output_tokens" => {
            config.generation.max_output_tokens = parse_opt_u32(value)?
        }
        "generation.reserve_answer_tokens" => {
            config.generation.reserve_answer_tokens = parse_opt_u32(value)?
        }
        "generation.context_window" => config.generation.context_window = parse_opt_u32(value)?,
        "generation.reasoning_effort" => {
            config.generation.reasoning_effort = parse_opt_string(value)
        }
        "generation.max_run_output_tokens" => {
            config.generation.max_run_output_tokens = parse_opt_u32(value)?
        }
        "generation.unknown_capacity_turn_tokens" => {
            config.generation.unknown_capacity_turn_tokens = parse_opt_u32(value)?
        }
        "generation.continuations" => config.generation.continuations = parse_opt_u32(value)?,
        "generation.adaptive_allocation" => {
            config.generation.adaptive_allocation = value.parse()?;
        }
        _ => return Err(format!("unknown or read-only config key '{key}'")),
    }
    Ok(())
}

/// Parse an optional string key: an empty value clears it.
/// `jev-1.13.0=0.8,jev-1.14.0=0.85`; an empty value clears every entry.
fn parse_version_thresholds(
    value: &str,
) -> Result<std::collections::BTreeMap<String, f32>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (version, threshold) = entry
                .split_once('=')
                .ok_or_else(|| format!("{entry:?} must be written version=threshold"))?;
            let threshold = threshold
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("{entry:?} needs a numeric threshold"))?;
            Ok((version.trim().to_owned(), threshold))
        })
        .collect()
}

fn parse_opt_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse an optional `u32` key: an empty value clears it.
fn parse_opt_u32(value: &str) -> Result<Option<u32>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<u32>()
        .map(Some)
        .map_err(|_| format!("expected a non-negative integer, got '{value}'"))
}

fn parse_u32(value: &str, key: &str) -> Result<u32, String> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("{key} expects a non-negative integer, got '{value}'"))
}

fn parse_usize(value: &str, key: &str) -> Result<usize, String> {
    value
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{key} expects a non-negative integer, got '{value}'"))
}

fn parse_u64(value: &str, key: &str) -> Result<u64, String> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("{key} expects a non-negative integer, got '{value}'"))
}

/// Parse a boolean config value, accepting the usual spellings.
fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!("expected a boolean, got '{value}'")),
    }
}

/// `lightagent config jev-routing-report`, shared by the CLI and the HTTP
/// API: the routing-permission dry run. For every profile on disk (and the
/// built-in `default`), whether it may route now and under `strict`. Profile
/// ids and fixed vocabulary only; no message, session or key is read, and
/// nothing is changed.
pub fn jev_routing_report(config: &Config, profiles: &[crate::ProfileId]) -> serde_json::Value {
    let jev = &config.platform.jev;
    let strict = crate::JevConfig {
        routing_permission: crate::JevRoutingPermission::Strict,
        ..jev.clone()
    };
    let mut ids: Vec<String> = profiles.iter().map(|id| id.as_str().to_owned()).collect();
    if !ids.iter().any(|id| id == "default") {
        ids.push("default".to_owned());
    }
    ids.sort();
    ids.dedup();
    let rows: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "profile": id,
                "now": jev.routing_grant(id).as_str(),
                "under_strict": strict.routing_grant(id).as_str(),
            })
        })
        .collect();
    let denied: Vec<&String> = ids
        .iter()
        .filter(|id| !strict.routing_grant(id).permits())
        .collect();
    let unknown: Vec<&String> = jev
        .routing_allowed_profiles
        .iter()
        .filter(|allowed| !ids.contains(allowed))
        .collect();
    let routing_active = jev.endpoint.enabled
        && (jev
            .allowed_models
            .iter()
            .any(|model| !model.trim().is_empty())
            || !jev.allowed_profiles.is_empty());
    serde_json::json!({
        "report": "jev-routing-permission-dry-run",
        "changes_made": false,
        "routing_enabled": routing_active,
        "routing_input": jev.routing_input.as_str(),
        "permission_policy": jev.routing_permission.as_str(),
        "compatibility": jev.routing_permission == crate::JevRoutingPermission::LegacyAllProfiles,
        "profiles": rows,
        "denied_under_strict": denied,
        "allowed_profiles_not_found": unknown,
    })
}
