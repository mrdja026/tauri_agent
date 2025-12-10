use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AXNode {
    pub node_id: String,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub bounds: Option<Bounds>,
    pub focusable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo { pub id: String, pub title: String, pub url: String, pub ws_url: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserState { pub url: String, pub title: String, pub screenshot_base64: String, pub accessibility_tree: Vec<AXNode> }

pub struct ChromeConnection {
    ws_write: Arc<Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>>>,
    ws_read: Arc<Mutex<futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>>,
    cmd_id: Arc<Mutex<u64>>,
}

pub fn launch_chrome_with_debugging(port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(target_os = "windows")] {
        let paths = vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ];
        let chrome = paths.iter().find(|p| std::path::Path::new(p).exists()).ok_or("Chrome not found")?;
        let data_dir = std::env::temp_dir().join("chrome-automation");
        std::fs::create_dir_all(&data_dir)?;
        Command::new(chrome).args(&[&format!("--remote-debugging-port={}", port), &format!("--user-data-dir={}", data_dir.display()), "--no-first-run"]).spawn()?;
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    #[cfg(target_os = "macos")] {
        Command::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome").args(&[&format!("--remote-debugging-port={}", port), "--user-data-dir=/tmp/chrome-auto", "--no-first-run"]).spawn()?;
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    #[cfg(target_os = "linux")] {
        Command::new("google-chrome").args(&[&format!("--remote-debugging-port={}", port), "--user-data-dir=/tmp/chrome-auto", "--no-first-run"]).spawn()?;
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Ok(())
}

pub async fn get_tabs(port: u16) -> Result<Vec<TabInfo>, Box<dyn std::error::Error + Send + Sync>> {
    let resp: Vec<Value> = reqwest::Client::new().get(format!("http://localhost:{}/json", port)).send().await?.json().await?;
    Ok(resp.iter().filter(|t| t["type"] == "page").map(|t| TabInfo {
        id: t["id"].as_str().unwrap_or("").to_string(),
        title: t["title"].as_str().unwrap_or("").to_string(),
        url: t["url"].as_str().unwrap_or("").to_string(),
        ws_url: t["webSocketDebuggerUrl"].as_str().unwrap_or("").to_string(),
    }).collect())
}

impl ChromeConnection {
    pub async fn connect(ws_url: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (ws, _) = connect_async(ws_url).await?;
        let (w, r) = ws.split();
        Ok(Self { ws_write: Arc::new(Mutex::new(w)), ws_read: Arc::new(Mutex::new(r)), cmd_id: Arc::new(Mutex::new(0)) })
    }

    pub async fn connect_to_first_tab(port: u16) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let tabs = get_tabs(port).await?;
        let tab = tabs.first().ok_or("No tabs")?;
        Self::connect(&tab.ws_url).await
    }

    async fn send(&self, method: &str, params: Value) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let mut id = self.cmd_id.lock().await;
        *id += 1;
        let cid = *id;
        drop(id);
        let cmd = json!({"id": cid, "method": method, "params": params});
        self.ws_write.lock().await.send(Message::Text(cmd.to_string())).await?;
        loop {
            if let Some(msg) = self.ws_read.lock().await.next().await {
                if let Message::Text(txt) = msg? {
                    let r: Value = serde_json::from_str(&txt)?;
                    if r.get("id").and_then(|i| i.as_u64()) == Some(cid) {
                        if let Some(e) = r.get("error") { return Err(format!("CDP: {:?}", e).into()); }
                        return Ok(r["result"].clone());
                    }
                }
            }
        }
    }

    pub async fn navigate(&self, url: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Page.navigate", json!({"url": url})).await?;
        self.send("Page.enable", json!({})).await?;
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        Ok(())
    }

    pub async fn get_url(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let r = self.send("Runtime.evaluate", json!({"expression": "window.location.href"})).await?;
        Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
    }

    pub async fn get_a11y_tree(&self) -> Result<Vec<AXNode>, Box<dyn std::error::Error + Send + Sync>> {
        self.send("Accessibility.enable", json!({})).await?;
        let r = self.send("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = r["nodes"].as_array().ok_or("No nodes")?;
        let roles = vec!["button", "link", "textbox", "searchbox", "combobox", "checkbox", "radio", "menuitem", "tab", "listitem"];
        Ok(nodes.iter().filter(|n| {
            let role = n["role"]["value"].as_str().unwrap_or("");
            roles.contains(&role) || n["focusable"]["value"].as_bool().unwrap_or(false)
        }).filter_map(|n| {
            Some(AXNode {
                node_id: n["nodeId"].as_str()?.to_string(),
                role: n["role"]["value"].as_str().unwrap_or("").to_string(),
                name: n["name"]["value"].as_str().map(|s| s.to_string()),
                value: n["value"]["value"].as_str().map(|s| s.to_string()),
                bounds: n["boundingBox"].as_object().map(|b| Bounds { x: b["x"].as_f64().unwrap_or(0.0), y: b["y"].as_f64().unwrap_or(0.0), width: b["width"].as_f64().unwrap_or(0.0), height: b["height"].as_f64().unwrap_or(0.0) }),
                focusable: n["focusable"]["value"].as_bool().unwrap_or(false),
            })
        }).collect())
    }

    pub async fn find_element(&self, selector: &str) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        self.send("DOM.enable", json!({})).await?;
        let doc = self.send("DOM.getDocument", json!({})).await?;
        let root = doc["root"]["nodeId"].as_i64().ok_or("No root")?;
        let r = self.send("DOM.querySelector", json!({"nodeId": root, "selector": selector})).await?;
        r["nodeId"].as_i64().ok_or("Not found".into())
    }

    pub async fn get_bounds(&self, node_id: i64) -> Result<Bounds, Box<dyn std::error::Error + Send + Sync>> {
        let r = self.send("DOM.getBoxModel", json!({"nodeId": node_id})).await?;
        let c = r["model"]["content"].as_array().ok_or("No box")?;
        Ok(Bounds { x: c[0].as_f64().unwrap_or(0.0), y: c[1].as_f64().unwrap_or(0.0), width: c[4].as_f64().unwrap_or(0.0) - c[0].as_f64().unwrap_or(0.0), height: c[5].as_f64().unwrap_or(0.0) - c[1].as_f64().unwrap_or(0.0) })
    }

    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Input.dispatchMouseEvent", json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1})).await?;
        self.send("Input.dispatchMouseEvent", json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1})).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        Ok(())
    }

    pub async fn click_element(&self, selector: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let id = self.find_element(selector).await?;
        let b = self.get_bounds(id).await?;
        self.click_at(b.x + b.width / 2.0, b.y + b.height / 2.0).await
    }

    pub async fn click_ax(&self, ax_id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let tree = self.send("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = tree["nodes"].as_array().ok_or("No nodes")?;
        let node = nodes.iter().find(|n| n["nodeId"].as_str() == Some(ax_id)).ok_or("AX not found")?;
        let backend = node["backendDOMNodeId"].as_i64().ok_or("No backend")?;
        let r = self.send("DOM.getBoxModel", json!({"backendNodeId": backend})).await?;
        let c = r["model"]["content"].as_array().ok_or("No box")?;
        let cx = (c[0].as_f64().unwrap_or(0.0) + c[4].as_f64().unwrap_or(0.0)) / 2.0;
        let cy = (c[1].as_f64().unwrap_or(0.0) + c[5].as_f64().unwrap_or(0.0)) / 2.0;
        self.click_at(cx, cy).await
    }

    pub async fn type_text(&self, text: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Input.insertText", json!({"text": text})).await?;
        Ok(())
    }

    pub async fn type_into(&self, selector: &str, text: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.click_element(selector).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        self.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "a", "modifiers": 2})).await?;
        self.type_text(text).await
    }

    pub async fn press_key(&self, key: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": key})).await?;
        self.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": key})).await?;
        Ok(())
    }

    pub async fn scroll(&self, dy: f64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Input.dispatchMouseEvent", json!({"type": "mouseWheel", "x": 400, "y": 300, "deltaX": 0, "deltaY": dy})).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        Ok(())
    }

    pub async fn screenshot(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let r = self.send("Page.captureScreenshot", json!({"format": "png"})).await?;
        Ok(r["data"].as_str().unwrap_or("").to_string())
    }

    // Bring page/tab to front (activate tab)
    pub async fn focus_window(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Page.bringToFront", json!({})).await?;
        Ok(())
    }

    // Find element by XPath and return node ID
    pub async fn find_by_xpath(&self, xpath: &str) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        self.send("DOM.enable", json!({})).await?;
        let _ = self.send("DOM.getDocument", json!({})).await?;

        // Use DOM.performSearch for XPath
        let search = self.send("DOM.performSearch", json!({"query": xpath})).await?;
        let search_id = search["searchId"].as_str().ok_or("No searchId")?;
        let count = search["resultCount"].as_i64().unwrap_or(0);

        if count == 0 {
            self.send("DOM.discardSearchResults", json!({"searchId": search_id})).await?;
            return Err("XPath not found".into());
        }

        let results = self.send("DOM.getSearchResults", json!({
            "searchId": search_id,
            "fromIndex": 0,
            "toIndex": 1
        })).await?;

        self.send("DOM.discardSearchResults", json!({"searchId": search_id})).await?;

        results["nodeIds"][0].as_i64().ok_or("No node found".into())
    }

    // Click element by XPath
    pub async fn click_xpath(&self, xpath: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let node_id = self.find_by_xpath(xpath).await?;
        let b = self.get_bounds(node_id).await?;
        self.click_at(b.x + b.width / 2.0, b.y + b.height / 2.0).await
    }

    // Hover over element (move mouse without clicking)
    pub async fn hover_at(&self, x: f64, y: f64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Input.dispatchMouseEvent", json!({"type": "mouseMoved", "x": x, "y": y})).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        Ok(())
    }

    pub async fn hover_element(&self, selector: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let id = self.find_element(selector).await?;
        let b = self.get_bounds(id).await?;
        self.hover_at(b.x + b.width / 2.0, b.y + b.height / 2.0).await
    }

    // Get text content of element
    pub async fn get_text(&self, selector: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let js = format!(r#"document.querySelector('{}')?.innerText || ''"#, selector.replace('\'', "\\'"));
        let r = self.send("Runtime.evaluate", json!({"expression": js})).await?;
        Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
    }

    // Get attribute value
    pub async fn get_attribute(&self, selector: &str, attr: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let js = format!(r#"document.querySelector('{}')?.getAttribute('{}') || ''"#,
            selector.replace('\'', "\\'"), attr.replace('\'', "\\'"));
        let r = self.send("Runtime.evaluate", json!({"expression": js})).await?;
        Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
    }

    // Select option from dropdown
    pub async fn select_option(&self, selector: &str, value: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let js = format!(r#"
            (function() {{
                const el = document.querySelector('{}');
                if (!el) return false;
                el.value = '{}';
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return true;
            }})()
        "#, selector.replace('\'', "\\'"), value.replace('\'', "\\'"));
        self.send("Runtime.evaluate", json!({"expression": js})).await?;
        Ok(())
    }

    // Wait for element to appear (polling)
    pub async fn wait_for_element(&self, selector: &str, timeout_ms: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);

        while start.elapsed() < timeout {
            let js = format!(r#"!!document.querySelector('{}')"#, selector.replace('\'', "\\'"));
            let r = self.send("Runtime.evaluate", json!({"expression": js})).await?;
            if r["result"]["value"].as_bool().unwrap_or(false) {
                return Ok(());
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        Err(format!("Timeout waiting for {}", selector).into())
    }

    // Execute arbitrary JavaScript
    pub async fn eval_js(&self, js: &str) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let r = self.send("Runtime.evaluate", json!({"expression": js, "returnByValue": true})).await?;
        Ok(r["result"]["value"].clone())
    }

    // Double click
    pub async fn double_click_at(&self, x: f64, y: f64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Input.dispatchMouseEvent", json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 2})).await?;
        self.send("Input.dispatchMouseEvent", json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 2})).await?;
        Ok(())
    }

    // Right click
    pub async fn right_click_at(&self, x: f64, y: f64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Input.dispatchMouseEvent", json!({"type": "mousePressed", "x": x, "y": y, "button": "right", "clickCount": 1})).await?;
        self.send("Input.dispatchMouseEvent", json!({"type": "mouseReleased", "x": x, "y": y, "button": "right", "clickCount": 1})).await?;
        Ok(())
    }

    // Clear input field
    pub async fn clear_input(&self, selector: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.click_element(selector).await?;
        // Select all and delete
        self.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "a", "modifiers": 2})).await?; // Ctrl+A
        self.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "a", "modifiers": 2})).await?;
        self.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Backspace"})).await?;
        self.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Backspace"})).await?;
        Ok(())
    }

    // Go back in history
    pub async fn go_back(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Page.enable", json!({})).await?;
        let history = self.send("Page.getNavigationHistory", json!({})).await?;
        let current = history["currentIndex"].as_i64().unwrap_or(0);
        if current > 0 {
            let entries = history["entries"].as_array().ok_or("No entries")?;
            let entry_id = entries[(current - 1) as usize]["id"].as_i64().ok_or("No id")?;
            self.send("Page.navigateToHistoryEntry", json!({"entryId": entry_id})).await?;
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        Ok(())
    }

    // Go forward in history
    pub async fn go_forward(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Page.enable", json!({})).await?;
        let history = self.send("Page.getNavigationHistory", json!({})).await?;
        let current = history["currentIndex"].as_i64().unwrap_or(0);
        let entries = history["entries"].as_array().ok_or("No entries")?;
        if (current as usize) < entries.len() - 1 {
            let entry_id = entries[(current + 1) as usize]["id"].as_i64().ok_or("No id")?;
            self.send("Page.navigateToHistoryEntry", json!({"entryId": entry_id})).await?;
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        Ok(())
    }

    // Reload page
    pub async fn reload(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.send("Page.reload", json!({})).await?;
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        Ok(())
    }

    pub async fn get_browser_state(&self) -> Result<BrowserState, Box<dyn std::error::Error + Send + Sync>> {
        let url = self.get_url().await?;
        let title = self.send("Runtime.evaluate", json!({"expression": "document.title"})).await?["result"]["value"].as_str().unwrap_or("").to_string();
        let screenshot = self.screenshot().await?;
        let tree = self.get_a11y_tree().await?;
        Ok(BrowserState { url, title, screenshot_base64: screenshot, accessibility_tree: tree })
    }

    pub async fn execute_llm_action(&self, action: &str, target: &Value, params: Option<&Value>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match action {
            "click" => {
                if let Some(s) = target.as_str() {
                    if s.starts_with("ax:") { self.click_ax(&s[3..]).await? }
                    else if s.starts_with("xpath:") { self.click_xpath(&s[6..]).await? }
                    else { self.click_element(s).await? }
                }
            }
            "double_click" => {
                if let Some(s) = target.as_str() {
                    let id = self.find_element(s).await?;
                    let b = self.get_bounds(id).await?;
                    self.double_click_at(b.x + b.width / 2.0, b.y + b.height / 2.0).await?;
                }
            }
            "right_click" => {
                if let Some(s) = target.as_str() {
                    let id = self.find_element(s).await?;
                    let b = self.get_bounds(id).await?;
                    self.right_click_at(b.x + b.width / 2.0, b.y + b.height / 2.0).await?;
                }
            }
            "hover" => {
                if let Some(s) = target.as_str() {
                    self.hover_element(s).await?;
                }
            }
            "type" => {
                let text = params.and_then(|p| p["text"].as_str()).ok_or("No text")?;
                if let Some(s) = target.as_str() {
                    if s.is_empty() {
                        // No target - type to currently focused element
                        self.type_text(text).await?;
                    } else if s.starts_with("ax:") {
                        // Click accessibility node first to focus, then type
                        self.click_ax(&s[3..]).await?;
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        self.type_text(text).await?;
                    } else if s.starts_with("xpath:") {
                        // Click XPath element first to focus, then type
                        self.click_xpath(&s[6..]).await?;
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        self.type_text(text).await?;
                    } else {
                        // CSS selector
                        self.type_into(s, text).await?;
                    }
                } else {
                    self.type_text(text).await?;
                }
            }
            "clear" => {
                if let Some(s) = target.as_str() {
                    if s.starts_with("ax:") {
                        self.click_ax(&s[3..]).await?;
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        // Select all and delete
                        self.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "a", "modifiers": 2})).await?;
                        self.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "a", "modifiers": 2})).await?;
                        self.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Backspace"})).await?;
                        self.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Backspace"})).await?;
                    } else {
                        self.clear_input(s).await?;
                    }
                }
            }
            "navigate" => {
                let url = params.and_then(|p| p["url"].as_str()).ok_or("No URL")?;
                self.navigate(url).await?;
            }
            "scroll" => {
                let dir = params.and_then(|p| p["direction"].as_str()).unwrap_or("down");
                let amt = params.and_then(|p| p["amount"].as_f64()).unwrap_or(300.0);
                self.scroll(if dir == "up" { -amt } else { amt }).await?;
            }
            "press_key" => {
                let key = params.and_then(|p| p["key"].as_str()).ok_or("No key")?;
                self.press_key(key).await?;
            }
            "focus_window" => {
                self.focus_window().await?;
            }
            "select" => {
                let value = params.and_then(|p| p["value"].as_str()).ok_or("No value")?;
                if let Some(s) = target.as_str() {
                    self.select_option(s, value).await?;
                }
            }
            "wait" => {
                let timeout = params.and_then(|p| p["timeout"].as_u64()).unwrap_or(5000);
                if let Some(s) = target.as_str() {
                    self.wait_for_element(s, timeout).await?;
                }
            }
            "go_back" => {
                self.go_back().await?;
            }
            "go_forward" => {
                self.go_forward().await?;
            }
            "reload" => {
                self.reload().await?;
            }
            "eval_js" => {
                let js = params.and_then(|p| p["code"].as_str()).ok_or("No code")?;
                self.eval_js(js).await?;
            }
            _ => return Err(format!("Unknown action: {}", action).into()),
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        Ok(())
    }
}