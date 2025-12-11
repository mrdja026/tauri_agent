use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tracing::{info, warn, error, debug};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use crate::{ActionCommand, HistoryEntry};

/// Log entry for the UI panel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub category: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

/// Aggregated stats for history panel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub total_actions: u32,
    pub successful_actions: u32,
    pub failed_actions: u32,
    pub success_rate: f32,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub current_streak: i32,  // positive = success streak, negative = fail streak
    pub longest_success_streak: u32,
    pub most_used_action: Option<String>,
    pub most_failed_action: Option<String>,
    pub avg_tokens_per_action: f32,
}

/// Chain of recent successful actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionChain {
    pub chain_type: String,  // "success_streak", "recent", "failed"
    pub actions: Vec<ChainAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainAction {
    pub step: u32,
    pub action_type: String,
    pub target_summary: String,
    pub reasoning_summary: String,
}

/// Full history analysis for UI panel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryAnalysis {
    pub stats: SessionStats,
    pub current_success_chain: ActionChain,
    pub recent_failures: ActionChain,
    pub action_frequency: Vec<(String, u32)>,
    pub logs: Vec<LogEntry>,
}

lazy_static::lazy_static! {
    static ref LOG_BUFFER: Mutex<Vec<LogEntry>> = Mutex::new(Vec::new());
}

/// Initialize logging system
pub fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).with_level(true))
        .with(filter)
        .init();

    info!("Logging system initialized");
}

/// Add a log entry to the buffer
pub fn log_action(level: &str, category: &str, message: &str, details: Option<serde_json::Value>) {
    let entry = LogEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        level: level.to_string(),
        category: category.to_string(),
        message: message.to_string(),
        details,
    };

    // Also emit via tracing
    match level {
        "ERROR" => error!(category = category, "{}", message),
        "WARN" => warn!(category = category, "{}", message),
        "DEBUG" => debug!(category = category, "{}", message),
        _ => info!(category = category, "{}", message),
    }

    if let Ok(mut buffer) = LOG_BUFFER.lock() {
        buffer.push(entry);
        // Keep last 100 logs
        if buffer.len() > 100 {
            buffer.remove(0);
        }
    }
}

/// Log action execution
pub fn log_action_start(action: &ActionCommand, step: u32, mode: &str) {
    let target_str = action.target.as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| action.target.to_string());

    log_action(
        "INFO",
        "ACTION",
        &format!("Step {}: Executing {} -> {}", step, action.action_type, truncate(&target_str, 50)),
        Some(serde_json::json!({
            "step": step,
            "action_type": action.action_type,
            "target": action.target,
            "mode": mode,
            "reasoning": action.reasoning
        }))
    );
}

/// Log action result
pub fn log_action_result(action: &ActionCommand, step: u32, success: bool, error: Option<&str>) {
    let level = if success { "INFO" } else { "WARN" };
    let status = if success { "SUCCESS" } else { "FAILED" };

    log_action(
        level,
        "RESULT",
        &format!("Step {}: {} - {}", step, action.action_type, status),
        Some(serde_json::json!({
            "step": step,
            "action_type": action.action_type,
            "success": success,
            "error": error
        }))
    );
}

/// Log LLM API call with detailed token info
pub fn log_llm_call(input_tokens: u32, output_tokens: u32, action_type: &str, elements_count: usize, prompt_chars: usize) {
    // Estimate cost (Claude Sonnet pricing: $3/1M input, $15/1M output)
    let input_cost = (input_tokens as f64 / 1_000_000.0) * 3.0;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * 15.0;
    let total_cost = input_cost + output_cost;

    log_action(
        "INFO",  // Changed to INFO so it shows in yellow
        "LLM",
        &format!(
            "API: {}+{} tokens ({} elements, {} chars) -> {} [${:.6}]",
            input_tokens, output_tokens, elements_count, prompt_chars, action_type, total_cost
        ),
        Some(serde_json::json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "elements_count": elements_count,
            "prompt_chars": prompt_chars,
            "action_type": action_type,
            "estimated_cost_usd": total_cost
        }))
    );
}

/// Log performance timing
pub fn log_performance(operation: &str, duration_ms: u64) {
    log_action(
        "INFO",
        "PERF",
        &format!("{}: {}ms", operation, duration_ms),
        Some(serde_json::json!({
            "operation": operation,
            "duration_ms": duration_ms
        }))
    );
}

/// Log mode switch
pub fn log_mode_switch(from: &str, to: &str) {
    log_action(
        "INFO",
        "MODE",
        &format!("Switching mode: {} -> {}", from, to),
        Some(serde_json::json!({
            "from": from,
            "to": to
        }))
    );
}

/// Analyze history and return structured data for UI
pub fn analyze_history(history: &[HistoryEntry]) -> HistoryAnalysis {
    let mut stats = SessionStats {
        total_actions: history.len() as u32,
        successful_actions: 0,
        failed_actions: 0,
        success_rate: 0.0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        current_streak: 0,
        longest_success_streak: 0,
        most_used_action: None,
        most_failed_action: None,
        avg_tokens_per_action: 0.0,
    };

    let mut action_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut fail_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut current_streak: i32 = 0;
    let mut longest_success = 0u32;
    let mut temp_streak = 0u32;

    for h in history {
        // Count successes/failures
        if h.success {
            stats.successful_actions += 1;
            if current_streak >= 0 {
                current_streak += 1;
                temp_streak += 1;
                longest_success = longest_success.max(temp_streak);
            } else {
                current_streak = 1;
                temp_streak = 1;
            }
        } else {
            stats.failed_actions += 1;
            if current_streak <= 0 {
                current_streak -= 1;
            } else {
                current_streak = -1;
            }
            temp_streak = 0;
            *fail_counts.entry(h.action.action_type.clone()).or_insert(0) += 1;
        }

        // Count action types
        *action_counts.entry(h.action.action_type.clone()).or_insert(0) += 1;

        // Sum tokens
        stats.total_input_tokens += h.input_tokens.unwrap_or(0);
        stats.total_output_tokens += h.output_tokens.unwrap_or(0);
    }

    stats.current_streak = current_streak;
    stats.longest_success_streak = longest_success;

    if stats.total_actions > 0 {
        stats.success_rate = (stats.successful_actions as f32 / stats.total_actions as f32) * 100.0;
        stats.avg_tokens_per_action = (stats.total_input_tokens + stats.total_output_tokens) as f32 / stats.total_actions as f32;
    }

    stats.most_used_action = action_counts.iter()
        .max_by_key(|(_, v)| *v)
        .map(|(k, _)| k.clone());

    stats.most_failed_action = fail_counts.iter()
        .max_by_key(|(_, v)| *v)
        .map(|(k, _)| k.clone());

    // Build success chain (last consecutive successes)
    let mut success_chain_actions = Vec::new();
    for h in history.iter().rev() {
        if h.success {
            success_chain_actions.push(history_to_chain_action(h));
            if success_chain_actions.len() >= 10 {
                break;
            }
        } else {
            break;
        }
    }
    success_chain_actions.reverse();

    // Build recent failures
    let recent_failures: Vec<ChainAction> = history.iter()
        .rev()
        .filter(|h| !h.success)
        .take(5)
        .map(history_to_chain_action)
        .collect();

    // Action frequency
    let mut action_frequency: Vec<(String, u32)> = action_counts.into_iter().collect();
    action_frequency.sort_by(|a, b| b.1.cmp(&a.1));

    // Get logs
    let logs = LOG_BUFFER.lock()
        .map(|b| b.clone())
        .unwrap_or_default();

    HistoryAnalysis {
        stats,
        current_success_chain: ActionChain {
            chain_type: "success_streak".to_string(),
            actions: success_chain_actions,
        },
        recent_failures: ActionChain {
            chain_type: "failed".to_string(),
            actions: recent_failures,
        },
        action_frequency,
        logs,
    }
}

fn history_to_chain_action(h: &HistoryEntry) -> ChainAction {
    let target_str = h.action.target.as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| h.action.target.to_string());

    ChainAction {
        step: h.step_number,
        action_type: h.action.action_type.clone(),
        target_summary: truncate(&target_str, 30),
        reasoning_summary: truncate(&h.llm_reasoning, 50),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Clear log buffer
pub fn clear_logs() {
    if let Ok(mut buffer) = LOG_BUFFER.lock() {
        buffer.clear();
    }
}
