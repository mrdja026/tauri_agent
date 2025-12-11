# PC Automation Agent

AI-powered desktop and browser automation using Tauri + React + Rust + Claude API.

## Table of Contents
- [Overview](#overview)
- [Architecture](#architecture)
- [Build Process](#build-process)
- [How It Works](#how-it-works)
- [Context Engineering](#context-engineering)
- [Windows UI Automation](#windows-ui-automation-windows_uirs)
- [Chrome CDP Automation](#chrome-cdp-automation-chrome_cdprs)
- [Known Issues & Flaws](#known-issues--flaws-ddx-peer-review)

---

## Overview

This application provides AI-driven automation of Windows desktop and Chrome browser. Users provide natural language commands (e.g., "open Chrome and search for cats"), and the AI agent:
1. Analyzes the current screen state via accessibility tree
2. Decides which action to take
3. Executes the action
4. Repeats until goal is achieved or auto-completion triggers

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      React Frontend                          │
│   (src/App.tsx - Command input, action approval, history)   │
└─────────────────────────┬───────────────────────────────────┘
                          │ Tauri IPC
┌─────────────────────────▼───────────────────────────────────┐
│                      Rust Backend                            │
│                     (src-tauri/src/)                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │  main.rs    │  │  claude.rs  │  │    logging.rs       │  │
│  │  (Core      │  │  (LLM API   │  │    (Structured      │  │
│  │   Loop)     │  │   + Prompt) │  │     Logging)        │  │
│  └──────┬──────┘  └─────────────┘  └─────────────────────┘  │
│         │                                                    │
│  ┌──────▼──────────────────────────────────────────────┐    │
│  │              Automation Layer                        │    │
│  │  ┌─────────────────┐    ┌─────────────────────┐     │    │
│  │  │  windows_ui.rs  │    │   chrome_cdp.rs     │     │    │
│  │  │  (Desktop Mode) │    │   (Browser Mode)    │     │    │
│  │  │  - UI Automation│    │   - CDP Protocol    │     │    │
│  │  │  - SendInput    │    │   - WebSocket       │     │    │
│  │  └─────────────────┘    └─────────────────────┘     │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## Build Process

### Prerequisites
- **Rust** (1.70+): https://rustup.rs/
- **Node.js** (18+): https://nodejs.org/
- **Visual Studio Build Tools** (Windows): C++ build tools for native dependencies

### Development Build
```bash
# Install frontend dependencies
npm install

# Run development server (hot-reload)
npm run tauri dev
```

### Production Build
```bash
# Build optimized binary
npm run tauri build

# Output: src-tauri/target/release/pc-automation-agent.exe
```

### Dependencies
Key Rust crates:
- `tauri` - Desktop app framework
- `windows` - Windows API bindings (UI Automation, SendInput)
- `tokio-tungstenite` - WebSocket for Chrome CDP
- `reqwest` - HTTP client for Claude API
- `xcap` - Screenshot capture
- `tracing` - Structured logging

---

## How It Works

### Main Execution Loop (`main.rs`)

```
User Input (goal)
    │
    ▼
┌─────────────────────────────────────────┐
│  1. Get Current State                    │
│     - Screenshot (disabled for perf)     │
│     - Accessibility Tree                 │
│     - Window Title, URL                  │
└─────────────────────┬───────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────┐
│  2. Call Claude API                      │
│     - Send: goal + state + history       │
│     - Receive: action JSON               │
└─────────────────────┬───────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────┐
│  3. Execute Action                       │
│     - click, type, press_key, etc.       │
│     - Wait 1.5s for UI to settle         │
│     - Get fresh state                    │
└─────────────────────┬───────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────┐
│  4. Check Completion                     │
│     - LLM returned "complete"? → Done    │
│     - 3 consecutive successes? →         │
│       Check goal keywords in a11y        │
│     - Max 20 steps? → Stop               │
└─────────────────────┬───────────────────┘
                      │
                      ▼
              Loop back to step 1
```

### Mode Detection
The system automatically detects which automation mode to use:
- **Browser Mode**: If Chrome with `--remote-debugging-port=9222` is accessible
- **Desktop Mode**: Falls back to Windows UI Automation

Mode can transition mid-execution (e.g., Desktop → Browser when Chrome opens).

---

## Context Engineering

The LLM prompt is carefully engineered to provide Claude with actionable context while minimizing token usage.

### System Prompt (`claude.rs`)

Key sections:
1. **Windows Environment Assumptions** - Taskbar position, common app locations
2. **Finding Apps Priority** - Taskbar pinned → Desktop icons → Start menu
3. **Execution Model** - When to use "complete" action
4. **Action Reference Table** - Available actions and parameters

### User Message Format

```
GOAL: {user's natural language goal}

STEP: {current step number}

STATE:
- Mode: BROWSER/DESKTOP
- Window: {active window title}
- URL: {current URL if browser}

INTERACTABLE ELEMENTS ({count} found):
- "Button Name" (Button) in [Parent] @ coords:x,y id:node_id
- "Edit Field" (Edit) in [Form] @ coords:x,y id:node_id
...

HISTORY:
[LEARNINGS FROM FAILURES]
- name:X not found, try coords
[RECENT FAILURES - avoid repeating]
Step 2: ✗ click -> "name:missing" | Element not found
[RECENT ACTIONS]
Step 1: ✓ click -> "coords:389,1416" [desktop] | Clicked Chrome

If goal is achieved, respond with: {"action_type":"complete",...}
Otherwise, next action JSON only.
```

### Token Optimization

1. **Interactable Elements Only** - Filters a11y tree to buttons, edits, links, etc.
2. **Truncated Names** - Element names capped at 50 chars
3. **Tiered History** - Recent 5 steps detailed, older summarized
4. **Learnings Extraction** - Extracts patterns from failures to avoid repetition

### Completion Detection

Three-tier completion system:
1. **LLM Signal** - Claude returns `{"action_type": "complete"}`
2. **Smart Complete** - After 3 successes, check if goal keywords appear in a11y
3. **Auto Complete** - Fallback after 3 successes if keywords not found (marked INCOMPLETE)

---

## Windows UI Automation (`windows_ui.rs`)

### Capabilities

| Category | Functions |
|----------|-----------|
| **Mouse** | `click_at`, `double_click_at`, `right_click_at`, `hover_at` |
| **Keyboard** | `type_text`, `press_key`, `press_key_combo` (e.g., Ctrl+A) |
| **Scroll** | `scroll` (up/down by delta) |
| **Windows** | `focus_window`, `get_window_title` |
| **A11y Tree** | `get_a11y_tree` - Full accessibility tree of focused window + taskbar |
| **Launch** | `launch_browser`, `launch_app`, `run_command` |
| **Screenshot** | `screenshot` - Base64 PNG of primary monitor |

### How It Works

1. **COM Initialization** - Uses Windows UI Automation COM interface
2. **Element Tree Walking** - `RawViewWalker` traverses UI elements
3. **Element Properties** - Extracts: RuntimeId, ControlType, Name, Value, BoundingRectangle
4. **Input Simulation** - Uses `SendInput` API for mouse/keyboard events

### Supported Control Types
Button, CheckBox, ComboBox, Edit, Hyperlink, ListItem, Menu, MenuItem,
RadioButton, Tab, TabItem, Text, ToolBar, Tree, TreeItem, Window, etc.

### Element Targeting
```rust
// By coordinates (most reliable)
click_at(389, 1416)

// By name (searches a11y tree)
click_by_name("Google Chrome pinned")

// By node_id (can be stale after UI changes)
click_element("42.123.456")
```

---

## Chrome CDP Automation (`chrome_cdp.rs`)

### Capabilities

| Category | Functions |
|----------|-----------|
| **Navigation** | `navigate`, `go_back`, `go_forward`, `reload` |
| **Mouse** | `click_at`, `double_click_at`, `right_click_at`, `hover_at` |
| **Keyboard** | `type_text`, `press_key`, `type_into` |
| **Elements** | `find_element` (CSS), `find_by_xpath`, `click_element` |
| **Forms** | `select_option`, `clear_input` |
| **State** | `get_a11y_tree`, `screenshot`, `get_url`, `get_text` |
| **Wait** | `wait_for_element` |
| **JavaScript** | `eval_js` |

### How It Works

1. **WebSocket Connection** - Connects to Chrome's debugging port (9222)
2. **CDP Protocol** - Sends JSON-RPC commands via WebSocket
3. **Domains Used**:
   - `Page` - Navigation, screenshots
   - `DOM` - Element queries
   - `Input` - Mouse/keyboard events
   - `Accessibility` - A11y tree
   - `Runtime` - JavaScript evaluation

### Element Targeting
```rust
// CSS Selector
click_element("input[name='q']")

// XPath
click_xpath("//button[contains(text(),'Search')]")

// Accessibility Node ID
click_ax("ax-node-123")

// Coordinates (viewport relative)
click_at(500.0, 300.0)
```

### Chrome Launch
Chrome must be started with debugging enabled:
```bash
chrome.exe --remote-debugging-port=9222
```
The app attempts to launch Chrome with this flag automatically.

---

## Known Issues & Flaws (DDX Peer Review)

### Critical Issues

1. **No Error Recovery on CDP Disconnect**
   - If Chrome closes mid-automation, the WebSocket connection dies
   - No reconnection logic; requires restart
   - *Fix*: Add reconnection with exponential backoff

2. **Race Condition in State Fetch**
   - `get_current_state_auto()` detects mode each time
   - Mode can switch between state fetch and action execution
   - *Fix*: Lock mode for duration of step

3. **Blocking UI Thread**
   - `std::thread::sleep` blocks in sync code paths
   - Can cause UI freezes during 1.5s waits
   - *Fix*: Use async `tokio::time::sleep` consistently

### Reliability Issues

4. **Stale Node IDs**
   - UI Automation RuntimeIds can change after any UI update
   - Clicking by node_id often fails after page changes
   - *Fix*: Prefer coordinates or re-query by name

5. **No Retry on Transient Failures**
   - Network timeouts to Claude API cause immediate failure
   - *Fix*: Add retry with exponential backoff for API calls

6. **Hardcoded Timeouts**
   - 1.5s wait after action may be too short for slow apps
   - 2s wait for Chrome launch may be insufficient
   - *Fix*: Make configurable or use dynamic wait (poll for change)

### Security Issues

7. **API Key Storage**
   - Stored in plaintext JSON in config directory
   - *Fix*: Use OS keychain (Windows Credential Manager)

8. **No Input Sanitization for `run_command`**
   - Passes user input to shell via `cmd /C start`
   - Potential command injection if goal contains shell metacharacters
   - *Fix*: Sanitize or whitelist allowed commands

### Performance Issues

9. **Full A11y Tree Per Step**
   - Fetches entire accessibility tree on every action
   - Can be slow (2+ seconds) for complex UIs
   - *Fix*: Cache and diff, or fetch only changed subtrees

10. **No Token Budget Management**
    - If a11y tree is huge, may exceed Claude's context
    - Currently truncates to 100 elements arbitrarily
    - *Fix*: Dynamic truncation based on token estimate

### UX Issues

11. **No Cancel Button**
    - Once automation starts, user can't stop it
    - *Fix*: Add abort signal/cancellation token

12. **Opaque Failures**
    - "Element not found" doesn't say which element or why
    - *Fix*: Include element details and nearby alternatives in error

13. **Auto-Complete May Be Premature**
    - 3 steps threshold may complete before goal is actually done
    - Keyword matching is naive (substring match)
    - *Fix*: Increase threshold or improve goal verification

### Code Quality Issues

14. **Duplicate `truncate` Functions**
    - Same function in `claude.rs` and `logging.rs`
    - *Fix*: Extract to shared utility module

15. **Large Functions**
    - `approve_action` is 200+ lines
    - `execute_llm_action` has massive match statement
    - *Fix*: Break into smaller, testable functions

16. **No Unit Tests**
    - Zero test coverage
    - *Fix*: Add tests for keyword extraction, truncation, parsing

17. **Inconsistent Error Handling**
    - Mix of `Result`, `Option`, `.unwrap()`, `.ok()`
    - Some errors silently ignored
    - *Fix*: Consistent error propagation with context

### Missing Features

18. **No Multi-Monitor Support**
    - Screenshots and coordinates assume single monitor
    - *Fix*: Detect monitor and adjust coordinates

19. **No Undo/History Navigation**
    - Can't go back to previous state if action was wrong
    - *Fix*: Save state snapshots for rollback

20. **No Keyboard Shortcuts**
    - All interaction via mouse in UI
    - *Fix*: Add hotkeys (Escape to cancel, Enter to approve)

---

## Usage

1. Start the app: `npm run tauri dev`
2. Enter your Claude API key (Settings)
3. Type a command: "Open Chrome and search for weather"
4. Review the proposed action
5. Click Approve to execute
6. Repeat until goal is achieved

## License

MIT
