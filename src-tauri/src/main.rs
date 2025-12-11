#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod automation;
mod ai;
mod logging;

use std::sync::Mutex;
use tauri::{Manager, State};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionCommand { 
    pub action_type: String, 
    pub target: serde_json::Value, 
    pub params: Option<serde_json::Value>, 
    pub reasoning: Option<String> 
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionState { 
    pub screenshot_base64: String, 
    pub accessibility_tree: serde_json::Value, 
    pub active_window: String, 
    pub url: Option<String>, 
    pub success: bool, 
    pub error: Option<String> 
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: String,
    pub step_number: u32,
    pub user_input: Option<String>,
    pub llm_reasoning: String,
    pub action: ActionCommand,
    pub success: bool,
    pub error: Option<String>,
    pub mode: String,  // "desktop" or "browser"
    pub window_context: String,  // what window/page was active
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

pub struct AppState {
    pub api_key: Mutex<Option<String>>,
    pub history: Mutex<Vec<HistoryEntry>>,
    pub pending_action: Mutex<Option<ActionCommand>>,
    pub current_goal: Mutex<Option<String>>,
}

#[tauri::command]
async fn save_api_key(key: String, state: State<'_, AppState>) -> Result<(), String> {
    *state.api_key.lock().unwrap() = Some(key.clone());
    let config_dir = dirs::config_dir().ok_or("No config dir")?.join("pc-automation-agent");
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join("config.json"), serde_json::json!({"api_key": key}).to_string()).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn load_api_key(state: State<'_, AppState>) -> Result<Option<String>, String> {
    if let Some(k) = state.api_key.lock().unwrap().clone() { return Ok(Some(k)); }
    let p = dirs::config_dir().ok_or("No config dir")?.join("pc-automation-agent").join("config.json");
    if p.exists() {
        let c: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if let Some(k) = c["api_key"].as_str() { 
            *state.api_key.lock().unwrap() = Some(k.to_string()); 
            return Ok(Some(k.to_string())); 
        }
    }
    Ok(None)
}

#[tauri::command]
async fn get_current_state() -> Result<ExecutionState, String> {
    get_current_state_auto().await
}

#[tauri::command]
async fn execute_user_command(command: String, state: State<'_, AppState>, window: tauri::Window) -> Result<ActionCommand, String> {
    *state.current_goal.lock().unwrap() = Some(command.clone());
    // Clear history for new goal
    state.history.lock().unwrap().clear();

    // Emit progress: scanning UI
    let _ = window.emit("progress", serde_json::json!({"stage": "scanning", "message": "Scanning UI elements..."}));
    logging::log_action("INFO", "PROGRESS", "Scanning UI elements", None);

    let start = std::time::Instant::now();
    let cs = get_current_state_auto().await?;
    let scan_ms = start.elapsed().as_millis();
    logging::log_action("INFO", "PERF", &format!("UI scan completed in {}ms", scan_ms), None);

    let api_key = state.api_key.lock().unwrap().clone().ok_or("API key not set")?;
    let history: Vec<HistoryEntry> = state.history.lock().unwrap().clone();

    // Emit progress: calling LLM
    let _ = window.emit("progress", serde_json::json!({"stage": "thinking", "message": "AI is deciding next action..."}));
    logging::log_action("INFO", "PROGRESS", "Calling Claude API", None);

    let start = std::time::Instant::now();
    let llm_response = ai::claude::get_next_action(&api_key, &command, &cs, &history)
        .await
        .map_err(|e| e.to_string())?;
    let llm_ms = start.elapsed().as_millis();

    // Log with detailed info
    logging::log_llm_call(
        llm_response.input_tokens,
        llm_response.output_tokens,
        &llm_response.action.action_type,
        llm_response.elements_count,
        llm_response.prompt_chars
    );
    logging::log_action("INFO", "PERF", &format!("LLM responded in {}ms", llm_ms), None);

    // Emit progress: done
    let _ = window.emit("progress", serde_json::json!({
        "stage": "ready",
        "message": format!("Action: {} ({}ms scan, {}ms LLM)", llm_response.action.action_type, scan_ms, llm_ms),
        "action": llm_response.action.action_type
    }));

    *state.pending_action.lock().unwrap() = Some(llm_response.action.clone());
    Ok(llm_response.action)
}

#[tauri::command]
async fn approve_action(approved: bool, state: State<'_, AppState>, window: tauri::Window) -> Result<ExecutionState, String> {
    if !approved {
        *state.pending_action.lock().unwrap() = None;
        return Err("Rejected".to_string());
    }

    let action = state.pending_action.lock().unwrap().clone().ok_or("No pending action")?;
    let goal = state.current_goal.lock().unwrap().clone().ok_or("No goal set")?;
    let api_key = state.api_key.lock().unwrap().clone().ok_or("No API key")?;

    let max_steps = 20;  // Maximum steps to prevent infinite loops
    let max_retries_per_step = 5;
    let auto_complete_threshold = 3;  // Auto-complete after N consecutive successful steps
    let mut consecutive_successes = 0;
    let mut current_action = action;

    // Track initial mode
    let initial_mode = detect_automation_mode().await;
    let initial_mode_str = match initial_mode { AutomationMode::Browser => "browser", AutomationMode::Desktop => "desktop" };
    logging::log_action("INFO", "MODE", &format!("Initial mode: {}", initial_mode_str), None);

    // Track current mode (can change during execution)
    let mut current_mode_str = initial_mode_str.to_string();

    // Emit: starting execution
    let _ = window.emit("progress", serde_json::json!({"stage": "executing", "message": "Starting execution..."}));

    // Get initial state ONCE
    let mut current_state = get_current_state_auto().await?;

    for loop_iter in 0..max_steps {
        let step_number = state.history.lock().unwrap().len() as u32 + 1;
        logging::log_action("INFO", "LOOP", &format!(
            "=== Loop iteration {}, Step {}, Action: '{}' ===",
            loop_iter + 1, step_number, current_action.action_type
        ), None);

        // Emit step progress
        let _ = window.emit("progress", serde_json::json!({
            "stage": "step",
            "step": step_number,
            "action": current_action.action_type,
            "message": format!("Step {}: {}", step_number, current_action.action_type)
        }));

        // Check if this is the "complete" action
        if current_action.action_type == "complete" {
            logging::log_action("INFO", "COMPLETE", &format!(
                "Goal achieved! Reason: {}",
                current_action.reasoning.clone().unwrap_or_else(|| "No reason provided".to_string())
            ), None);
            let entry = HistoryEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                step_number,
                user_input: Some(goal.clone()),
                llm_reasoning: current_action.reasoning.clone().unwrap_or_else(|| "Goal completed".to_string()),
                action: current_action.clone(),
                success: true,
                error: None,
                mode: current_mode_str.clone(),
                window_context: current_state.active_window.clone(),
                input_tokens: None,
                output_tokens: None,
            };
            state.history.lock().unwrap().push(entry);
            *state.pending_action.lock().unwrap() = None;
            return Ok(current_state);
        }

        // Try to execute current action with retries
        let mut retry_count = 0;
        let mut chunk_index = 0;
        let mut action_to_try = current_action.clone();
        let mut action_succeeded = false;

        loop {
            retry_count += 1;

            // Log action start
            logging::log_action_start(&action_to_try, step_number, &current_mode_str);

            match execute_action_auto(&action_to_try).await {
                Ok(_) => {
                    // Log success
                    logging::log_action_result(&action_to_try, step_number, true, None);

                    // Wait for UI to settle after action
                    logging::log_action("DEBUG", "STATE", &format!("Step {} succeeded, waiting for UI to settle...", step_number), None);
                    std::thread::sleep(std::time::Duration::from_millis(1500));

                    // Get FRESH state after action - this captures the new focused window/app
                    let fresh_state = match get_current_state_auto().await {
                        Ok(s) => {
                            logging::log_action("DEBUG", "STATE", &format!("Fresh state: window='{}', url={:?}", s.active_window, s.url), None);
                            s
                        }
                        Err(e) => {
                            logging::log_action("WARN", "STATE", &format!("Failed to get fresh state: {}, using previous", e), None);
                            current_state.clone()
                        }
                    };

                    // Detect if mode changed (e.g., Desktop -> Browser after opening Chrome)
                    let new_mode = detect_automation_mode().await;
                    let new_mode_str = match new_mode { AutomationMode::Browser => "browser", AutomationMode::Desktop => "desktop" };

                    let mode_changed = new_mode_str != current_mode_str;
                    if mode_changed {
                        logging::log_action("INFO", "MODE", &format!(
                            "*** MODE TRANSITION: {} -> {} (Browser detected!) ***",
                            current_mode_str, new_mode_str
                        ), None);
                        current_mode_str = new_mode_str.to_string();
                    }

                    // Action succeeded - record in history with fresh window context
                    // Include mode transition info if it happened
                    let reasoning = if mode_changed {
                        format!("{} [MODE: {} -> {}]",
                            action_to_try.reasoning.clone().unwrap_or_default(),
                            initial_mode_str, new_mode_str)
                    } else {
                        action_to_try.reasoning.clone().unwrap_or_default()
                    };

                    let entry = HistoryEntry {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        step_number,
                        user_input: if step_number == 1 { Some(goal.clone()) } else { None },
                        llm_reasoning: reasoning,
                        action: action_to_try.clone(),
                        success: true,
                        error: None,
                        mode: current_mode_str.clone(),
                        window_context: fresh_state.active_window.clone(),
                        input_tokens: None,
                        output_tokens: None,
                    };
                    state.history.lock().unwrap().push(entry);
                    current_state = fresh_state;
                    action_succeeded = true;
                    consecutive_successes += 1;
                    break;
                }
                Err(e) if retry_count < max_retries_per_step => {
                    // Log failure and retry
                    logging::log_action_result(&action_to_try, step_number, false, Some(&e));
                    logging::log_action("DEBUG", "RETRY", &format!("Retrying step {} with chunk {}", step_number, chunk_index + 1), None);

                    // Retry with next chunk - reuse current_state, don't fetch again
                    chunk_index += 1;
                    let history = state.history.lock().unwrap().clone();

                    let llm_response = ai::claude::get_retry_action(
                        &api_key, &action_to_try, &e, &current_state, &history, chunk_index
                    ).await.map_err(|e| e.to_string())?;

                    action_to_try = llm_response.action;
                }
                Err(e) => {
                    // Log final failure
                    logging::log_action_result(&action_to_try, step_number, false, Some(&e));
                    logging::log_action("WARN", "ACTION", &format!("Step {} failed after {} retries", step_number, retry_count), None);

                    // Max retries reached - record failure
                    let entry = HistoryEntry {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        step_number,
                        user_input: if step_number == 1 { Some(goal.clone()) } else { None },
                        llm_reasoning: action_to_try.reasoning.clone().unwrap_or_default(),
                        action: action_to_try.clone(),
                        success: false,
                        error: Some(e.clone()),
                        mode: current_mode_str.clone(),
                        window_context: current_state.active_window.clone(),
                        input_tokens: None,
                        output_tokens: None,
                    };
                    state.history.lock().unwrap().push(entry);
                    consecutive_successes = 0;  // Reset on failure
                    break;
                }
            }
        }

        // Check for auto-complete after threshold consecutive successes
        if action_succeeded && consecutive_successes >= auto_complete_threshold {
            logging::log_action("INFO", "AUTO_COMPLETE", &format!(
                "Threshold reached ({} steps), checking if goal achieved via a11y...",
                consecutive_successes
            ), None);

            // Fetch fresh a11y tree and check if goal keywords are present
            let fresh_state = get_current_state_auto().await.unwrap_or(current_state.clone());
            let goal_achieved = check_goal_in_a11y(&goal, &fresh_state);

            let (completion_type, error_msg) = if goal_achieved {
                logging::log_action("INFO", "AUTO_COMPLETE", &format!(
                    "Goal keywords found in a11y - marking as COMPLETE"
                ), None);
                ("smart_complete", None)
            } else {
                logging::log_action("WARN", "AUTO_COMPLETE", &format!(
                    "Goal keywords NOT found in a11y - marking as INCOMPLETE for learning"
                ), None);
                ("auto_complete", Some("[INCOMPLETE] LLM did not signal completion, goal keywords not found in a11y".to_string()))
            };

            // Record in history
            let entry = HistoryEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                step_number: state.history.lock().unwrap().len() as u32 + 1,
                user_input: None,
                llm_reasoning: format!("[{}] After {} steps - {}",
                    completion_type.to_uppercase(),
                    consecutive_successes,
                    if goal_achieved { "goal keywords found in a11y" } else { "LLM failed to signal, keywords not found" }
                ),
                action: ActionCommand {
                    action_type: completion_type.to_string(),
                    target: serde_json::Value::Null,
                    params: Some(serde_json::json!({
                        "reason": if goal_achieved { "goal_detected" } else { "threshold_reached" },
                        "steps": consecutive_successes,
                        "goal_found": goal_achieved
                    })),
                    reasoning: Some(format!("{}: {} steps, goal_in_a11y={}", completion_type, consecutive_successes, goal_achieved)),
                },
                success: goal_achieved,
                error: error_msg,
                mode: current_mode_str.clone(),
                window_context: fresh_state.active_window.clone(),
                input_tokens: None,
                output_tokens: None,
            };
            state.history.lock().unwrap().push(entry);

            // Emit completion to UI
            let _ = window.emit("progress", serde_json::json!({
                "stage": completion_type,
                "message": format!("Task {} after {} steps",
                    if goal_achieved { "completed" } else { "auto-completed (incomplete)" },
                    consecutive_successes
                ),
                "goal_achieved": goal_achieved
            }));

            *state.pending_action.lock().unwrap() = None;
            return Ok(fresh_state);
        }

        // If action failed, get fresh state for next LLM call
        if !action_succeeded {
            logging::log_action("DEBUG", "STATE", "Action failed, fetching fresh state...", None);
            current_state = get_current_state_auto().await?;
        }

        // Get next action from LLM with current state and history
        let history = state.history.lock().unwrap().clone();
        logging::log_action("DEBUG", "LLM", &format!(
            "Requesting next action: goal='{}', window='{}', history_len={}",
            goal, current_state.active_window, history.len()
        ), None);

        let llm_response = match ai::claude::get_next_action(&api_key, &goal, &current_state, &history).await {
            Ok(r) => {
                logging::log_action("DEBUG", "LLM", &format!(
                    "LLM returned: action='{}', target={:?}",
                    r.action.action_type, r.action.target
                ), None);
                r
            }
            Err(e) => {
                logging::log_action("ERROR", "LLM", &format!("LLM call failed: {}", e), None);
                return Err(e.to_string());
            }
        };

        // Log LLM call with detailed info
        logging::log_llm_call(
            llm_response.input_tokens,
            llm_response.output_tokens,
            &llm_response.action.action_type,
            llm_response.elements_count,
            llm_response.prompt_chars
        );

        // Update token counts in last history entry
        {
            let mut hist = state.history.lock().unwrap();
            if let Some(last) = hist.last_mut() {
                last.input_tokens = Some(llm_response.input_tokens);
                last.output_tokens = Some(llm_response.output_tokens);
            }
        }

        current_action = llm_response.action;
    }

    logging::log_action("WARN", "LOOP", &format!("Max steps ({}) reached without completion", max_steps), None);
    *state.pending_action.lock().unwrap() = None;
    Ok(current_state)
}

#[tauri::command]
async fn get_history(state: State<'_, AppState>) -> Result<Vec<HistoryEntry>, String> { 
    Ok(state.history.lock().unwrap().clone()) 
}

#[tauri::command]
async fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    state.history.lock().unwrap().clear();
    logging::clear_logs();
    logging::log_action("INFO", "SESSION", "History cleared", None);
    Ok(())
}

#[tauri::command]
async fn get_history_analysis(state: State<'_, AppState>) -> Result<logging::HistoryAnalysis, String> {
    let history = state.history.lock().unwrap().clone();
    Ok(logging::analyze_history(&history))
}

#[tauri::command]
async fn take_screenshot_to_clipboard() -> Result<(), String> {
    use arboard::{Clipboard, ImageData};
    use xcap::Monitor;

    let monitors = Monitor::all().map_err(|e| e.to_string())?;
    let monitor = monitors.first().ok_or("No monitor found")?;
    let img = monitor.capture_image().map_err(|e| e.to_string())?;

    let width = img.width() as usize;
    let height = img.height() as usize;
    let raw = img.into_raw();

    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_image(ImageData {
        width,
        height,
        bytes: std::borrow::Cow::Owned(raw),
    }).map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
async fn get_screen_a11y_tree() -> Result<String, String> {
    let ps_script = r#"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$auto = [System.Windows.Automation.AutomationElement]::RootElement
$walker = [System.Windows.Automation.TreeWalker]::ContentViewWalker

function Get-Tree {
    param($el, $depth)
    if ($depth -gt 4 -or $null -eq $el) { return $null }

    $current = $el.Current
    $children = @()

    $child = $walker.GetFirstChild($el)
    while ($null -ne $child) {
        $c = Get-Tree -el $child -depth ($depth + 1)
        if ($c) { $children += $c }
        $child = $walker.GetNextSibling($child)
    }

    @{
        name = $current.Name
        type = $current.ControlType.ProgrammaticName
        className = $current.ClassName
        automationId = $current.AutomationId
        children = $children
    }
}

$tree = Get-Tree -el $auto -depth 0
$tree | ConvertTo-Json -Depth 20 -Compress
"#;

    let output = std::process::Command::new("powershell")
        .args(["-ExecutionPolicy", "Bypass", "-Command", ps_script])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Extract keywords from goal and check if they appear in a11y tree or window title
/// Used for smart completion detection
fn check_goal_in_a11y(goal: &str, state: &ExecutionState) -> bool {
    // Extract meaningful keywords from goal (skip common words)
    let stop_words = ["the", "a", "an", "in", "on", "for", "to", "and", "or", "of", "with",
                      "open", "search", "find", "go", "click", "type", "press", "enter",
                      "chrome", "browser", "google"];

    let goal_lower = goal.to_lowercase();
    let keywords: Vec<&str> = goal_lower
        .split_whitespace()
        .filter(|w| w.len() > 2 && !stop_words.contains(w))
        .collect();

    if keywords.is_empty() {
        // No meaningful keywords, can't verify
        return false;
    }

    // Build searchable text from state
    let mut searchable = state.active_window.to_lowercase();

    // Add URL if present
    if let Some(url) = &state.url {
        searchable.push(' ');
        searchable.push_str(&url.to_lowercase());
    }

    // Add a11y tree names
    fn collect_names(tree: &serde_json::Value, names: &mut String) {
        if let Some(obj) = tree.as_object() {
            if let Some(name) = obj.get("name").and_then(|n| n.as_str()) {
                if !name.is_empty() {
                    names.push(' ');
                    names.push_str(&name.to_lowercase());
                }
            }
            if let Some(children) = obj.get("children").and_then(|c| c.as_array()) {
                for child in children {
                    collect_names(child, names);
                }
            }
        } else if let Some(arr) = tree.as_array() {
            for item in arr {
                collect_names(item, names);
            }
        }
    }
    collect_names(&state.accessibility_tree, &mut searchable);

    // Check if any keyword is found
    let found = keywords.iter().any(|kw| searchable.contains(*kw));

    logging::log_action("DEBUG", "GOAL_CHECK", &format!(
        "Keywords: {:?}, Found: {}, Window: '{}'",
        keywords, found, state.active_window
    ), None);

    found
}

/// Automation mode - browser (Chrome CDP) or desktop (Windows UI Automation)
#[derive(Debug, Clone, Copy, PartialEq)]
enum AutomationMode {
    Browser,
    Desktop,
}

/// Detect which automation mode to use based on whether Chrome is available
async fn detect_automation_mode() -> AutomationMode {
    // Try to connect to Chrome debugging port
    match automation::chrome_cdp::ChromeConnection::connect_to_first_tab(9222).await {
        Ok(_) => AutomationMode::Browser,
        Err(_) => AutomationMode::Desktop,
    }
}

async fn get_browser_state() -> Result<ExecutionState, String> {
    let conn = automation::chrome_cdp::ChromeConnection::connect_to_first_tab(9222)
        .await
        .map_err(|e| format!("Chrome connection failed: {}. Make sure Chrome is running with --remote-debugging-port=9222", e))?;

    let browser_state = conn.get_browser_state()
        .await
        .map_err(|e| e.to_string())?;

    Ok(ExecutionState {
        screenshot_base64: browser_state.screenshot_base64,
        accessibility_tree: serde_json::to_value(&browser_state.accessibility_tree).unwrap_or_default(),
        active_window: browser_state.title,
        url: Some(browser_state.url),
        success: true,
        error: None,
    })
}

#[cfg(target_os = "windows")]
fn get_desktop_state_sync() -> Result<ExecutionState, String> {
    let wa = automation::windows_ui::WindowsAutomation::new()
        .map_err(|e| e.to_string())?;

    let desktop_state = wa.get_desktop_state()
        .map_err(|e| e.to_string())?;

    Ok(ExecutionState {
        screenshot_base64: desktop_state.screenshot_base64,
        accessibility_tree: serde_json::to_value(&desktop_state.accessibility_tree).unwrap_or_default(),
        active_window: desktop_state.window_title,
        url: None,
        success: true,
        error: None,
    })
}

#[cfg(not(target_os = "windows"))]
fn get_desktop_state_sync() -> Result<ExecutionState, String> {
    Err("Desktop automation is only available on Windows".to_string())
}

async fn get_current_state_auto() -> Result<ExecutionState, String> {
    match detect_automation_mode().await {
        AutomationMode::Browser => get_browser_state().await,
        AutomationMode::Desktop => get_desktop_state_sync(),
    }
}

async fn execute_browser_action(action: &ActionCommand) -> Result<ExecutionState, String> {
    let conn = automation::chrome_cdp::ChromeConnection::connect_to_first_tab(9222)
        .await
        .map_err(|e| e.to_string())?;

    conn.execute_llm_action(&action.action_type, &action.target, action.params.as_ref())
        .await
        .map_err(|e| e.to_string())?;

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    get_browser_state().await
}

#[cfg(target_os = "windows")]
fn execute_desktop_action_sync(action: &ActionCommand) -> Result<ExecutionState, String> {
    let wa = automation::windows_ui::WindowsAutomation::new()
        .map_err(|e| e.to_string())?;

    wa.execute_llm_action(&action.action_type, &action.target, action.params.as_ref())
        .map_err(|e| e.to_string())?;

    std::thread::sleep(std::time::Duration::from_millis(500));
    get_desktop_state_sync()
}

#[cfg(not(target_os = "windows"))]
fn execute_desktop_action_sync(_action: &ActionCommand) -> Result<ExecutionState, String> {
    Err("Desktop automation is only available on Windows".to_string())
}

async fn execute_action_auto(action: &ActionCommand) -> Result<ExecutionState, String> {
    match detect_automation_mode().await {
        AutomationMode::Browser => execute_browser_action(action).await,
        AutomationMode::Desktop => execute_desktop_action_sync(action),
    }
}

fn main() {
    // Initialize logging
    logging::init_logging();
    logging::log_action("INFO", "SESSION", "Application starting", None);

    tauri::Builder::default()
        .setup(|app| {
            app.manage(AppState {
                api_key: Mutex::new(None),
                history: Mutex::new(Vec::new()),
                pending_action: Mutex::new(None),
                current_goal: Mutex::new(None),
            });

            // Try to launch Chrome with debugging
            std::thread::spawn(|| {
                let _ = automation::chrome_cdp::launch_chrome_with_debugging(9222);
            });

            logging::log_action("INFO", "SESSION", "Application setup complete", None);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            save_api_key,
            load_api_key,
            get_current_state,
            execute_user_command,
            approve_action,
            get_history,
            clear_history,
            get_history_analysis,
            take_screenshot_to_clipboard,
            get_screen_a11y_tree
        ])
        .run(tauri::generate_context!())
        .expect("error running app");
}