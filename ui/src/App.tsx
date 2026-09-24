import React, { useState, useEffect, Component, type ReactNode, type ErrorInfo } from "react";
import { Dashboard } from "./components/Dashboard.js";
import { GraphPanel } from "./components/GraphPanel.js";
import { Settings } from "./components/Settings.js";

class ErrorBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  constructor(props: { children: ReactNode }) {
    super(props);
    this.state = { error: null };
  }
  static getDerivedStateFromError(err: unknown) {
    return { error: String(err) };
  }
  componentDidCatch(err: Error, info: ErrorInfo) {
    console.error("[Anamnesis] Render error:", err, info.componentStack);
  }
  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 16 }}>
          <div style={{ color: "var(--color-error, #e05)", fontWeight: 600, marginBottom: 8 }}>Render error</div>
          <pre style={{ fontSize: 11, color: "var(--text-faint)", whiteSpace: "pre-wrap", wordBreak: "break-all" }}>{this.state.error}</pre>
          <button className="btn" style={{ marginTop: 8 }} onClick={() => this.setState({ error: null })}>Dismiss</button>
        </div>
      );
    }
    return this.props.children;
  }
}

type Tab = "dashboard" | "graph" | "settings";

interface StatusPayload {
  indexStatus?: { state?: string };
  chunkCount?: number;
}

function headerDotClass(payload: StatusPayload): string {
  const s = payload.indexStatus?.state;
  if (!s || s === "idle") return "idle";
  return s;
}

export function App() {
  const [tab, setTab] = useState<Tab>("dashboard");
  const [status, setStatus] = useState<StatusPayload>({});

  useEffect(() => {
    document.documentElement.dataset.platform = window.anamnesis.platform;
  }, []);

  useEffect(() => {
    void (async () => {
      try { setStatus(await window.anamnesis.getStatus() as StatusPayload); } catch { /* ignore */ }
    })();
    const unsub = window.anamnesis.onStatusUpdate((p) => setStatus(p as StatusPayload));
    return () => { unsub(); };
  }, []);

  return (
    <div className="app">
      <div className="header">
        <div className={`header-status-dot ${headerDotClass(status)}`} />
        <span className="header-title">Anamnesis</span>
      </div>

      <div className="tabs">
        <div className={`tab${tab === "dashboard" ? " active" : ""}`} onClick={() => setTab("dashboard")}>Dashboard</div>
        <div className={`tab${tab === "graph" ? " active" : ""}`} onClick={() => setTab("graph")}>Vector Graph</div>
        <div className={`tab${tab === "settings" ? " active" : ""}`} onClick={() => setTab("settings")}>Settings</div>
      </div>

      {/* Tabs are hidden via CSS rather than unmounted so state (e.g. built graph) persists */}
      <div style={{ display: tab === "dashboard" ? "contents" : "none" }}>
        <div className="scroll">
          <ErrorBoundary><Dashboard /></ErrorBoundary>
        </div>
      </div>
      <div style={{ display: tab === "graph" ? "contents" : "none" }}>
        <ErrorBoundary><GraphPanel chunkCount={status.chunkCount ?? 0} /></ErrorBoundary>
      </div>
      <div style={{ display: tab === "settings" ? "contents" : "none" }}>
        <ErrorBoundary><Settings /></ErrorBoundary>
      </div>
    </div>
  );
}
