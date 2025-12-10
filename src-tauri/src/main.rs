#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod automation;
mod ai;

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
    pub user_input: Option<String>, 
    pub llm_reasoning: String, 
    pub action: ActionCommand, 
    pub success: bool, 
    pub error: Option<String> 
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
    get_browser_state().await
}

#[tauri::command]
async fn execute_user_command(command: String, state: State<'_, AppState>) -> Result<ActionCommand, String> {
    *state.current_goal.lock().unwrap() = Some(command.clone());
    
    let cs = get_browser_state().await?;
    
    // Get history without holding the lock across await
    let recent: Vec<HistoryEntry> = {
        let h = state.history.lock().unwrap();
        h.iter().rev().take(10).cloned().collect()
    };
    
    let api_key = state.api_key.lock().unwrap().clone().ok_or("API key not set")?;
    
    let action = ai::claude::get_next_action(&api_key, &command, &cs, &recent)
        .await
        .map_err(|e| e.to_string())?;
    
    *state.pending_action.lock().unwrap() = Some(action.clone());
    Ok(action)
}

#[tauri::command]
async fn approve_action(approved: bool, state: State<'_, AppState>) -> Result<ExecutionState, String> {
    if !approved { 
        *state.pending_action.lock().unwrap() = None; 
        return Err("Rejected".to_string()); 
    }
    
    let action = state.pending_action.lock().unwrap().clone().ok_or("No pending action")?;
    let goal = state.current_goal.lock().unwrap().clone();
    let api_key = state.api_key.lock().unwrap().clone().ok_or("No API key")?;
    
    let mut attempts = 0;
    let mut current_action = action.clone();
    
    loop {
        attempts += 1;
        match execute_browser_action(&current_action).await {
            Ok(new_state) => {
                let entry = HistoryEntry { 
                    timestamp: chrono::Utc::now().to_rfc3339(), 
                    user_input: goal.clone(), 
                    llm_reasoning: current_action.reasoning.clone().unwrap_or_default(), 
                    action: current_action.clone(), 
                    success: true, 
                    error: None 
                };
                state.history.lock().unwrap().push(entry);
                *state.pending_action.lock().unwrap() = None;
                return Ok(new_state);
            }
            Err(e) if attempts < 3 => {
                let failure_state = get_browser_state().await?;
                let recent: Vec<HistoryEntry> = {
                    let h = state.history.lock().unwrap();
                    h.iter().rev().take(10).cloned().collect()
                };
                current_action = ai::claude::get_retry_action(&api_key, &current_action, &e, &failure_state, &recent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => {
                let entry = HistoryEntry { 
                    timestamp: chrono::Utc::now().to_rfc3339(), 
                    user_input: goal.clone(), 
                    llm_reasoning: format!("Failed after {} attempts", attempts), 
                    action: current_action.clone(), 
                    success: false, 
                    error: Some(e.clone()) 
                };
                state.history.lock().unwrap().push(entry);
                *state.pending_action.lock().unwrap() = None;
                return Err(format!("Failed after {} attempts: {}", attempts, e));
            }
        }
    }
}

#[tauri::command]
async fn get_history(state: State<'_, AppState>) -> Result<Vec<HistoryEntry>, String> { 
    Ok(state.history.lock().unwrap().clone()) 
}

#[tauri::command]
async fn clear_history(state: State<'_, AppState>) -> Result<(), String> { 
    state.history.lock().unwrap().clear(); 
    Ok(()) 
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

fn main() {
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
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            save_api_key, 
            load_api_key, 
            get_current_state, 
            execute_user_command, 
            approve_action, 
            get_history, 
            clear_history
        ])
        .run(tauri::generate_context!())
        .expect("error running app");
}