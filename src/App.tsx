import React, { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/tauri";
import { listen } from "@tauri-apps/api/event";

interface ActionCommand { action_type: string; target: any; params?: any; reasoning?: string; }
interface HistoryEntry {
  timestamp: string;
  step_number: number;
  user_input?: string;
  llm_reasoning: string;
  action: ActionCommand;
  success: boolean;
  error?: string;
  mode: string;
  window_context: string;
  input_tokens?: number;
  output_tokens?: number;
}

interface HistoryAnalysis {
  stats: {
    total_actions: number;
    successful_actions: number;
    failed_actions: number;
    success_rate: number;
    total_input_tokens: number;
    total_output_tokens: number;
    current_streak: number;
    longest_success_streak: number;
    most_used_action?: string;
    most_failed_action?: string;
    avg_tokens_per_action: number;
  };
  current_success_chain: { actions: Array<{ step: number; action_type: string; target_summary: string; reasoning_summary: string; }> };
  recent_failures: { actions: Array<{ step: number; action_type: string; target_summary: string; reasoning_summary: string; }> };
  logs: Array<{ timestamp: string; level: string; category: string; message: string; }>;
}

interface ProgressEvent {
  stage: "scanning" | "thinking" | "ready" | "executing" | "step";
  message: string;
  step?: number;
  action?: string;
}

export default function App() {
  const [apiKey, setApiKey] = useState("");
  const [apiKeySet, setApiKeySet] = useState(false);
  const [messages, setMessages] = useState<{role: string; content: string}[]>([]);
  const [input, setInput] = useState("");
  const [pendingAction, setPendingAction] = useState<ActionCommand | null>(null);
  const [status, setStatus] = useState<"idle"|"scanning"|"thinking"|"waiting"|"executing">("idle");
  const [progressMessage, setProgressMessage] = useState("");
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [historyAnalysis, setHistoryAnalysis] = useState<HistoryAnalysis | null>(null);
  const [autoApprove, setAutoApprove] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const [toast, setToast] = useState<{message: string; type: "success"|"error"|"info"} | null>(null);

  const showToast = (message: string, type: "success"|"error"|"info") => {
    setToast({message, type});
    setTimeout(() => setToast(null), 3000);
  };

  // Listen to progress events from Rust backend
  useEffect(() => {
    const unlisten = listen<ProgressEvent>("progress", (event) => {
      const { stage, message } = event.payload;
      setProgressMessage(message);

      if (stage === "scanning") {
        setStatus("scanning");
      } else if (stage === "thinking") {
        setStatus("thinking");
      } else if (stage === "ready") {
        // Will transition to waiting after action is set
      } else if (stage === "executing" || stage === "step") {
        setStatus("executing");
      }
    });

    return () => { unlisten.then(fn => fn()); };
  }, []);

  const handleScreenshot = async () => {
    try {
      await invoke("take_screenshot_to_clipboard");
      showToast("Screenshot copied!", "success");
    } catch (e: any) {
      showToast(`Error: ${e}`, "error");
    }
  };

  const handleA11yTree = async () => {
    try {
      const json: string = await invoke("get_screen_a11y_tree");
      await navigator.clipboard.writeText(json);
      showToast("A11y tree copied!", "success");
    } catch (e: any) {
      showToast(`Error: ${e}`, "error");
    }
  };

  useEffect(() => { loadApiKey(); }, []);

  const loadApiKey = async () => {
    try {
      const key: string | null = await invoke("load_api_key");
      if (key) { setApiKey(key); setApiKeySet(true); }
    } catch (e) { console.log("No saved API key"); }
  };

  const saveApiKey = async () => {
    try { await invoke("save_api_key", { key: apiKey }); setApiKeySet(true); }
    catch (e) { console.error("Failed:", e); }
  };

  const refreshHistory = async () => {
    try {
      const h: HistoryEntry[] = await invoke("get_history");
      setHistory(h);
      const analysis: HistoryAnalysis = await invoke("get_history_analysis");
      setHistoryAnalysis(analysis);
    } catch (e) {
      console.error("Failed to get history:", e);
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!input.trim()) return;
    const userMsg = input;
    setMessages(prev => [...prev, { role: "user", content: userMsg }]);
    setInput("");
    setStatus("scanning");
    setProgressMessage("Starting...");

    try {
      const action: ActionCommand = await invoke("execute_user_command", { command: userMsg });
      setMessages(prev => [...prev, { role: "assistant", content: `**Action:** ${action.action_type}\n**Target:** ${JSON.stringify(action.target)}\n**Reasoning:** ${action.reasoning || "N/A"}` }]);
      setPendingAction(action);
      setStatus("waiting");
      setProgressMessage("");
      if (autoApprove) await handleApprove(true);
    } catch (e: any) {
      setMessages(prev => [...prev, { role: "error", content: `Error: ${e}` }]);
      setStatus("idle");
      setProgressMessage("");
    }
  };

  const handleApprove = async (approved: boolean) => {
    if (!approved) {
      setMessages(prev => [...prev, { role: "system", content: "Action rejected." }]);
      setPendingAction(null);
      setStatus("idle");
      setProgressMessage("");
      return;
    }
    setStatus("executing");
    setProgressMessage("Executing...");

    try {
      const result: any = await invoke("approve_action", { approved: true });
      setMessages(prev => [...prev, { role: "system", content: result.success ? `Done! Window: ${result.active_window}` : `Failed: ${result.error}` }]);
      await refreshHistory();
    } catch (e: any) {
      setMessages(prev => [...prev, { role: "error", content: `Failed: ${e}` }]);
    }
    setPendingAction(null);
    setStatus("idle");
    setProgressMessage("");
  };

  // Status badge with progress message
  const StatusBadge = () => {
    const configs = {
      idle: { bg: "bg-gray-600", text: "Ready", animate: false },
      scanning: { bg: "bg-purple-600", text: "Scanning UI...", animate: true },
      thinking: { bg: "bg-yellow-600", text: "AI Thinking...", animate: true },
      waiting: { bg: "bg-orange-600", text: "Awaiting Approval", animate: false },
      executing: { bg: "bg-blue-600", text: "Executing...", animate: true },
    };
    const config = configs[status];

    return (
      <div className="flex items-center gap-2">
        <span className={`px-3 py-1 rounded-full text-xs ${config.bg} ${config.animate ? "animate-pulse" : ""}`}>
          {config.text}
        </span>
        {progressMessage && (
          <span className="text-xs text-gray-400">{progressMessage}</span>
        )}
      </div>
    );
  };

  if (!apiKeySet) {
    return (
      <div className="min-h-screen bg-gray-900 text-white flex items-center justify-center">
        <div className="bg-gray-800 p-8 rounded-lg max-w-md w-full">
          <h1 className="text-2xl font-bold mb-6 text-center">PC Automation Agent</h1>
          <p className="text-gray-400 mb-4">Enter your Claude API key:</p>
          <input type="password" value={apiKey} onChange={e => setApiKey(e.target.value)} placeholder="sk-ant-..." className="w-full p-3 rounded bg-gray-700 border border-gray-600 mb-4" />
          <button onClick={saveApiKey} disabled={!apiKey} className="w-full py-3 bg-blue-600 hover:bg-blue-700 rounded font-semibold disabled:opacity-50">Save & Continue</button>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gray-900 text-white flex">
      {showHistory && (
        <div className="w-96 bg-gray-800 border-r border-gray-700 p-4 overflow-y-auto">
          <div className="flex justify-between items-center mb-4">
            <h2 className="font-semibold">History & Logs</h2>
            <button onClick={async () => { await invoke("clear_history"); setHistory([]); setHistoryAnalysis(null); }} className="text-xs text-red-400 hover:text-red-300">Clear</button>
          </div>

          {/* Stats Summary */}
          {historyAnalysis && historyAnalysis.stats.total_actions > 0 && (
            <div className="bg-gray-700/50 rounded p-3 mb-4 text-xs">
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <span className="text-gray-400">Success Rate:</span>
                  <span className={`ml-1 font-bold ${historyAnalysis.stats.success_rate > 80 ? "text-green-400" : historyAnalysis.stats.success_rate > 50 ? "text-yellow-400" : "text-red-400"}`}>
                    {historyAnalysis.stats.success_rate.toFixed(1)}%
                  </span>
                </div>
                <div>
                  <span className="text-gray-400">Steps:</span>
                  <span className="ml-1">{historyAnalysis.stats.successful_actions}/{historyAnalysis.stats.total_actions}</span>
                </div>
                <div>
                  <span className="text-gray-400">Tokens:</span>
                  <span className="ml-1">{historyAnalysis.stats.total_input_tokens + historyAnalysis.stats.total_output_tokens}</span>
                </div>
                <div>
                  <span className="text-gray-400">Streak:</span>
                  <span className={`ml-1 ${historyAnalysis.stats.current_streak > 0 ? "text-green-400" : "text-red-400"}`}>
                    {historyAnalysis.stats.current_streak > 0 ? `+${historyAnalysis.stats.current_streak}` : historyAnalysis.stats.current_streak}
                  </span>
                </div>
              </div>
            </div>
          )}

          {/* Logs */}
          <div className="mb-4">
            <h3 className="text-xs font-semibold text-gray-400 mb-2">LOGS</h3>
            <div className="space-y-1 max-h-48 overflow-y-auto">
              {historyAnalysis?.logs.slice(-20).reverse().map((log, i) => (
                <div key={i} className={`text-xs p-1.5 rounded ${
                  log.level === "ERROR" ? "bg-red-900/30 text-red-300" :
                  log.level === "WARN" ? "bg-yellow-900/30 text-yellow-300" :
                  log.category === "LLM" ? "bg-blue-900/30 text-blue-300" :
                  log.category === "PERF" ? "bg-purple-900/30 text-purple-300" :
                  "bg-gray-700/30 text-gray-300"
                }`}>
                  <span className="text-gray-500">{new Date(log.timestamp).toLocaleTimeString()}</span>
                  <span className="ml-1 font-mono">[{log.category}]</span>
                  <span className="ml-1">{log.message}</span>
                </div>
              ))}
              {(!historyAnalysis?.logs || historyAnalysis.logs.length === 0) && (
                <div className="text-gray-500 text-xs">No logs yet</div>
              )}
            </div>
          </div>

          {/* Action History */}
          <div>
            <h3 className="text-xs font-semibold text-gray-400 mb-2">ACTIONS</h3>
            {history.slice(-10).reverse().map((h, i) => (
              <div key={i} className={`p-2 mb-2 rounded text-xs ${h.success ? "bg-green-900/30 border-l-2 border-green-500" : "bg-red-900/30 border-l-2 border-red-500"}`}>
                <div className="flex justify-between">
                  <span className="font-mono font-bold">Step {h.step_number}: {h.action.action_type}</span>
                  <span className="text-gray-500">{h.mode}</span>
                </div>
                <div className="text-gray-400 truncate">{JSON.stringify(h.action.target).slice(0, 40)}</div>
                {h.input_tokens && (
                  <div className="text-gray-500">{h.input_tokens}+{h.output_tokens} tokens</div>
                )}
                {h.error && <div className="text-red-400 truncate">{h.error}</div>}
              </div>
            ))}
            {history.length === 0 && <div className="text-gray-500 text-xs">No actions yet</div>}
          </div>
        </div>
      )}

      <div className="flex-1 flex flex-col">
        <div className="bg-gray-800 border-b border-gray-700 p-4 flex justify-between">
          <div className="flex items-center gap-4">
            <h1 className="text-xl font-bold">PC Automation Agent</h1>
            <StatusBadge />
          </div>
          <div className="flex items-center gap-4">
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={autoApprove} onChange={e => setAutoApprove(e.target.checked)} className="rounded" />
              Auto-approve
            </label>
            <button onClick={handleScreenshot} className="px-3 py-1 bg-gray-700 hover:bg-gray-600 rounded text-sm">Screenshot</button>
            <button onClick={handleA11yTree} className="px-3 py-1 bg-gray-700 hover:bg-gray-600 rounded text-sm">A11y Tree</button>
            <button onClick={() => { setShowHistory(!showHistory); if (!showHistory) refreshHistory(); }} className="px-3 py-1 bg-gray-700 hover:bg-gray-600 rounded text-sm">
              {showHistory ? "Hide" : "Show"} History
            </button>
          </div>
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {messages.length === 0 && (
            <div className="text-center text-gray-500 mt-8">
              <p className="text-lg">What would you like to automate?</p>
              <p className="text-sm mt-2">Example: "Open Google Chrome" or "Search for weather"</p>
            </div>
          )}
          {messages.map((m, i) => (
            <div key={i} className={`flex ${m.role === "user" ? "justify-end" : "justify-start"}`}>
              <div className={`max-w-2xl p-4 rounded-lg ${
                m.role === "user" ? "bg-blue-600" :
                m.role === "error" ? "bg-red-900" :
                m.role === "system" ? "bg-gray-700" :
                "bg-gray-800"
              }`}>
                <div className="text-xs text-gray-400 mb-1 uppercase">{m.role}</div>
                <div className="whitespace-pre-wrap text-sm">{m.content}</div>
              </div>
            </div>
          ))}

          {pendingAction && status === "waiting" && !autoApprove && (
            <div className="bg-yellow-900/30 border border-yellow-600 rounded-lg p-4">
              <h3 className="font-semibold text-yellow-400 mb-2">Pending Approval</h3>
              <div className="bg-gray-800 p-3 rounded mb-4 font-mono text-sm">
                <div><span className="text-gray-400">Type:</span> {pendingAction.action_type}</div>
                <div><span className="text-gray-400">Target:</span> {JSON.stringify(pendingAction.target)}</div>
                {pendingAction.params && <div><span className="text-gray-400">Params:</span> {JSON.stringify(pendingAction.params)}</div>}
              </div>
              <div className="flex gap-3">
                <button onClick={() => handleApprove(true)} className="px-4 py-2 bg-green-600 hover:bg-green-700 rounded font-semibold">Approve</button>
                <button onClick={() => handleApprove(false)} className="px-4 py-2 bg-red-600 hover:bg-red-700 rounded font-semibold">Reject</button>
              </div>
            </div>
          )}
        </div>

        <div className="border-t border-gray-700 p-4">
          <form onSubmit={handleSubmit} className="flex gap-3">
            <input
              type="text"
              value={input}
              onChange={e => setInput(e.target.value)}
              placeholder="Tell me what to automate..."
              disabled={status !== "idle" && status !== "waiting"}
              className="flex-1 p-3 rounded bg-gray-800 border border-gray-700 disabled:opacity-50 focus:border-blue-500 outline-none"
            />
            <button
              type="submit"
              disabled={(status !== "idle" && status !== "waiting") || !input.trim()}
              className="px-6 py-3 bg-blue-600 hover:bg-blue-700 rounded font-semibold disabled:opacity-50"
            >
              Send
            </button>
          </form>
        </div>
      </div>

      {toast && (
        <div className={`fixed bottom-4 right-4 px-4 py-2 rounded-lg text-sm shadow-lg ${
          toast.type === "success" ? "bg-green-600" :
          toast.type === "error" ? "bg-red-600" :
          "bg-blue-600"
        }`}>
          {toast.message}
        </div>
      )}
    </div>
  );
}
