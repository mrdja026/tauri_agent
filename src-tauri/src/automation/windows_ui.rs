// Stub file - Windows UI Automation disabled for now
// We're focusing on browser automation only

pub struct WindowsAutomation;

impl WindowsAutomation {
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Err("Windows UI Automation is disabled".into())
    }
}