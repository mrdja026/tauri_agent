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

pub async fn get_retry_action(api_key: &str, failed: &ActionCommand, error: &str, state: &ExecutionState, _history: &[HistoryEntry], chunk_index: usize) -> Result<ActionCommand, Box<dyn std::error::Error + Send + Sync>> {
    let req = ClaudeRequest { model: MODEL.to_string(), max_tokens: 1000, system: system_prompt(), messages: vec![Message { role: "user".to_string(), content: retry_msg_with_chunk(failed, error, state, chunk_index) }] };
    let res = Client::new().post(CLAUDE_API_URL).header("x-api-key", api_key).header("anthropic-version", "2023-06-01").header("content-type", "application/json").json(&req).send().await?;
    parse_response(&res.json().await?)
}

fn system_prompt() -> String {
    r#"You are a PC automation assistant. ONE action at a time.

MODES (auto-detected based on Chrome availability):
- BROWSER MODE: When Chrome with debugging port is running
- DESKTOP MODE: When automating Windows desktop applications

SHARED ACTIONS (both modes):
| Action        | Target Format                                      | Params              |
|---------------|---------------------------------------------------|---------------------|
| click         | CSS|"ax:id"|"xpath:..."|node_id|"name:X"|"coords:x,y" | -                |
| double_click  | CSS|"ax:id"|"xpath:..."|node_id|"name:X"|"coords:x,y" | -                |
| right_click   | CSS|"ax:id"|"xpath:..."|node_id|"name:X"|"coords:x,y" | -                |
| hover         | CSS|"ax:id"|"xpath:..."|node_id|"name:X"|"coords:x,y" | -                |
| type          | target (or empty for focused element)              | text: string        |
| clear         | target                                             | -                   |
| scroll        | -                                                  | direction, amount   |
| press_key     | -                                                  | key: string         |
| focus_window  | -                                                  | -                   |

DESKTOP-ONLY ACTIONS:
| Action         | Target    | Params                              | Description                    |
|----------------|-----------|-------------------------------------|--------------------------------|
| launch_browser | -         | url (optional)                      | Opens Chrome/Edge/Firefox      |
| launch         | app name  | app, args[] (optional)              | Launch app by name or path     |
| run            | command   | command                             | Run like Win+R shell command   |

App names for launch: chrome, edge, firefox, notepad, explorer, cmd, powershell, calc
Or provide full path: "C:\Program Files\App\app.exe"

BROWSER-ONLY ACTIONS:
| Action      | Target          | Params                    |
|-------------|-----------------|---------------------------|
| navigate    | -               | url: string               |
| select      | CSS selector    | value: string             |
| wait        | CSS selector    | timeout: ms               |
| go_back     | -               | -                         |
| go_forward  | -               | -                         |
| reload      | -               | -                         |
| eval_js     | -               | code: JavaScript string   |

TARGETING BY MODE:
Browser: CSS selector (default), "ax:nodeId", "xpath://..."
Desktop: node_id from tree (default), "name:ElementText", "coords:x,y"

KEYS: Enter, Tab, Escape, Backspace, Delete, Space, ArrowUp/Down/Left/Right, Home, End

OUTPUT JSON ONLY: {"action_type":"...","target":"...","params":{...},"reasoning":"..."}"#.to_string()
}

const CHUNK_SIZE: usize = 500;

/// Flatten tree to list of nodes, then extract a chunk
fn get_tree_chunk(tree: &serde_json::Value, chunk_index: usize) -> (serde_json::Value, usize, usize) {
    // Flatten tree to vec of nodes
    let mut nodes = Vec::new();
    fn flatten(node: &serde_json::Value, nodes: &mut Vec<serde_json::Value>) {
        if let Some(obj) = node.as_object() {
            // Create node without children
            let mut flat_node = serde_json::Map::new();
            for (k, v) in obj {
                if k != "children" {
                    flat_node.insert(k.clone(), v.clone());
                }
            }
            nodes.push(serde_json::Value::Object(flat_node));
            // Recurse into children
            if let Some(children) = obj.get("children").and_then(|c| c.as_array()) {
                for child in children {
                    flatten(child, nodes);
                }
            }
        } else if let Some(arr) = node.as_array() {
            for item in arr {
                flatten(item, nodes);
            }
        }
    }
    flatten(tree, &mut nodes);

    let total = nodes.len();
    let start = chunk_index * CHUNK_SIZE;
    let end = (start + CHUNK_SIZE).min(total);

    if start >= total {
        return (serde_json::Value::Array(vec![]), total, 0);
    }

    let chunk: Vec<_> = nodes[start..end].to_vec();
    let chunk_len = chunk.len();
    (serde_json::Value::Array(chunk), total, chunk_len)
}

fn user_msg(cmd: &str, state: &ExecutionState, history: &[HistoryEntry]) -> String {
    let h = history.iter().map(|h| format!("- {}: {} ({})", h.action.action_type, h.action.reasoning.as_deref().unwrap_or(""), if h.success {"ok"} else {"fail"})).collect::<Vec<_>>().join("\n");

    // First request always gets chunk 0
    let (chunk, total, chunk_len) = get_tree_chunk(&state.accessibility_tree, 0);
    let tree_str = serde_json::to_string(&chunk).unwrap_or_default();

    format!("GOAL: {}\n\nSTATE:\n- Window: {}\n- URL: {}\n- A11y Tree (nodes 1-{} of {}):\n{}\n\nHISTORY:\n{}\n\nNext action? JSON only.",
        cmd, state.active_window, state.url.as_deref().unwrap_or("N/A"), chunk_len, total, tree_str,
        if h.is_empty() {"(none)".to_string()} else {h})
}

pub fn retry_msg_with_chunk(action: &ActionCommand, error: &str, state: &ExecutionState, chunk_index: usize) -> String {
    let (chunk, total, chunk_len) = get_tree_chunk(&state.accessibility_tree, chunk_index);
    let tree_str = serde_json::to_string(&chunk).unwrap_or_default();
    let start = chunk_index * CHUNK_SIZE + 1;
    let end = start + chunk_len - 1;

    format!("FAILED: {} on {:?}\nError: {}\n\nCURRENT STATE:\n- Window: {}\n- URL: {}\n- A11y Tree (nodes {}-{} of {}):\n{}\n\nSuggest alternative. JSON only.",
        action.action_type, action.target, error, state.active_window, state.url.as_deref().unwrap_or("N/A"),
        start, end, total, tree_str)
}

fn parse_response(res: &serde_json::Value) -> Result<ActionCommand, Box<dyn std::error::Error + Send + Sync>> {
    // Check for API errors first
    if let Some(err) = res.get("error") {
        let msg = err["message"].as_str().unwrap_or("Unknown API error");
        let err_type = err["type"].as_str().unwrap_or("error");
        return Err(format!("Claude API error ({}): {}", err_type, msg).into());
    }

    // Try to extract text from response
    let t = res["content"][0]["text"].as_str()
        .ok_or_else(|| format!("No text in response. Full response: {}", serde_json::to_string_pretty(res).unwrap_or_default()))?;

    // Try multiple extraction strategies
    // 1. Look for ```json ... ``` block
    if let Some(start) = t.find("```json") {
        if let Some(end) = t[start+7..].find("```") {
            let json_str = t[start+7..start+7+end].trim();
            if let Ok(action) = serde_json::from_str(json_str) {
                return Ok(action);
            }
        }
    }

    // 2. Look for ``` ... ``` block (without json tag)
    if let Some(start) = t.find("```") {
        if let Some(end) = t[start+3..].find("```") {
            let json_str = t[start+3..start+3+end].trim();
            if let Ok(action) = serde_json::from_str(json_str) {
                return Ok(action);
            }
        }
    }

    // 3. Look for first { ... } JSON object in text
    if let Some(start) = t.find('{') {
        // Find matching closing brace
        let mut depth = 0;
        let mut end_idx = start;
        for (i, c) in t[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_idx = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end_idx > start {
            let json_str = &t[start..end_idx];
            if let Ok(action) = serde_json::from_str(json_str) {
                return Ok(action);
            }
        }
    }

    // 4. Last resort: try the whole thing trimmed
    let trimmed = t.trim();
    serde_json::from_str(trimmed)
        .map_err(|e| format!("Failed to parse action JSON: {}. Raw text: {}", e, t).into())
}
