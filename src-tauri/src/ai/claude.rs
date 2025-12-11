use reqwest::Client;
use serde::{Deserialize, Serialize};
use crate::{ActionCommand, ExecutionState, HistoryEntry};

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-sonnet-4-20250514";

#[derive(Serialize)]
struct ClaudeRequest { model: String, max_tokens: u32, system: String, messages: Vec<Message> }
#[derive(Serialize, Deserialize)]
struct Message { role: String, content: String }

/// Response with action and token usage
#[derive(Debug, Clone)]
pub struct LLMResponse {
    pub action: ActionCommand,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub elements_count: usize,
    pub prompt_chars: usize,
}

pub async fn get_next_action(api_key: &str, cmd: &str, state: &ExecutionState, history: &[HistoryEntry]) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
    // Extract elements for counting
    let interactables = extract_interactables(&state.accessibility_tree);
    let elements_count = interactables.len();

    let user_content = user_msg(cmd, state, history);
    let prompt_chars = user_content.len();

    let req = ClaudeRequest {
        model: MODEL.to_string(),
        max_tokens: 500,  // Reduced - we only need JSON response
        system: system_prompt(),
        messages: vec![Message { role: "user".to_string(), content: user_content }]
    };
    let res: serde_json::Value = Client::new()
        .post(CLAUDE_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await?
        .json()
        .await?;

    parse_response(&res, elements_count, prompt_chars)
}

pub async fn get_retry_action(api_key: &str, failed: &ActionCommand, error: &str, state: &ExecutionState, _history: &[HistoryEntry], chunk_index: usize) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
    let interactables = extract_interactables(&state.accessibility_tree);
    let elements_count = interactables.len();

    let user_content = retry_msg_with_chunk(failed, error, state, chunk_index);
    let prompt_chars = user_content.len();

    let req = ClaudeRequest {
        model: MODEL.to_string(),
        max_tokens: 500,
        system: system_prompt(),
        messages: vec![Message { role: "user".to_string(), content: user_content }]
    };
    let res: serde_json::Value = Client::new()
        .post(CLAUDE_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await?
        .json()
        .await?;

    parse_response(&res, elements_count, prompt_chars)
}

fn system_prompt() -> String {
    r#"You are a PC automation assistant executing multi-step tasks on Windows. And expert in Windows UI and Web And App ui navigation and automation.

WINDOWS ENVIRONMENT ASSUMPTIONS:
- Taskbar is at BOTTOM of screen (y ≈ screen height - 40px, typically y > 1040 for 1080p)
- Pinned apps are in taskbar - look for "pinned" in name (e.g., "Google Chrome pinned")
- Desktop icons accessible via Win+D or clicking empty desktop area
- Start menu opens with Win key or clicking Start button (bottom-left)
- Common apps: Chrome, Edge, Firefox, Notepad, Explorer, Settings
- Screen resolution typically 1920x1080; taskbar icons spaced ~50px apart
- Right-click opens context menus; double-click opens apps/files

FINDING APPS (priority order):
1. Taskbar pinned icons - fastest, look for "pinned" in a11y tree
2. Desktop icons - if visible, double-click to open
3. Start menu search - click Start, type app name, Enter
4. launch_browser/launch actions - direct launch if app not visible

EXECUTION MODEL:
- After each action, you'll see UPDATED state with new window/UI info and history
- CRITICAL: Use "complete" action IMMEDIATELY when goal is achieved:
  * "open [app]" → complete when app window visible
  * "open browser/Chrome" → complete when mode=BROWSER or Chrome in Window title
  * "search for X" → complete IMMEDIATELY after press_key Enter (search submitted!)
  * "go to [url]" → complete when page loaded
  * "type X" → complete after text entered
- SEARCH IS DONE AFTER ENTER: If you did type + press_key Enter, the search is COMPLETE!
  Do NOT continue after pressing Enter on a search - return complete action.
- MODE TRANSITIONS: [MODE: desktop -> browser] = Chrome opened successfully
- DO NOT add extra steps user didn't request
- LEARN FROM HISTORY - don't repeat successful actions
- When unsure, use "complete" action

ACTIONS:
| Action         | Target                                    | Params                    |
|----------------|-------------------------------------------|---------------------------|
| click          | node_id, "name:X", "coords:x,y"           | -                         |
| double_click   | node_id, "name:X", "coords:x,y"           | -                         |
| right_click    | node_id, "name:X", "coords:x,y"           | -                         |
| hover          | node_id, "name:X", "coords:x,y"           | -                         |
| type           | target (or empty for focused)             | text: string              |
| clear          | target                                    | -                         |
| scroll         | -                                         | direction, amount         |
| press_key      | -                                         | key: string               |
| focus_window   | -                                         | -                         |
| launch_browser | -                                         | url (optional)            |
| launch         | app name                                  | app, args[]               |
| run            | command                                   | command                   |
| complete       | -                                         | summary: string           | ← USE THIS when goal achieved!

BROWSER MODE (when Chrome with CDP is active):
| navigate       | -                                         | url: string               |
| select         | CSS selector                              | value: string             |
| go_back/forward/reload | -                                | -                         |

KEYS: Enter, Tab, Escape, Backspace, Delete, Space, ArrowUp/Down/Left/Right, Home, End

TARGETING TIPS:
- "coords:x,y" most reliable for taskbar icons
- "name:X" good for labeled buttons/fields
- node_id can be stale after UI changes - prefer name/coords
- If element not in current a11y chunk, try coords from bounds or scroll

OUTPUT JSON: {"action_type":"...","target":"...","params":{...},"reasoning":"..."}"#.to_string()
}

/// Interactable element with parent context
#[derive(Debug, Clone, serde::Serialize)]
struct InteractableElement {
    node_id: String,
    role: String,
    name: String,
    parent_context: String,  // Parent name/role for context
    coords: Option<String>,  // "x,y" center coords if bounds available
    focusable: bool,
}

/// Extract only interactable elements from tree (buttons, edits, links, etc.)
/// Returns compact list instead of full tree - saves ~95% tokens
fn extract_interactables(tree: &serde_json::Value) -> Vec<InteractableElement> {
    let mut elements = Vec::new();
    let interactable_roles = [
        "Button", "Edit", "ComboBox", "CheckBox", "RadioButton",
        "Link", "MenuItem", "ListItem", "TabItem", "TreeItem",
        "Hyperlink", "SplitButton", "MenuBar", "Menu", "ToolBar"
    ];

    fn walk(
        node: &serde_json::Value,
        parent_ctx: &str,
        elements: &mut Vec<InteractableElement>,
        roles: &[&str]
    ) {
        if let Some(obj) = node.as_object() {
            let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let node_id = obj.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
            let focusable = obj.get("focusable").and_then(|v| v.as_bool()).unwrap_or(false);

            // Calculate coords from bounds if available
            let coords = obj.get("bounds").and_then(|b| {
                let x = b.get("x").and_then(|v| v.as_f64())?;
                let y = b.get("y").and_then(|v| v.as_f64())?;
                let w = b.get("width").and_then(|v| v.as_f64())?;
                let h = b.get("height").and_then(|v| v.as_f64())?;
                Some(format!("{},{}", (x + w/2.0) as i32, (y + h/2.0) as i32))
            });

            // Check if this is an interactable element
            let is_interactable = roles.iter().any(|r| role.contains(r)) || focusable;

            // Only add if it has a name (skip unnamed elements)
            if is_interactable && !name.is_empty() && !node_id.is_empty() {
                elements.push(InteractableElement {
                    node_id: node_id.to_string(),
                    role: role.to_string(),
                    name: truncate_str(name, 50),
                    parent_context: parent_ctx.to_string(),
                    coords,
                    focusable,
                });
            }

            // Build context for children
            let child_ctx = if !name.is_empty() && name.len() < 30 {
                name.to_string()
            } else if !role.is_empty() {
                role.to_string()
            } else {
                parent_ctx.to_string()
            };

            // Recurse into children
            if let Some(children) = obj.get("children").and_then(|c| c.as_array()) {
                for child in children {
                    walk(child, &child_ctx, elements, roles);
                }
            }
        } else if let Some(arr) = node.as_array() {
            for item in arr {
                walk(item, parent_ctx, elements, roles);
            }
        }
    }

    walk(tree, "Desktop", &mut elements, &interactable_roles);

    // Limit to most relevant elements (prioritize taskbar, then by name)
    elements.truncate(100);
    elements
}

/// Format interactables as compact string for LLM
fn format_interactables(elements: &[InteractableElement]) -> String {
    if elements.is_empty() {
        return "(no interactable elements found)".to_string();
    }

    elements.iter().map(|e| {
        let coords_str = e.coords.as_ref()
            .map(|c| format!(" @ coords:{}", c))
            .unwrap_or_default();
        format!(
            "- \"{}\" ({}) in [{}]{} id:{}",
            e.name, e.role, e.parent_context, coords_str, e.node_id
        )
    }).collect::<Vec<_>>().join("\n")
}

/// Tiered history formatting with context engineering
/// - Extracts learnings from failures
/// - Keeps recent failures prominent
/// - Summarizes older successful actions
/// - Caps total tokens
fn format_history(history: &[HistoryEntry]) -> String {
    if history.is_empty() {
        return "(no previous actions)".to_string();
    }

    let mut output = String::new();

    // 1. LEARNINGS - Extract patterns from failures
    let failures: Vec<_> = history.iter().filter(|h| !h.success).collect();
    if !failures.is_empty() {
        output.push_str("[LEARNINGS FROM FAILURES]\n");
        let mut learnings: Vec<String> = Vec::new();

        for f in &failures {
            let target_str = f.action.target.as_str().unwrap_or("");
            let error = f.error.as_deref().unwrap_or("unknown");

            // Extract actionable learnings
            if error.contains("not found") || error.contains("No element") {
                if target_str.starts_with("name:") {
                    learnings.push(format!("- name:{} not found, try coords or different name",
                        target_str.trim_start_matches("name:")));
                } else if !target_str.starts_with("coords:") {
                    learnings.push(format!("- node_id {} stale, use coords from bounds instead", target_str));
                }
            }
            if error.contains("timeout") {
                learnings.push(format!("- {} timed out, element may need scroll or wait", f.action.action_type));
            }
        }

        // Deduplicate learnings
        learnings.sort();
        learnings.dedup();
        for l in learnings.iter().take(5) {
            output.push_str(l);
            output.push('\n');
        }
        output.push('\n');
    }

    // 2. RECENT FAILURES - Last 3 failures with detail
    let recent_failures: Vec<_> = history.iter().rev().filter(|h| !h.success).take(3).collect();
    if !recent_failures.is_empty() {
        output.push_str("[RECENT FAILURES - avoid repeating]\n");
        for f in recent_failures.iter().rev() {
            output.push_str(&format!(
                "Step {}: ✗ {} -> {:?} | {}\n",
                f.step_number,
                f.action.action_type,
                f.action.target,
                f.error.as_deref().unwrap_or("failed")
            ));
        }
        output.push('\n');
    }

    // 3. RECENT ACTIONS - Last 5 steps with full detail
    output.push_str("[RECENT ACTIONS]\n");
    let recent: Vec<_> = history.iter().rev().take(5).collect();
    let mut prev_mode: Option<&str> = None;
    for h in recent.iter().rev() {
        let status = if h.success { "✓" } else { "✗" };
        let target_display = match &h.action.target {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        // Truncate long targets
        let target_short = if target_display.len() > 40 {
            format!("{}...", &target_display[..40])
        } else {
            target_display
        };

        // Detect and highlight mode transitions
        let mode_marker = if let Some(pm) = prev_mode {
            if pm != h.mode.as_str() {
                format!(" *** MODE: {} -> {} ***", pm, h.mode)
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        prev_mode = Some(&h.mode);

        output.push_str(&format!(
            "Step {}: {} {} -> {} [{}]{} | {}\n",
            h.step_number,
            status,
            h.action.action_type,
            target_short,
            h.mode,
            mode_marker,
            truncate_str(&h.llm_reasoning, 60)
        ));
    }

    // 4. SUMMARY - If more than 5 steps, summarize older ones
    if history.len() > 5 {
        let older: Vec<_> = history.iter().take(history.len() - 5).collect();
        let success_count = older.iter().filter(|h| h.success).count();
        let fail_count = older.len() - success_count;

        output.push_str(&format!(
            "\n[OLDER: {} actions ({} succeeded, {} failed)]\n",
            older.len(), success_count, fail_count
        ));
    }

    output
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        // Find nearest char boundary to avoid panic on UTF-8
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

fn user_msg(cmd: &str, state: &ExecutionState, history: &[HistoryEntry]) -> String {
    // Extract only interactable elements - much smaller than full tree
    let interactables = extract_interactables(&state.accessibility_tree);
    let elements_str = format_interactables(&interactables);
    let history_str = format_history(history);

    let step = history.len() + 1;

    format!(
r#"GOAL: {}

STEP: {}

STATE:
- Mode: {}
- Window: {}
- URL: {}

INTERACTABLE ELEMENTS ({} found):
{}

HISTORY:
{}

If goal is achieved, respond with: {{"action_type":"complete","target":"","params":{{"summary":"..."}}}}
Otherwise, next action JSON only."#,
        cmd,
        step,
        if state.url.is_some() { "BROWSER" } else { "DESKTOP" },
        state.active_window,
        state.url.as_deref().unwrap_or("N/A"),
        interactables.len(),
        elements_str,
        history_str
    )
}

pub fn retry_msg_with_chunk(action: &ActionCommand, error: &str, state: &ExecutionState, _chunk_index: usize) -> String {
    // Use interactables for retry too - more targeted
    let interactables = extract_interactables(&state.accessibility_tree);
    let elements_str = format_interactables(&interactables);

    format!(
r#"FAILED: {} on {:?}
ERROR: {}

STATE:
- Window: {}
- URL: {}

INTERACTABLE ELEMENTS ({} found):
{}

Try different approach:
- Use coords:x,y from element listing
- Try different element name
- Use launch_browser for opening browsers

JSON only."#,
        action.action_type, action.target, error,
        state.active_window, state.url.as_deref().unwrap_or("N/A"),
        interactables.len(),
        elements_str
    )
}

fn parse_response(res: &serde_json::Value, elements_count: usize, prompt_chars: usize) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
    // Check for API errors first
    if let Some(err) = res.get("error") {
        let msg = err["message"].as_str().unwrap_or("Unknown API error");
        let err_type = err["type"].as_str().unwrap_or("error");
        return Err(format!("Claude API error ({}): {}", err_type, msg).into());
    }

    // Extract token usage
    let input_tokens = res["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
    let output_tokens = res["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;

    // Try to extract text from response
    let t = res["content"][0]["text"].as_str()
        .ok_or_else(|| format!("No text in response. Full response: {}", serde_json::to_string_pretty(res).unwrap_or_default()))?;

    // Try multiple extraction strategies
    // 1. Look for ```json ... ``` block
    if let Some(start) = t.find("```json") {
        if let Some(end) = t[start+7..].find("```") {
            let json_str = t[start+7..start+7+end].trim();
            if let Ok(action) = serde_json::from_str(json_str) {
                return Ok(LLMResponse { action, input_tokens, output_tokens, elements_count, prompt_chars });
            }
        }
    }

    // 2. Look for ``` ... ``` block (without json tag)
    if let Some(start) = t.find("```") {
        if let Some(end) = t[start+3..].find("```") {
            let json_str = t[start+3..start+3+end].trim();
            if let Ok(action) = serde_json::from_str(json_str) {
                return Ok(LLMResponse { action, input_tokens, output_tokens, elements_count, prompt_chars });
            }
        }
    }

    // 3. Look for first { ... } JSON object in text
    if let Some(start) = t.find('{') {
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
                return Ok(LLMResponse { action, input_tokens, output_tokens, elements_count, prompt_chars });
            }
        }
    }

    // 4. Last resort: try the whole thing trimmed
    let trimmed = t.trim();
    let action = serde_json::from_str(trimmed)
        .map_err(|e| format!("Failed to parse action JSON: {}. Raw text: {}", e, t))?;

    Ok(LLMResponse { action, input_tokens, output_tokens, elements_count, prompt_chars })
}
