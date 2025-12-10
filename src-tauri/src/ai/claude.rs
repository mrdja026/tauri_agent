use reqwest::Client;
use serde::{Deserialize, Serialize};
use crate::{ActionCommand, ExecutionState, HistoryEntry};

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-sonnet-4-20250514";

#[derive(Serialize)]
struct ClaudeRequest { model: String, max_tokens: u32, system: String, messages: Vec<Message> }
#[derive(Serialize, Deserialize)]
struct Message { role: String, content: String }

pub async fn get_next_action(api_key: &str, cmd: &str, state: &ExecutionState, history: &[HistoryEntry]) -> Result<ActionCommand, Box<dyn std::error::Error + Send + Sync>> {
    let req = ClaudeRequest { model: MODEL.to_string(), max_tokens: 1000, system: system_prompt(), messages: vec![Message { role: "user".to_string(), content: user_msg(cmd, state, history) }] };
    let res = Client::new().post(CLAUDE_API_URL).header("x-api-key", api_key).header("anthropic-version", "2023-06-01").header("content-type", "application/json").json(&req).send().await?;
    parse_response(&res.json().await?)
}

pub async fn get_retry_action(api_key: &str, failed: &ActionCommand, error: &str, state: &ExecutionState, history: &[HistoryEntry]) -> Result<ActionCommand, Box<dyn std::error::Error + Send + Sync>> {
    let req = ClaudeRequest { model: MODEL.to_string(), max_tokens: 1000, system: system_prompt(), messages: vec![Message { role: "user".to_string(), content: retry_msg(failed, error, state, history) }] };
    let res = Client::new().post(CLAUDE_API_URL).header("x-api-key", api_key).header("anthropic-version", "2023-06-01").header("content-type", "application/json").json(&req).send().await?;
    parse_response(&res.json().await?)
}

fn system_prompt() -> String {
    r#"You are a PC automation assistant. RULES: 1) ONE action at a time 2) Use element IDs from accessibility tree 3) For browsers use CSS selectors or ax: prefix 4) For desktop use uia_ prefix 5) Learn from failures
ACTIONS: click, type (params.text), navigate (params.url), scroll (params.direction/amount), press_key (params.key), focus_window
OUTPUT JSON ONLY: {"action_type":"...","target":"...","params":{...},"reasoning":"..."}"#.to_string()
}

fn user_msg(cmd: &str, state: &ExecutionState, history: &[HistoryEntry]) -> String {
    let h = history.iter().map(|h| format!("- {}: {} ({})", h.action.action_type, h.action.reasoning.as_deref().unwrap_or(""), if h.success {"ok"} else {"fail"})).collect::<Vec<_>>().join("\n");
    format!("GOAL: {}\n\nSTATE:\n- Window: {}\n- URL: {}\n- A11y Tree:\n{}\n\nHISTORY:\n{}\n\nNext action? JSON only.", cmd, state.active_window, state.url.as_deref().unwrap_or("N/A"), serde_json::to_string_pretty(&state.accessibility_tree).unwrap_or_default(), if h.is_empty() {"(none)".to_string()} else {h})
}

fn retry_msg(action: &ActionCommand, error: &str, state: &ExecutionState, _: &[HistoryEntry]) -> String {
    format!("FAILED: {} on {:?}\nError: {}\n\nCURRENT STATE:\n- Window: {}\n- URL: {}\n- A11y:\n{}\n\nSuggest alternative. JSON only.", action.action_type, action.target, error, state.active_window, state.url.as_deref().unwrap_or("N/A"), serde_json::to_string_pretty(&state.accessibility_tree).unwrap_or_default())
}

fn parse_response(res: &serde_json::Value) -> Result<ActionCommand, Box<dyn std::error::Error + Send + Sync>> {
    let t = res["content"][0]["text"].as_str().ok_or("No text")?;
    let j = t.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    Ok(serde_json::from_str(j)?)
}
