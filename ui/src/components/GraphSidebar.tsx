import React, { useState, useRef, useCallback } from "react";
import type { GraphNode } from "./GraphCanvas.js";

interface SearchResult {
  file_path: string;
  heading: string;
  context_path: string;
  chunk_index: number;
  text: string;
  tags: string;
  importance_score: number;
  match_sources: ("semantic" | "bm25")[];
}

interface Neighbor {
  index: number;
  label: string;
  color: string;
  score: number;
}

interface Props {
  nodes: GraphNode[];
  selectedIndex: number | null;
  neighbors: Neighbor[];
  onSelectNode: (index: number | null) => void;
  onSearchResults: (filePaths: Set<string>) => void;
  onClearSearch: () => void;
  hasActiveSearch: boolean;
}

function truncate(text: string, max: number): string {
  const t = text.replace(/\s+/g, " ").trim();
  return t.length > max ? t.slice(0, max) + "…" : t;
}

function friendlyName(filePath: string): string {
  return filePath.replace(/\.[^.]+$/, "").replace(/\\/g, "/").split("/").pop() ?? filePath;
}

function formatDate(mtime: number): string {
  if (!mtime) return "Unknown";
  return new Date(mtime).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

function ResultCard({ filePath, chunks, onClickResult }: { filePath: string; chunks: SearchResult[]; onClickResult: (fp: string) => void }) {
  const sources = chunks[0].match_sources;
  const hasSemantic = sources.includes("semantic");
  const hasKeyword = sources.includes("bm25");

  return (
    <div className="card" style={{ marginBottom: 4, cursor: "pointer", padding: "6px 8px" }} onClick={() => onClickResult(filePath)}>
      <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
        <span style={{ fontWeight: 600, fontSize: 11, color: "var(--fg-1)", flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {friendlyName(filePath)}
        </span>
        <div style={{ display: "flex", gap: 3, flexShrink: 0 }}>
          {hasSemantic && <span className="badge" style={{ background: "var(--purple-glow)", color: "#fff", fontSize: 9, padding: "1px 4px" }} title="Semantic match">S</span>}
          {hasKeyword && <span className="badge" style={{ background: "var(--sem-ok)", color: "#fff", fontSize: 9, padding: "1px 4px" }} title="Keyword match">K</span>}
        </div>
      </div>
      {chunks.slice(0, 1).map((chunk, i) => (
        <div key={i} style={{ fontSize: 10, color: "var(--fg-3)", lineHeight: 1.4 }}>
          {chunk.context_path && <span style={{ color: "var(--fg-4)", fontWeight: 500 }}>{chunk.context_path} — </span>}
          <span>{truncate(chunk.text, 100)}</span>
        </div>
      ))}
    </div>
  );
}

export function GraphSidebar({ nodes, selectedIndex, neighbors, onSelectNode, onSearchResults, onClearSearch, hasActiveSearch }: Props) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [searchStatus, setSearchStatus] = useState("");
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastQueryRef = useRef("");

  const runSearch = useCallback(async (q: string) => {
    const trimmed = q.trim();
    lastQueryRef.current = trimmed;
    if (!trimmed) {
      setResults([]);
      setSearchStatus("");
      onClearSearch();
      return;
    }
    setSearchStatus("Searching…");
    try {
      const raw = await window.anamnesis.search(trimmed, 50);
      if (lastQueryRef.current !== trimmed) return;
      if (!Array.isArray(raw)) throw new Error((raw as { error?: string })?.error ?? "Unexpected response");
      const hits = raw as SearchResult[];
      setResults(hits);
      setSearchStatus(hits.length === 0 ? "No results." : `${hits.length} result${hits.length !== 1 ? "s" : ""}`);
      onSearchResults(new Set(hits.map((h) => h.file_path)));
    } catch (e) {
      if (lastQueryRef.current !== trimmed) return;
      setResults([]);
      setSearchStatus(`Error: ${String(e)}`);
      onClearSearch();
    }
  }, [onSearchResults, onClearSearch]);

  const onInput = (value: string) => {
    setQuery(value);
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (!value.trim()) {
      setResults([]);
      setSearchStatus("");
      onClearSearch();
      return;
    }
    debounceRef.current = setTimeout(() => void runSearch(value), 400);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      void runSearch(query);
    }
    if (e.key === "Escape") {
      setQuery("");
      setResults([]);
      setSearchStatus("");
      onClearSearch();
      onSelectNode(null);
    }
  };

  const onClickResult = (filePath: string) => {
    const idx = nodes.findIndex((n) => n.path === filePath || n.path.startsWith(filePath + ":"));
    if (idx >= 0) onSelectNode(idx);
  };

  const selectedNode = selectedIndex !== null ? nodes[selectedIndex] : null;

  // Group results by file
  const byFile = new Map<string, SearchResult[]>();
  for (const hit of results) {
    const arr = byFile.get(hit.file_path) ?? [];
    arr.push(hit);
    byFile.set(hit.file_path, arr);
  }

  return (
    <div className="graph-sidebar">
      <div className="graph-sidebar-search">
        <input
          className="folder-input"
          style={{ width: "100%", fontSize: 12, boxSizing: "border-box" }}
          placeholder="Search your vault…"
          value={query}
          onChange={(e) => onInput(e.target.value)}
          onKeyDown={onKeyDown}
        />
      </div>

      <div className="graph-sidebar-content">
        {selectedNode ? (
          <div className="graph-detail-card">
            <div className="graph-detail-back" onClick={() => onSelectNode(null)}>
              ← {hasActiveSearch ? "Back to results" : "Back"}
            </div>
            <div className="graph-detail-title">{selectedNode.label}</div>
            <div className="graph-detail-path" onClick={(e) => { e.stopPropagation(); void window.anamnesis.openFileFolder(selectedNode.path); }} title="Open in file explorer">
              {selectedNode.path}
            </div>
            <div className="graph-detail-meta">
              {selectedNode.tags && <span>Tags: {selectedNode.tags}</span>}
              <span>Modified: {formatDate(selectedNode.mtime)}</span>
              <span>Type: {selectedNode.ext || ".md"}</span>
            </div>
            <div className="graph-detail-snippet">{truncate(selectedNode.snippet, 200)}</div>

            {neighbors.length > 0 && (
              <>
                <div className="graph-neighbors-title">Nearest Neighbors</div>
                {neighbors.map((n) => (
                  <div key={n.index} className="graph-neighbor-row" onClick={() => onSelectNode(n.index)}>
                    <div className="graph-neighbor-dot" style={{ background: n.color }} />
                    <span className="graph-neighbor-label">{n.label}</span>
                    <span className="graph-neighbor-score">{n.score.toFixed(2)}</span>
                  </div>
                ))}
              </>
            )}
          </div>
        ) : (
          <>
            {searchStatus && <div className="graph-sidebar-status">{searchStatus}</div>}
            {results.length > 0 ? (
              [...byFile.entries()].map(([fp, chunks]) => (
                <ResultCard key={fp} filePath={fp} chunks={chunks} onClickResult={onClickResult} />
              ))
            ) : (
              !searchStatus && (
                <div className="graph-sidebar-hint">
                  {nodes.length > 0 ? "Search or click a node to explore" : "Index files to enable search"}
                </div>
              )
            )}
          </>
        )}
      </div>
    </div>
  );
}
