import React, { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/tauri";

interface ActionCommand { action_type: string; target: any; params?: any; reasoning?: string; }
interface HistoryEntry { timestamp: string; user_input?: string; llm_reasoning: string; action: ActionCommand; approved: boolean; success: boolean; }

export default function App() {
  const [apiKey, setApiKey] = useState("");
  const [apiKeySet, setApiKeySet] = useState(false);
  const [messages, setMessages] = useState<{role: string; content: string}[]>([]);
  const [input, setInput] = useState("");
  const [pendingAction, setPendingAction] = useState<ActionCommand | null>(null);
  const [status, setStatus] = useState<"idle"|"thinking"|"waiting"|"executing">("idle");
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [autoApprove, setAutoApprove] = useState(false);
  const [showHistory, setShowHistory] = useState(false);

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

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!input.trim()) return;
    const userMsg = input;
    setMessages(prev => [...prev, { role: "user", content: userMsg }]);
    setInput(""); setStatus("thinking");
    try {
      const action: ActionCommand = await invoke("execute_user_command", { command: userMsg });
      setMessages(prev => [...prev, { role: "assistant", content: `**Action:** ${action.action_type}\n**Target:** ${JSON.stringify(action.target)}\n**Reasoning:** ${action.reasoning || "N/A"}` }]);
      setPendingAction(action); setStatus("waiting");
      if (autoApprove) await handleApprove(true);
    } catch (e: any) { setMessages(prev => [...prev, { role: "error", content: `Error: ${e}` }]); setStatus("idle"); }
  };

  const handleApprove = async (approved: boolean) => {
    if (!approved) { setMessages(prev => [...prev, { role: "system", content: "Action rejected." }]); setPendingAction(null); setStatus("idle"); return; }
    setStatus("executing");
    try {
      const result: any = await invoke("approve_action", { approved: true });
      setMessages(prev => [...prev, { role: "system", content: result.success ? `Done! Window: ${result.active_window}` : `Failed: ${result.error}` }]);
      const h: HistoryEntry[] = await invoke("get_history"); setHistory(h);
    } catch (e: any) { setMessages(prev => [...prev, { role: "error", content: `Failed: ${e}` }]); }
    setPendingAction(null); setStatus("idle");
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
        <div className="w-80 bg-gray-800 border-r border-gray-700 p-4 overflow-y-auto">
          <div className="flex justify-between items-center mb-4">
            <h2 className="font-semibold">History</h2>
            <button onClick={async () => { await invoke("clear_history"); setHistory([]); }} className="text-xs text-red-400">Clear</button>
          </div>
          {history.slice(-10).reverse().map((h, i) => (
            <div key={i} className={`p-2 mb-2 rounded text-xs ${h.success ? "bg-green-900/30" : "bg-red-900/30"}`}>
              <div className="text-gray-400">{new Date(h.timestamp).toLocaleTimeString()}</div>
              <div className="font-mono">{h.action.action_type}</div>
              <div className={h.success ? "text-green-400" : "text-red-400"}>{h.success ? "Success" : "Failed"}</div>
            </div>
          ))}
        </div>
      )}
      <div className="flex-1 flex flex-col">
        <div className="bg-gray-800 border-b border-gray-700 p-4 flex justify-between">
          <div className="flex items-center gap-4">
            <h1 className="text-xl font-bold">PC Automation Agent</h1>
            <span className={`px-3 py-1 rounded-full text-xs ${status === "idle" ? "bg-gray-600" : status === "thinking" ? "bg-yellow-600 animate-pulse" : status === "waiting" ? "bg-orange-600" : "bg-blue-600 animate-pulse"}`}>
              {status === "idle" ? "Ready" : status === "thinking" ? "Thinking..." : status === "waiting" ? "Waiting" : "Executing..."}
            </span>
          </div>
          <div className="flex items-center gap-4">
            <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={autoApprove} onChange={e => setAutoApprove(e.target.checked)} />Auto-approve</label>
            <button onClick={() => setShowHistory(!showHistory)} className="px-3 py-1 bg-gray-700 rounded text-sm">{showHistory ? "Hide" : "Show"} History</button>
          </div>
        </div>
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {messages.length === 0 && <div className="text-center text-gray-500 mt-8"><p className="text-lg">What would you like to automate?</p><p className="text-sm mt-2">Example: "Navigate to google.com"</p></div>}
          {messages.map((m, i) => (
            <div key={i} className={`flex ${m.role === "user" ? "justify-end" : "justify-start"}`}>
              <div className={`max-w-2xl p-4 rounded-lg ${m.role === "user" ? "bg-blue-600" : m.role === "error" ? "bg-red-900" : m.role === "system" ? "bg-gray-700" : "bg-gray-800"}`}>
                <div className="text-xs text-gray-400 mb-1 uppercase">{m.role}</div>
                <div className="whitespace-pre-wrap text-sm">{m.content}</div>
              </div>
            </div>
          ))}
          {pendingAction && status === "waiting" && !autoApprove && (
            <div className="bg-yellow-900/30 border border-yellow-600 rounded-lg p-4">
              <h3 className="font-semibold text-yellow-400 mb-2">Pending Approval</h3>
              <div className="bg-gray-800 p-3 rounded mb-4 font-mono text-sm">
                <div>Type: {pendingAction.action_type}</div>
                <div>Target: {JSON.stringify(pendingAction.target)}</div>
                {pendingAction.params && <div>Params: {JSON.stringify(pendingAction.params)}</div>}
              </div>
              <div className="flex gap-3">
                <button onClick={() => handleApprove(true)} className="px-4 py-2 bg-green-600 rounded">Approve</button>
                <button onClick={() => handleApprove(false)} className="px-4 py-2 bg-red-600 rounded">Reject</button>
              </div>
            </div>
          )}
        </div>
        <div className="border-t border-gray-700 p-4">
          <form onSubmit={handleSubmit} className="flex gap-3">
            <input type="text" value={input} onChange={e => setInput(e.target.value)} placeholder="Tell me what to automate..." disabled={status !== "idle"} className="flex-1 p-3 rounded bg-gray-800 border border-gray-700 disabled:opacity-50" />
            <button type="submit" disabled={status !== "idle" || !input.trim()} className="px-6 py-3 bg-blue-600 rounded font-semibold disabled:opacity-50">Send</button>
          </form>
        </div>
      </div>
    </div>
  );
}
