use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(unused_imports)]
use std::mem::zeroed;

#[cfg(target_os = "windows")]
#[allow(unused_imports)]
use windows::{
    core::{BSTR, PCWSTR},
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED},
            Variant::VARIANT,
            Ole::{SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetElement},
        },
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
                UIA_BoundingRectanglePropertyId, UIA_ControlTypePropertyId,
                UIA_IsKeyboardFocusablePropertyId, UIA_NamePropertyId,
                UIA_ValueValuePropertyId, IUIAutomationTextPattern, UIA_TextPatternId,
            },
            Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_MOUSE, INPUT_KEYBOARD,
                MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN,
                MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
                VIRTUAL_KEY, VK_RETURN, VK_TAB, VK_ESCAPE, VK_BACK, VK_DELETE,
                VK_UP, VK_DOWN, VK_LEFT, VK_RIGHT, VK_HOME, VK_END,
                VK_CONTROL, VK_SHIFT, VK_MENU, VK_SPACE,
            },
            WindowsAndMessaging::{
                GetForegroundWindow, GetWindowTextW, SetForegroundWindow, SetCursorPos,
                FindWindowW,
            },
        },
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AXNode {
    pub node_id: String,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub text: Option<String>,
    pub bounds: Option<Bounds>,
    pub focusable: bool,
    pub is_leaf: bool,
    pub children: Vec<AXNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopState {
    pub window_title: String,
    pub screenshot_base64: String,
    pub accessibility_tree: Vec<AXNode>,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(target_os = "windows")]
pub struct WindowsAutomation {
    automation: IUIAutomation,
}

#[cfg(not(target_os = "windows"))]
pub struct WindowsAutomation;

#[cfg(not(target_os = "windows"))]
impl WindowsAutomation {
    pub fn new() -> Result<Self, BoxError> {
        Err("Windows UI Automation is only available on Windows".into())
    }
}

#[cfg(target_os = "windows")]
impl WindowsAutomation {
    pub fn new() -> Result<Self, BoxError> {
        unsafe {
            // Initialize COM
            CoInitializeEx(None, COINIT_MULTITHREADED).ok();

            // Create UI Automation instance
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;

            Ok(Self { automation })
        }
    }

    /// Get the currently focused window's HWND
    pub fn get_focused_hwnd(&self) -> HWND {
        unsafe { GetForegroundWindow() }
    }

    /// Get the focused window as a UI Automation element
    pub fn get_focused_window(&self) -> Result<IUIAutomationElement, BoxError> {
        unsafe {
            let hwnd = self.get_focused_hwnd();
            let element = self.automation.ElementFromHandle(hwnd)?;
            Ok(element)
        }
    }

    /// Get window title of the focused window
    pub fn get_window_title(&self) -> Result<String, BoxError> {
        unsafe {
            let hwnd = self.get_focused_hwnd();
            let mut buffer = [0u16; 512];
            let len = GetWindowTextW(hwnd, &mut buffer);
            if len > 0 {
                Ok(String::from_utf16_lossy(&buffer[..len as usize]))
            } else {
                Ok(String::new())
            }
        }
    }

    /// Build accessibility tree from focused window + taskbar only (fast scan)
    pub fn get_a11y_tree(&self) -> Result<Vec<AXNode>, BoxError> {
        unsafe {
            let walker = self.automation.RawViewWalker()?;
            let mut nodes = Vec::new();

            // 1. Get focused window tree
            let focused_hwnd = self.get_focused_hwnd();
            if focused_hwnd.0 != 0 {
                if let Ok(focused_element) = self.automation.ElementFromHandle(focused_hwnd) {
                    self.walk_tree(&focused_element, &walker, &mut nodes)?;
                }
            }

            // 2. Get taskbar tree (for pinned apps, start button, tray)
            // Taskbar class name is "Shell_TrayWnd"
            let taskbar_class: Vec<u16> = "Shell_TrayWnd\0".encode_utf16().collect();
            let taskbar_hwnd = FindWindowW(PCWSTR(taskbar_class.as_ptr()), PCWSTR::null());
            if taskbar_hwnd.0 != 0 {
                if let Ok(taskbar_element) = self.automation.ElementFromHandle(taskbar_hwnd) {
                    self.walk_tree(&taskbar_element, &walker, &mut nodes)?;
                }
            }

            Ok(nodes)
        }
    }

    /// Recursively walk the UI tree
    unsafe fn walk_tree(
        &self,
        element: &IUIAutomationElement,
        walker: &IUIAutomationTreeWalker,
        nodes: &mut Vec<AXNode>,
    ) -> Result<(), BoxError> {
        let node = self.element_to_axnode(element)?;

        // Get children
        let mut children = Vec::new();
        if let Ok(first_child) = walker.GetFirstChildElement(element) {
            self.walk_children(&first_child, walker, &mut children)?;
        }

        let mut node = node;
        node.is_leaf = children.is_empty();
        node.children = children;

        // For leaf nodes, try to extract text
        if node.is_leaf {
            node.text = self.extract_text(element).ok().flatten();
        }

        nodes.push(node);
        Ok(())
    }

    /// Walk sibling children
    unsafe fn walk_children(
        &self,
        element: &IUIAutomationElement,
        walker: &IUIAutomationTreeWalker,
        children: &mut Vec<AXNode>,
    ) -> Result<(), BoxError> {
        let mut current = Some(element.clone());

        while let Some(ref elem) = current {
            let mut node = self.element_to_axnode(elem)?;

            // Get grandchildren
            let mut grandchildren = Vec::new();
            if let Ok(first_child) = walker.GetFirstChildElement(elem) {
                self.walk_children(&first_child, walker, &mut grandchildren)?;
            }

            node.is_leaf = grandchildren.is_empty();
            node.children = grandchildren;

            // For leaf nodes, try to extract text
            if node.is_leaf {
                node.text = self.extract_text(elem).ok().flatten();
            }

            children.push(node);

            // Move to next sibling
            current = walker.GetNextSiblingElement(elem).ok();
        }

        Ok(())
    }

    /// Convert a UI Automation element to an AXNode
    unsafe fn element_to_axnode(&self, element: &IUIAutomationElement) -> Result<AXNode, BoxError> {
        // Get RuntimeId as node_id - RuntimeId is a SAFEARRAY of i32
        let runtime_id_ptr = element.GetRuntimeId()?;
        let node_id = if !runtime_id_ptr.is_null() {
            // Convert SAFEARRAY to a string representation
            let lbound = SafeArrayGetLBound(runtime_id_ptr, 1).unwrap_or(0);
            let ubound = SafeArrayGetUBound(runtime_id_ptr, 1).unwrap_or(-1);

            let mut ids = Vec::new();
            for i in lbound..=ubound {
                let mut val: i32 = 0;
                if SafeArrayGetElement(runtime_id_ptr, &i, &mut val as *mut i32 as *mut _).is_ok() {
                    ids.push(val.to_string());
                }
            }
            ids.join(".")
        } else {
            String::from("unknown")
        };

        // Get ControlType as role
        let control_type = get_variant_i32(&element.GetCurrentPropertyValue(UIA_ControlTypePropertyId)?);
        let role = control_type_to_string(control_type);

        // Get Name
        let name = get_variant_string(&element.GetCurrentPropertyValue(UIA_NamePropertyId).unwrap_or_default());

        // Get Value
        let value = get_variant_string(&element.GetCurrentPropertyValue(UIA_ValueValuePropertyId).unwrap_or_default());

        // Get BoundingRectangle - comes as array of doubles [x, y, width, height]
        let bounds = get_variant_rect(&element.GetCurrentPropertyValue(UIA_BoundingRectanglePropertyId).unwrap_or_default());

        // Get IsKeyboardFocusable
        let focusable = get_variant_bool(&element.GetCurrentPropertyValue(UIA_IsKeyboardFocusablePropertyId).unwrap_or_default());

        Ok(AXNode {
            node_id,
            role,
            name,
            value,
            text: None, // Will be filled for leaf nodes
            bounds,
            focusable,
            is_leaf: false, // Will be updated after checking children
            children: Vec::new(),
        })
    }

    /// Extract text content from element (for leaf nodes)
    unsafe fn extract_text(&self, element: &IUIAutomationElement) -> Result<Option<String>, BoxError> {
        // Try TextPattern first (for rich text controls)
        if let Ok(pattern) = element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) {
            if let Ok(range) = pattern.DocumentRange() {
                if let Ok(text) = range.GetText(-1) {
                    let s = text.to_string();
                    if !s.is_empty() {
                        return Ok(Some(s));
                    }
                }
            }
        }

        // Fall back to Value property
        if let Ok(v) = element.GetCurrentPropertyValue(UIA_ValueValuePropertyId) {
            if let Some(s) = get_variant_string(&v) {
                if !s.is_empty() {
                    return Ok(Some(s));
                }
            }
        }

        // Fall back to Name property
        if let Ok(v) = element.GetCurrentPropertyValue(UIA_NamePropertyId) {
            if let Some(s) = get_variant_string(&v) {
                if !s.is_empty() {
                    return Ok(Some(s));
                }
            }
        }

        Ok(None)
    }

    // ==================== Mouse Operations ====================

    /// Move mouse to absolute screen coordinates
    pub fn move_mouse(&self, x: i32, y: i32) -> Result<(), BoxError> {
        unsafe {
            if SetCursorPos(x, y).is_ok() {
                Ok(())
            } else {
                Err("Failed to move cursor".into())
            }
        }
    }

    /// Click at absolute screen coordinates
    pub fn click_at(&self, x: i32, y: i32) -> Result<(), BoxError> {
        self.move_mouse(x, y)?;
        std::thread::sleep(std::time::Duration::from_millis(50));

        unsafe {
            let mut inputs: [INPUT; 2] = zeroed();

            // Mouse down
            inputs[0].r#type = INPUT_MOUSE;
            inputs[0].Anonymous.mi.dwFlags = MOUSEEVENTF_LEFTDOWN;

            // Mouse up
            inputs[1].r#type = INPUT_MOUSE;
            inputs[1].Anonymous.mi.dwFlags = MOUSEEVENTF_LEFTUP;

            let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            if sent == 2 {
                Ok(())
            } else {
                Err("Failed to send click input".into())
            }
        }
    }

    /// Double click at absolute screen coordinates
    pub fn double_click_at(&self, x: i32, y: i32) -> Result<(), BoxError> {
        self.click_at(x, y)?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.click_at(x, y)?;
        Ok(())
    }

    /// Right click at absolute screen coordinates
    pub fn right_click_at(&self, x: i32, y: i32) -> Result<(), BoxError> {
        self.move_mouse(x, y)?;
        std::thread::sleep(std::time::Duration::from_millis(50));

        unsafe {
            let mut inputs: [INPUT; 2] = zeroed();

            inputs[0].r#type = INPUT_MOUSE;
            inputs[0].Anonymous.mi.dwFlags = MOUSEEVENTF_RIGHTDOWN;

            inputs[1].r#type = INPUT_MOUSE;
            inputs[1].Anonymous.mi.dwFlags = MOUSEEVENTF_RIGHTUP;

            let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            if sent == 2 {
                Ok(())
            } else {
                Err("Failed to send right click input".into())
            }
        }
    }

    /// Hover at coordinates (move mouse without clicking)
    pub fn hover_at(&self, x: i32, y: i32) -> Result<(), BoxError> {
        self.move_mouse(x, y)?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        Ok(())
    }

    /// Find element by node_id in the tree and click its center
    pub fn click_element(&self, node_id: &str) -> Result<(), BoxError> {
        let tree = self.get_a11y_tree()?;
        if let Some(node) = find_node_by_id(&tree, node_id) {
            if let Some(bounds) = &node.bounds {
                let cx = (bounds.x + bounds.width / 2.0) as i32;
                let cy = (bounds.y + bounds.height / 2.0) as i32;
                self.click_at(cx, cy)
            } else {
                Err("Element has no bounds".into())
            }
        } else {
            Err(format!("Element not found: {}", node_id).into())
        }
    }

    /// Find element by name and click it
    pub fn click_by_name(&self, name: &str) -> Result<(), BoxError> {
        let tree = self.get_a11y_tree()?;
        if let Some(node) = find_node_by_name(&tree, name) {
            if let Some(bounds) = &node.bounds {
                let cx = (bounds.x + bounds.width / 2.0) as i32;
                let cy = (bounds.y + bounds.height / 2.0) as i32;
                self.click_at(cx, cy)
            } else {
                Err("Element has no bounds".into())
            }
        } else {
            Err(format!("Element not found by name: {}", name).into())
        }
    }

    // ==================== Keyboard Operations ====================

    /// Type a text string (Unicode)
    pub fn type_text(&self, text: &str) -> Result<(), BoxError> {
        for c in text.chars() {
            self.type_char(c)?;
        }
        Ok(())
    }

    /// Type a single character
    fn type_char(&self, c: char) -> Result<(), BoxError> {
        unsafe {
            let mut inputs: [INPUT; 2] = zeroed();

            // Key down
            inputs[0].r#type = INPUT_KEYBOARD;
            inputs[0].Anonymous.ki.wScan = c as u16;
            inputs[0].Anonymous.ki.dwFlags = KEYEVENTF_UNICODE;

            // Key up
            inputs[1].r#type = INPUT_KEYBOARD;
            inputs[1].Anonymous.ki.wScan = c as u16;
            inputs[1].Anonymous.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;

            let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            if sent == 2 {
                Ok(())
            } else {
                Err("Failed to type character".into())
            }
        }
    }

    /// Press a special key by name
    pub fn press_key(&self, key: &str) -> Result<(), BoxError> {
        let vk = key_name_to_vk(key)?;
        self.press_vk(vk)
    }

    /// Press a virtual key
    fn press_vk(&self, vk: VIRTUAL_KEY) -> Result<(), BoxError> {
        unsafe {
            let mut inputs: [INPUT; 2] = zeroed();

            // Key down
            inputs[0].r#type = INPUT_KEYBOARD;
            inputs[0].Anonymous.ki.wVk = vk;

            // Key up
            inputs[1].r#type = INPUT_KEYBOARD;
            inputs[1].Anonymous.ki.wVk = vk;
            inputs[1].Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;

            let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            if sent == 2 {
                Ok(())
            } else {
                Err("Failed to press key".into())
            }
        }
    }

    /// Press a key combination (e.g., Ctrl+A)
    pub fn press_key_combo(&self, modifiers: &[&str], key: &str) -> Result<(), BoxError> {
        // Press modifiers down
        for m in modifiers {
            let vk = modifier_to_vk(m)?;
            self.key_down(vk)?;
        }

        // Press the main key
        let vk = key_name_to_vk(key)?;
        self.press_vk(vk)?;

        // Release modifiers
        for m in modifiers.iter().rev() {
            let vk = modifier_to_vk(m)?;
            self.key_up(vk)?;
        }

        Ok(())
    }

    fn key_down(&self, vk: VIRTUAL_KEY) -> Result<(), BoxError> {
        unsafe {
            let mut input: INPUT = zeroed();
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki.wVk = vk;

            let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            if sent == 1 { Ok(()) } else { Err("Failed to key down".into()) }
        }
    }

    fn key_up(&self, vk: VIRTUAL_KEY) -> Result<(), BoxError> {
        unsafe {
            let mut input: INPUT = zeroed();
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki.wVk = vk;
            input.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;

            let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            if sent == 1 { Ok(()) } else { Err("Failed to key up".into()) }
        }
    }

    // ==================== Scroll Operations ====================

    /// Scroll by delta (positive = up, negative = down)
    pub fn scroll(&self, delta_y: i32) -> Result<(), BoxError> {
        unsafe {
            let mut input: INPUT = zeroed();
            input.r#type = INPUT_MOUSE;
            input.Anonymous.mi.dwFlags = MOUSEEVENTF_WHEEL;
            input.Anonymous.mi.mouseData = delta_y as u32;

            let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            if sent == 1 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                Ok(())
            } else {
                Err("Failed to scroll".into())
            }
        }
    }

    // ==================== Application Launch ====================

    /// Try to launch a browser (Chrome, Edge, Firefox) with remote debugging enabled
    /// This allows CDP (Chrome DevTools Protocol) to connect for better automation
    pub fn launch_browser(&self, url: Option<&str>) -> Result<String, BoxError> {
        const DEBUG_PORT: u16 = 9222;

        let browsers = [
            // Chrome paths - supports --remote-debugging-port
            (r"C:\Program Files\Google\Chrome\Application\chrome.exe", "Chrome", true),
            (r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe", "Chrome", true),
            // Edge paths - supports --remote-debugging-port (Chromium-based)
            (r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe", "Edge", true),
            (r"C:\Program Files\Microsoft\Edge\Application\msedge.exe", "Edge", true),
            // Firefox paths - uses different debugging protocol, no CDP flag
            (r"C:\Program Files\Mozilla Firefox\firefox.exe", "Firefox", false),
            (r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe", "Firefox", false),
        ];

        for (path, name, supports_cdp) in browsers {
            if std::path::Path::new(path).exists() {
                let mut cmd = std::process::Command::new(path);

                // Add remote debugging flag for Chromium-based browsers
                if supports_cdp {
                    cmd.arg(format!("--remote-debugging-port={}", DEBUG_PORT));
                }

                if let Some(u) = url {
                    cmd.arg(u);
                }

                match cmd.spawn() {
                    Ok(_) => {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        let debug_info = if supports_cdp {
                            format!(" (CDP enabled on port {})", DEBUG_PORT)
                        } else {
                            String::new()
                        };
                        return Ok(format!("Launched {}{}", name, debug_info));
                    }
                    Err(_) => continue,
                }
            }
        }

        Err("No browser found. Tried Chrome, Edge, Firefox in standard locations.".into())
    }

    /// Launch a specific application by name or path
    /// Browsers (Chrome, Edge) are launched with remote debugging enabled for CDP
    pub fn launch_app(&self, app: &str, args: Option<&[&str]>) -> Result<(), BoxError> {
        const DEBUG_PORT: u16 = 9222;

        // Check if this is a browser that supports CDP
        let (app_path, is_chromium_browser) = match app.to_lowercase().as_str() {
            "chrome" | "google chrome" => (self.find_app(&[
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            ]), true),
            "edge" | "msedge" | "microsoft edge" => (self.find_app(&[
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            ]), true),
            "firefox" | "mozilla firefox" => (self.find_app(&[
                r"C:\Program Files\Mozilla Firefox\firefox.exe",
                r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
            ]), false),
            "notepad" => (Some(r"C:\Windows\System32\notepad.exe".to_string()), false),
            "explorer" | "file explorer" => (Some(r"C:\Windows\explorer.exe".to_string()), false),
            "cmd" | "command prompt" => (Some(r"C:\Windows\System32\cmd.exe".to_string()), false),
            "powershell" => (Some(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".to_string()), false),
            "calc" | "calculator" => (Some(r"C:\Windows\System32\calc.exe".to_string()), false),
            // If not a known name, treat as path
            _ => {
                if std::path::Path::new(app).exists() {
                    (Some(app.to_string()), false)
                } else {
                    (None, false)
                }
            }
        };

        let path = app_path.ok_or(format!("Application not found: {}", app))?;

        let mut cmd = std::process::Command::new(&path);

        // Add remote debugging for Chromium browsers
        if is_chromium_browser {
            cmd.arg(format!("--remote-debugging-port={}", DEBUG_PORT));
        }

        if let Some(a) = args {
            cmd.args(a);
        }

        cmd.spawn().map_err(|e| format!("Failed to launch {}: {}", path, e))?;
        std::thread::sleep(std::time::Duration::from_secs(1));
        Ok(())
    }

    /// Find first existing path from list
    fn find_app(&self, paths: &[&str]) -> Option<String> {
        paths.iter()
            .find(|p| std::path::Path::new(p).exists())
            .map(|s| s.to_string())
    }

    /// Run a command via shell (like Win+R)
    pub fn run_command(&self, command: &str) -> Result<(), BoxError> {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", command])
            .spawn()
            .map_err(|e| format!("Failed to run command: {}", e))?;

        std::thread::sleep(std::time::Duration::from_secs(1));
        Ok(())
    }

    // ==================== Window Operations ====================

    /// Bring the focused window to front
    pub fn focus_window(&self) -> Result<(), BoxError> {
        unsafe {
            let hwnd = self.get_focused_hwnd();
            // SetForegroundWindow returns BOOL, which is non-zero on success
            if SetForegroundWindow(hwnd).as_bool() {
                Ok(())
            } else {
                Err("Failed to focus window".into())
            }
        }
    }

    // ==================== Screenshot ====================

    /// Capture screenshot of the primary monitor
    pub fn screenshot(&self) -> Result<String, BoxError> {
        use xcap::Monitor;
        use xcap::image::ImageFormat;
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let monitors = Monitor::all().map_err(|e| format!("Monitor error: {}", e))?;
        let monitor = monitors.first().ok_or("No monitor found")?;
        let img = monitor.capture_image().map_err(|e| format!("Capture error: {}", e))?;

        let mut buffer = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png)
            .map_err(|e| format!("Image encode error: {}", e))?;

        Ok(STANDARD.encode(&buffer))
    }

    // ==================== Combined State ====================

    /// Get full desktop state (window info + accessibility tree)
    /// Screenshot disabled for performance - enable via get_desktop_state_with_screenshot()
    pub fn get_desktop_state(&self) -> Result<DesktopState, BoxError> {
        let window_title = self.get_window_title()?;
        // Screenshot disabled for performance
        let screenshot_base64 = String::new();
        let accessibility_tree = self.get_a11y_tree()?;

        Ok(DesktopState {
            window_title,
            screenshot_base64,
            accessibility_tree,
        })
    }

    // ==================== LLM Action Executor ====================

    /// Execute an action from LLM (mirrors chrome_cdp interface)
    pub fn execute_llm_action(
        &self,
        action: &str,
        target: &Value,
        params: Option<&Value>,
    ) -> Result<(), BoxError> {
        match action {
            "click" => {
                if let Some(s) = target.as_str() {
                    if s.starts_with("coords:") {
                        let coords: Vec<&str> = s[7..].split(',').collect();
                        if coords.len() == 2 {
                            let x: i32 = coords[0].trim().parse()?;
                            let y: i32 = coords[1].trim().parse()?;
                            self.click_at(x, y)?;
                        }
                    } else if s.starts_with("name:") {
                        self.click_by_name(&s[5..])?;
                    } else {
                        self.click_element(s)?;
                    }
                }
            }
            "double_click" => {
                if let Some(s) = target.as_str() {
                    if s.starts_with("coords:") {
                        let coords: Vec<&str> = s[7..].split(',').collect();
                        if coords.len() == 2 {
                            let x: i32 = coords[0].trim().parse()?;
                            let y: i32 = coords[1].trim().parse()?;
                            self.double_click_at(x, y)?;
                        }
                    } else if s.starts_with("name:") {
                        let tree = self.get_a11y_tree()?;
                        if let Some(node) = find_node_by_name(&tree, &s[5..]) {
                            if let Some(bounds) = &node.bounds {
                                let cx = (bounds.x + bounds.width / 2.0) as i32;
                                let cy = (bounds.y + bounds.height / 2.0) as i32;
                                self.double_click_at(cx, cy)?;
                            }
                        } else {
                            return Err(format!("Element not found by name: {}", &s[5..]).into());
                        }
                    } else {
                        // Find element by node_id and double click
                        let tree = self.get_a11y_tree()?;
                        if let Some(node) = find_node_by_id(&tree, s) {
                            if let Some(bounds) = &node.bounds {
                                let cx = (bounds.x + bounds.width / 2.0) as i32;
                                let cy = (bounds.y + bounds.height / 2.0) as i32;
                                self.double_click_at(cx, cy)?;
                            }
                        } else {
                            return Err(format!("Element not found: {}", s).into());
                        }
                    }
                }
            }
            "right_click" => {
                if let Some(s) = target.as_str() {
                    if s.starts_with("coords:") {
                        let coords: Vec<&str> = s[7..].split(',').collect();
                        if coords.len() == 2 {
                            let x: i32 = coords[0].trim().parse()?;
                            let y: i32 = coords[1].trim().parse()?;
                            self.right_click_at(x, y)?;
                        }
                    } else if s.starts_with("name:") {
                        let tree = self.get_a11y_tree()?;
                        if let Some(node) = find_node_by_name(&tree, &s[5..]) {
                            if let Some(bounds) = &node.bounds {
                                let cx = (bounds.x + bounds.width / 2.0) as i32;
                                let cy = (bounds.y + bounds.height / 2.0) as i32;
                                self.right_click_at(cx, cy)?;
                            }
                        } else {
                            return Err(format!("Element not found by name: {}", &s[5..]).into());
                        }
                    } else {
                        let tree = self.get_a11y_tree()?;
                        if let Some(node) = find_node_by_id(&tree, s) {
                            if let Some(bounds) = &node.bounds {
                                let cx = (bounds.x + bounds.width / 2.0) as i32;
                                let cy = (bounds.y + bounds.height / 2.0) as i32;
                                self.right_click_at(cx, cy)?;
                            }
                        } else {
                            return Err(format!("Element not found: {}", s).into());
                        }
                    }
                }
            }
            "hover" => {
                if let Some(s) = target.as_str() {
                    if s.starts_with("coords:") {
                        let coords: Vec<&str> = s[7..].split(',').collect();
                        if coords.len() == 2 {
                            let x: i32 = coords[0].trim().parse()?;
                            let y: i32 = coords[1].trim().parse()?;
                            self.hover_at(x, y)?;
                        }
                    } else if s.starts_with("name:") {
                        let tree = self.get_a11y_tree()?;
                        if let Some(node) = find_node_by_name(&tree, &s[5..]) {
                            if let Some(bounds) = &node.bounds {
                                let cx = (bounds.x + bounds.width / 2.0) as i32;
                                let cy = (bounds.y + bounds.height / 2.0) as i32;
                                self.hover_at(cx, cy)?;
                            }
                        } else {
                            return Err(format!("Element not found by name: {}", &s[5..]).into());
                        }
                    } else {
                        let tree = self.get_a11y_tree()?;
                        if let Some(node) = find_node_by_id(&tree, s) {
                            if let Some(bounds) = &node.bounds {
                                let cx = (bounds.x + bounds.width / 2.0) as i32;
                                let cy = (bounds.y + bounds.height / 2.0) as i32;
                                self.hover_at(cx, cy)?;
                            }
                        } else {
                            return Err(format!("Element not found: {}", s).into());
                        }
                    }
                }
            }
            "type" => {
                let text = params.and_then(|p| p["text"].as_str()).ok_or("No text param")?;
                // If target is specified, click it first
                if let Some(s) = target.as_str() {
                    if !s.is_empty() {
                        if s.starts_with("name:") {
                            self.click_by_name(&s[5..])?;
                        } else if !s.starts_with("coords:") {
                            self.click_element(s)?;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
                self.type_text(text)?;
            }
            "clear" => {
                // Select all and delete
                if let Some(s) = target.as_str() {
                    if s.starts_with("name:") {
                        self.click_by_name(&s[5..])?;
                    } else if !s.is_empty() {
                        self.click_element(s)?;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                self.press_key_combo(&["Ctrl"], "a")?;
                std::thread::sleep(std::time::Duration::from_millis(50));
                self.press_key("Backspace")?;
            }
            "press_key" => {
                let key = params.and_then(|p| p["key"].as_str()).ok_or("No key param")?;
                self.press_key(key)?;
            }
            "scroll" => {
                let direction = params.and_then(|p| p["direction"].as_str()).unwrap_or("down");
                let amount = params.and_then(|p| p["amount"].as_i64()).unwrap_or(300) as i32;
                let delta = if direction == "up" { amount } else { -amount };
                self.scroll(delta)?;
            }
            "focus_window" => {
                self.focus_window()?;
            }
            // Desktop-specific actions for launching applications
            "launch_browser" => {
                // Launch first available browser (Chrome, Edge, Firefox)
                // Optionally with a URL
                let url = params.and_then(|p| p["url"].as_str());
                self.launch_browser(url)?;
            }
            "launch" | "launch_app" => {
                // Launch a specific application by name or path
                let app = params.and_then(|p| p["app"].as_str())
                    .or_else(|| target.as_str())
                    .ok_or("No app specified. Use params.app or target.")?;

                // Collect args if provided
                let args: Option<Vec<&str>> = params
                    .and_then(|p| p["args"].as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect());

                self.launch_app(app, args.as_deref())?;
            }
            "run" | "run_command" => {
                // Run a command like Win+R (uses shell)
                let cmd = params.and_then(|p| p["command"].as_str())
                    .or_else(|| target.as_str())
                    .ok_or("No command specified. Use params.command or target.")?;
                self.run_command(cmd)?;
            }
            // Goal completion action - no-op, just signals done
            "complete" => {
                // This is a signal action - nothing to execute
                // The main loop handles this specially
            }
            // Browser-only actions - return clear error in desktop mode
            "navigate" => {
                return Err("'navigate' action is only available in browser mode. Use Chrome with debugging port.".into());
            }
            "select" => {
                return Err("'select' action is only available in browser mode (for HTML <select> elements).".into());
            }
            "wait" => {
                return Err("'wait' action is only available in browser mode. In desktop mode, elements are always present in the accessibility tree.".into());
            }
            "go_back" => {
                return Err("'go_back' action is only available in browser mode.".into());
            }
            "go_forward" => {
                return Err("'go_forward' action is only available in browser mode.".into());
            }
            "reload" => {
                return Err("'reload' action is only available in browser mode.".into());
            }
            "eval_js" => {
                return Err("'eval_js' action is only available in browser mode.".into());
            }
            _ => return Err(format!("Unknown action: {}", action).into()),
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
        Ok(())
    }
}

// ==================== Helper Functions ====================

#[cfg(target_os = "windows")]
fn get_variant_string(v: &VARIANT) -> Option<String> {
    use windows::Win32::System::Variant::VT_BSTR;
    unsafe {
        // Check if variant contains a BSTR
        if v.Anonymous.Anonymous.vt == VT_BSTR {
            let bstr = &v.Anonymous.Anonymous.Anonymous.bstrVal;
            if !bstr.is_empty() {
                return Some(bstr.to_string());
            }
        }
        None
    }
}

#[cfg(target_os = "windows")]
fn get_variant_i32(v: &VARIANT) -> i32 {
    use windows::Win32::System::Variant::{VT_I4, VT_I2, VT_UI4};
    unsafe {
        match v.Anonymous.Anonymous.vt {
            VT_I4 => v.Anonymous.Anonymous.Anonymous.lVal,
            VT_I2 => v.Anonymous.Anonymous.Anonymous.iVal as i32,
            VT_UI4 => v.Anonymous.Anonymous.Anonymous.ulVal as i32,
            _ => 0,
        }
    }
}

#[cfg(target_os = "windows")]
fn get_variant_bool(v: &VARIANT) -> bool {
    use windows::Win32::System::Variant::VT_BOOL;
    unsafe {
        if v.Anonymous.Anonymous.vt == VT_BOOL {
            v.Anonymous.Anonymous.Anonymous.boolVal.as_bool()
        } else {
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn get_variant_rect(v: &VARIANT) -> Option<Bounds> {
    use windows::Win32::System::Variant::VT_ARRAY;
    unsafe {
        // BoundingRectangle comes as a SAFEARRAY of doubles (VT_ARRAY | VT_R8)
        let vt = v.Anonymous.Anonymous.vt.0;
        if (vt & VT_ARRAY.0) != 0 {
            let parray = v.Anonymous.Anonymous.Anonymous.parray;
            if !parray.is_null() {
                let lbound = SafeArrayGetLBound(parray, 1).unwrap_or(0);
                let ubound = SafeArrayGetUBound(parray, 1).unwrap_or(-1);

                if ubound - lbound >= 3 {
                    let mut arr = [0.0f64; 4];
                    for (idx, i) in (lbound..=lbound + 3).enumerate() {
                        let mut val: f64 = 0.0;
                        let _ = SafeArrayGetElement(parray, &i, &mut val as *mut f64 as *mut _);
                        arr[idx] = val;
                    }
                    return Some(Bounds {
                        x: arr[0],
                        y: arr[1],
                        width: arr[2],
                        height: arr[3],
                    });
                }
            }
        }
        None
    }
}

#[cfg(target_os = "windows")]
fn control_type_to_string(ct: i32) -> String {
    match ct {
        50000 => "Button".to_string(),
        50001 => "Calendar".to_string(),
        50002 => "CheckBox".to_string(),
        50003 => "ComboBox".to_string(),
        50004 => "Edit".to_string(),
        50005 => "Hyperlink".to_string(),
        50006 => "Image".to_string(),
        50007 => "ListItem".to_string(),
        50008 => "List".to_string(),
        50009 => "Menu".to_string(),
        50010 => "MenuBar".to_string(),
        50011 => "MenuItem".to_string(),
        50012 => "ProgressBar".to_string(),
        50013 => "RadioButton".to_string(),
        50014 => "ScrollBar".to_string(),
        50015 => "Slider".to_string(),
        50016 => "Spinner".to_string(),
        50017 => "StatusBar".to_string(),
        50018 => "Tab".to_string(),
        50019 => "TabItem".to_string(),
        50020 => "Text".to_string(),
        50021 => "ToolBar".to_string(),
        50022 => "ToolTip".to_string(),
        50023 => "Tree".to_string(),
        50024 => "TreeItem".to_string(),
        50025 => "Custom".to_string(),
        50026 => "Group".to_string(),
        50027 => "Thumb".to_string(),
        50028 => "DataGrid".to_string(),
        50029 => "DataItem".to_string(),
        50030 => "Document".to_string(),
        50031 => "SplitButton".to_string(),
        50032 => "Window".to_string(),
        50033 => "Pane".to_string(),
        50034 => "Header".to_string(),
        50035 => "HeaderItem".to_string(),
        50036 => "Table".to_string(),
        50037 => "TitleBar".to_string(),
        50038 => "Separator".to_string(),
        _ => format!("Unknown({})", ct),
    }
}

#[cfg(target_os = "windows")]
fn key_name_to_vk(key: &str) -> Result<VIRTUAL_KEY, BoxError> {
    match key.to_lowercase().as_str() {
        "enter" | "return" => Ok(VK_RETURN),
        "tab" => Ok(VK_TAB),
        "escape" | "esc" => Ok(VK_ESCAPE),
        "backspace" => Ok(VK_BACK),
        "delete" | "del" => Ok(VK_DELETE),
        "up" | "arrowup" => Ok(VK_UP),
        "down" | "arrowdown" => Ok(VK_DOWN),
        "left" | "arrowleft" => Ok(VK_LEFT),
        "right" | "arrowright" => Ok(VK_RIGHT),
        "home" => Ok(VK_HOME),
        "end" => Ok(VK_END),
        "space" => Ok(VK_SPACE),
        // Single letters
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap().to_ascii_uppercase();
            Ok(VIRTUAL_KEY(c as u16))
        }
        _ => Err(format!("Unknown key: {}", key).into()),
    }
}

#[cfg(target_os = "windows")]
fn modifier_to_vk(modifier: &str) -> Result<VIRTUAL_KEY, BoxError> {
    match modifier.to_lowercase().as_str() {
        "ctrl" | "control" => Ok(VK_CONTROL),
        "shift" => Ok(VK_SHIFT),
        "alt" => Ok(VK_MENU),
        _ => Err(format!("Unknown modifier: {}", modifier).into()),
    }
}

/// Find a node by its node_id in the tree
fn find_node_by_id<'a>(nodes: &'a [AXNode], id: &str) -> Option<&'a AXNode> {
    for node in nodes {
        if node.node_id == id {
            return Some(node);
        }
        if let Some(found) = find_node_by_id(&node.children, id) {
            return Some(found);
        }
    }
    None
}

/// Find a node by its name in the tree
fn find_node_by_name<'a>(nodes: &'a [AXNode], name: &str) -> Option<&'a AXNode> {
    for node in nodes {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        if let Some(found) = find_node_by_name(&node.children, name) {
            return Some(found);
        }
    }
    None
}
