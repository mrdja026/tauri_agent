# PC Automation Agent

AI-powered desktop automation using Tauri + React + Rust.

## Setup

1. Install prerequisites: Rust, Node.js, VS Build Tools
2. Run: `npm install`
3. Run: `npm run tauri dev`

## Important

You need to manually copy 2 large Rust files from the Claude chat:
- `src-tauri/src/automation/chrome_cdp.rs` - from artifact "chrome-cdp-full"
- `src-tauri/src/automation/windows_ui.rs` - from artifact "windows-ui-automation"

## Usage

1. Enter your Claude API key
2. Type natural language commands like "Navigate to google.com"
3. Approve actions before execution
