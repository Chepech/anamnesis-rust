import React, { useRef, useState, useEffect, useCallback, useMemo } from "react";
import { UMAP } from "umap-js";
import { GraphCanvas, type GraphNode, type GraphEdge, type RenderMode } from "./GraphCanvas.js";
import { GraphSidebar } from "./GraphSidebar.js";

const K_NEIGHBORS = 5;
const K_EDGES = 5;

// ── Color utilities ──────────────────────────────────────────────────────────

type ColorMode = "folder" | "filetype" | "tags" | "recency";

const FILE_TYPE_COLORS: Record<string, string> = {
  ".md": "hsl(270, 60%, 60%)",
  ".pdf": "hsl(0, 60%, 55%)",
  ".docx": "hsl(210, 60%, 55%)",
  ".html": "hsl(30, 70%, 55%)",
  ".htm": "hsl(30, 70%, 55%)",
};
const FILE_TYPE_DEFAULT = "hsl(0, 0%, 50%)";

const TAG_PALETTE_HUES = [270, 200, 330, 150, 40, 290, 180, 60, 350, 120];

function buildHueFamilyColors(nodes: GraphNode[], commonDepth: number): { colorMap: Map<number, string>; legend: { vault: string; vaultColor: string; subs: { name: string; color: string }[] }[] } {
  const vaultSubfolders = new Map<string, Set<string>>();
  const nodeVaultSub = new Map<number, { vault: string; sub: string }>();

  for (let i = 0; i < nodes.length; i++) {
    const parts = nodes[i].path.replace(/\\/g, "/").split("/").filter(Boolean);
    const vault = parts[commonDepth] ?? "root";
    const sub = parts[commonDepth + 1] ?? "(root)";
    if (!vaultSubfolders.has(vault)) vaultSubfolders.set(vault, new Set());
    vaultSubfolders.get(vault)!.add(sub);
    nodeVaultSub.set(i, { vault, sub });
  }

  const vaults = [...vaultSubfolders.keys()].sort();
  const vaultHues = new Map<string, number>();
  const hueStep = 360 / Math.max(vaults.length, 1);
  vaults.forEach((v, i) => vaultHues.set(v, (220 + i * hueStep) % 360));

  const colorMap = new Map<number, string>();
  const legend: { vault: string; vaultColor: string; subs: { name: string; color: string }[] }[] = [];

  for (const vault of vaults) {
    const hue = vaultHues.get(vault)!;
    const subs = [...vaultSubfolders.get(vault)!].sort();
    const subColors = new Map<string, string>();

    const count = subs.length;
    subs.forEach((sub, i) => {
      const saturation = 45 + (35 * i / Math.max(count - 1, 1));
      const lightness = 40 + (30 * i / Math.max(count - 1, 1));
      const color = `hsl(${hue}, ${saturation.toFixed(0)}%, ${lightness.toFixed(0)}%)`;
      subColors.set(sub, color);
    });

    const vaultColor = `hsl(${hue}, 60%, 55%)`;
    legend.push({
      vault,
      vaultColor,
      subs: subs.filter((s) => s !== "(root)").map((s) => ({ name: s, color: subColors.get(s)! })),
    });

    for (let i = 0; i < nodes.length; i++) {
      const vs = nodeVaultSub.get(i);
      if (vs && vs.vault === vault) {
        colorMap.set(i, subColors.get(vs.sub) ?? vaultColor);
      }
    }
  }

  return { colorMap, legend };
}

function computeCommonDepth(nodes: GraphNode[]): number {
  const allParts = nodes.map((n) => n.path.replace(/\\/g, "/").split("/").filter(Boolean));
  let commonDepth = 0;
  if (allParts.length > 1) {
    const minLen = Math.min(...allParts.map((p) => p.length));
    for (let d = 0; d < minLen; d++) {
      if (allParts.every((p) => p[d] === allParts[0][d])) commonDepth = d + 1;
      else break;
    }
  }
  return commonDepth;
}

function applyColorMode(nodes: GraphNode[], mode: ColorMode, commonDepth: number): { nodes: GraphNode[]; legend: { vault: string; vaultColor: string; subs: { name: string; color: string }[] }[] } {
  if (mode === "folder") {
    const { colorMap, legend } = buildHueFamilyColors(nodes, commonDepth);
    const colored = nodes.map((n, i) => ({ ...n, color: colorMap.get(i) ?? n.color }));
    return { nodes: colored, legend };
  }

  if (mode === "filetype") {
    const legend: { vault: string; vaultColor: string; subs: { name: string; color: string }[] }[] = [{
      vault: "File Types", vaultColor: "var(--fg-3)",
      subs: Object.entries(FILE_TYPE_COLORS).map(([ext, color]) => ({ name: ext, color })),
    }];
    const colored = nodes.map((n) => ({ ...n, color: FILE_TYPE_COLORS[n.ext] ?? FILE_TYPE_DEFAULT }));
    return { nodes: colored, legend };
  }

  if (mode === "tags") {
    const tagColors = new Map<string, string>();
    let idx = 0;
    const colored = nodes.map((n) => {
      const firstTag = n.tags.split(",")[0]?.trim() || "";
      if (!firstTag) return { ...n, color: "hsl(0, 0%, 40%)" };
      if (!tagColors.has(firstTag)) {
        const hue = TAG_PALETTE_HUES[idx % TAG_PALETTE_HUES.length];
        tagColors.set(firstTag, `hsl(${hue}, 55%, 55%)`);
        idx++;
      }
      return { ...n, color: tagColors.get(firstTag)! };
    });
    const legend: { vault: string; vaultColor: string; subs: { name: string; color: string }[] }[] = [{
      vault: "Tags", vaultColor: "var(--fg-3)",
      subs: [...tagColors.entries()].map(([tag, color]) => ({ name: tag, color })),
    }];
    return { nodes: colored, legend };
  }

  // recency
  const mtimes = nodes.map((n) => n.mtime).filter((m) => m > 0);
  const minT = Math.min(...mtimes, 0);
  const maxT = Math.max(...mtimes, 1);
  const range = maxT - minT || 1;
  const colored = nodes.map((n) => {
    const t = (n.mtime - minT) / range;
    const h = 20;
    const s = 20 + t * 50;
    const l = 30 + t * 30;
    return { ...n, color: `hsl(${h}, ${s.toFixed(0)}%, ${l.toFixed(0)}%)` };
  });
  const legend: { vault: string; vaultColor: string; subs: { name: string; color: string }[] }[] = [{
    vault: "Recency", vaultColor: "var(--fg-3)",
    subs: [{ name: "Old", color: "hsl(20, 20%, 30%)" }, { name: "Recent", color: "hsl(20, 70%, 60%)" }],
  }];
  return { nodes: colored, legend };
}

// ── Cosine similarity ────────────────────────────────────────────────────────

function cosine(a: number[], b: number[]): number {
  let dot = 0, na = 0, nb = 0;
  for (let i = 0; i < a.length; i++) { dot += a[i] * b[i]; na += a[i] * a[i]; nb += b[i] * b[i]; }
  return na === 0 || nb === 0 ? 0 : dot / (Math.sqrt(na) * Math.sqrt(nb));
}

function knnEdges(vecs: number[][], k: number): GraphEdge[] {
  const n = vecs.length;
  const seen = new Set<string>();
  const edges: GraphEdge[] = [];
  for (let i = 0; i < n; i++) {
    const sims: { j: number; sim: number }[] = [];
    for (let j = 0; j < n; j++) { if (i !== j) sims.push({ j, sim: cosine(vecs[i], vecs[j]) }); }
    sims.sort((a, b) => b.sim - a.sim);
    for (const { j, sim } of sims.slice(0, k)) {
      const key = i < j ? `${i}-${j}` : `${j}-${i}`;
      if (!seen.has(key) && sim > 0) { seen.add(key); edges.push({ a: i, b: j, strength: sim }); }
    }
  }
  return edges;
}

function getNeighbors(nodes: GraphNode[], selectedIndex: number, k: number): { index: number; score: number }[] {
  const vec = nodes[selectedIndex].vector;
  const sims: { index: number; score: number }[] = [];
  for (let i = 0; i < nodes.length; i++) {
    if (i === selectedIndex) continue;
    sims.push({ index: i, score: cosine(vec, nodes[i].vector) });
  }
  sims.sort((a, b) => b.score - a.score);
  return sims.slice(0, k);
}

// ── Main component ───────────────────────────────────────────────────────────

interface RawNode { id: string; vector: number[]; text: string; tags: string; last_modified: number; }

export function GraphPanel({ chunkCount }: { chunkCount: number }) {
  const [status, setStatus] = useState("Click Rebuild to load graph.");
  const [building, setBuilding] = useState(false);
  const [colorMode, setColorMode] = useState<ColorMode>("folder");

  const [rawNodes, setRawNodes] = useState<GraphNode[]>([]);
  const [coloredNodes, setColoredNodes] = useState<GraphNode[]>([]);
  const [edges, setEdges] = useState<GraphEdge[]>([]);
  const [legend, setLegend] = useState<{ vault: string; vaultColor: string; subs: { name: string; color: string }[] }[]>([]);
  const [commonDepth, setCommonDepth] = useState(0);

  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  const [highlightSet, setHighlightSet] = useState<Set<number>>(new Set());
  const [neighborSet, setNeighborSet] = useState<Set<number>>(new Set());
  const [searchFilePaths, setSearchFilePaths] = useState<Set<string>>(new Set());
  const [panTarget, setPanTarget] = useState<{ x: number; y: number; zoom: number } | null>(null);

  const autoBuiltRef = useRef(false);

  const renderMode: RenderMode = selectedIndex !== null ? "selected" : highlightSet.size > 0 ? "search" : "idle";

  // Apply color mode when rawNodes or colorMode changes
  useEffect(() => {
    if (rawNodes.length === 0) return;
    const { nodes: colored, legend: newLegend } = applyColorMode(rawNodes, colorMode, commonDepth);
    setColoredNodes(colored);
    setLegend(newLegend);
  }, [rawNodes, colorMode, commonDepth]);

  // Compute neighbors when selection changes
  useEffect(() => {
    if (selectedIndex === null || coloredNodes.length === 0) {
      setNeighborSet(new Set());
      return;
    }
    const nbs = getNeighbors(coloredNodes, selectedIndex, K_NEIGHBORS);
    setNeighborSet(new Set(nbs.map((n) => n.index)));
  }, [selectedIndex, coloredNodes]);

  // Map search results to highlight set
  useEffect(() => {
    if (searchFilePaths.size === 0) {
      setHighlightSet(new Set());
      return;
    }
    const set = new Set<number>();
    for (let i = 0; i < coloredNodes.length; i++) {
      if (searchFilePaths.has(coloredNodes[i].path)) set.add(i);
    }
    setHighlightSet(set);
  }, [searchFilePaths, coloredNodes]);

  const neighbors = useMemo(() => {
    if (selectedIndex === null || coloredNodes.length === 0) return [];
    return getNeighbors(coloredNodes, selectedIndex, K_NEIGHBORS).map((n) => ({
      index: n.index,
      label: coloredNodes[n.index].label,
      color: coloredNodes[n.index].color,
      score: n.score,
    }));
  }, [selectedIndex, coloredNodes]);

  const handleNodeClick = useCallback((index: number | null) => {
    if (index === null) {
      setSelectedIndex(null);
      return;
    }
    setSelectedIndex(index);
    const node = coloredNodes[index];
    if (node) setPanTarget({ x: node.x, y: node.y, zoom: 2.0 });
  }, [coloredNodes]);

  const handleNodeHover = useCallback((_index: number | null) => {
    // Currently no state needed for hover — canvas handles it internally
  }, []);

  const handleSearchResults = useCallback((filePaths: Set<string>) => {
    setSearchFilePaths(filePaths);
    setSelectedIndex(null);
  }, []);

  const handleClearSearch = useCallback(() => {
    setSearchFilePaths(new Set());
    setHighlightSet(new Set());
  }, []);

  const handleSelectFromSidebar = useCallback((index: number | null) => {
    if (index === null) {
      setSelectedIndex(null);
      return;
    }
    setSelectedIndex(index);
    const node = coloredNodes[index];
    if (node) setPanTarget({ x: node.x, y: node.y, zoom: 2.0 });
  }, [coloredNodes]);

  const handlePanComplete = useCallback(() => {
    setPanTarget(null);
  }, []);

  const buildGraph = useCallback(async () => {
    setBuilding(true);
    setStatus("Fetching vectors…");
    setRawNodes([]); setColoredNodes([]); setEdges([]);
    setSelectedIndex(null); setHighlightSet(new Set()); setNeighborSet(new Set());

    try {
      const response = await window.anamnesis.getVectors() as { nodes: RawNode[] };
      const data = response.nodes ?? response;
      if (!Array.isArray(data) || data.length === 0) { setStatus("No indexed files yet — run re-index first."); setBuilding(false); return; }

      setStatus(`Computing layout for ${data.length} notes…`);
      const vecs = data.map((n: RawNode) => n.vector);

      const builtRaw: GraphNode[] = data.map((n: RawNode) => {
        const parts = n.id.replace(/\\/g, "/").split("/").filter(Boolean);
        const ext = "." + (parts[parts.length - 1]?.split(".").pop() ?? "md");
        return {
          x: 0, y: 0,
          path: n.id,
          label: n.id.replace(/\.[^.]+$/, "").split("/").pop() ?? n.id,
          snippet: n.text,
          color: "",
          folder: "",
          ext,
          tags: n.tags ?? "",
          mtime: n.last_modified ?? 0,
          vector: n.vector,
        };
      });

      const depth = computeCommonDepth(builtRaw);
      setCommonDepth(depth);

      // Assign folder field
      for (const node of builtRaw) {
        const parts = node.path.replace(/\\/g, "/").split("/").filter(Boolean);
        node.folder = (parts[depth] ?? "root") + "/" + (parts[depth + 1] ?? "");
      }

      const nNeighbors = Math.min(15, Math.max(2, data.length - 1));
      const umap = new UMAP({ nComponents: 2, nEpochs: 300, nNeighbors, minDist: 0.05 });

      const coords = await umap.fitAsync(vecs, (epoch: number) => {
        if (epoch % 30 === 0) setStatus(`Layout ${Math.round((epoch / 300) * 100)}%…`);
      });

      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      for (const [x, y] of coords) { if (x < minX) minX = x; if (x > maxX) maxX = x; if (y < minY) minY = y; if (y > maxY) maxY = y; }
      const rX = maxX - minX || 1, rY = maxY - minY || 1;

      for (let i = 0; i < builtRaw.length; i++) {
        builtRaw[i].x = (coords[i][0] - minX) / rX;
        builtRaw[i].y = (coords[i][1] - minY) / rY;
      }

      setStatus("Computing edges…");
      const builtEdges = knnEdges(vecs, K_EDGES);

      setRawNodes(builtRaw);
      setEdges(builtEdges);
      setStatus(`${data.length} notes · ${builtEdges.length} edges — scroll to zoom, drag to pan, click to explore`);
    } catch (err) {
      setStatus(`Error: ${String(err)}`);
    } finally {
      setBuilding(false);
    }
  }, []);

  // Auto-build once on mount if vectors exist
  useEffect(() => {
    if (chunkCount > 0 && !autoBuiltRef.current && rawNodes.length === 0 && !building) {
      autoBuiltRef.current = true;
      void buildGraph();
    }
  }, [chunkCount, rawNodes.length, building, buildGraph]);

  return (
    <div className="graph-panel">
      <div className="graph-top-bar">
        <span className="graph-status">{status}</span>
        <select
          className="select-input"
          style={{ flex: "none", width: 110, fontSize: 11, padding: "4px 24px 4px 8px" }}
          value={colorMode}
          onChange={(e) => setColorMode(e.target.value as ColorMode)}
        >
          <option value="folder">Folder</option>
          <option value="filetype">File Type</option>
          <option value="tags">Tags</option>
          <option value="recency">Recency</option>
        </select>
        <button className="btn" disabled={building} onClick={() => void buildGraph()}>
          {building ? "Building…" : "Rebuild"}
        </button>
      </div>

      <div className="graph-split">
        <GraphCanvas
          nodes={coloredNodes}
          edges={edges}
          renderMode={renderMode}
          highlightSet={highlightSet}
          selectedIndex={selectedIndex}
          neighborSet={neighborSet}
          onNodeClick={handleNodeClick}
          onNodeHover={handleNodeHover}
          panTarget={panTarget}
          onPanComplete={handlePanComplete}
        />

        {legend.length > 0 && (
          <div className="graph-legend" style={{ position: "absolute", bottom: 12, left: 12 }}>
            <div className="legend-title">
              {colorMode === "folder" ? "Folders" : colorMode === "filetype" ? "Types" : colorMode === "tags" ? "Tags" : "Recency"}
            </div>
            {legend.map((group) => (
              <React.Fragment key={group.vault}>
                <div className="legend-vault">
                  <div className="legend-dot" style={{ background: group.vaultColor }} />
                  <span>{group.vault}</span>
                </div>
                {group.subs.map((sub) => (
                  <div key={sub.name} className="legend-row indent">
                    <div className="legend-dot" style={{ background: sub.color }} />
                    <span className="legend-label">{sub.name}</span>
                  </div>
                ))}
              </React.Fragment>
            ))}
          </div>
        )}

        <GraphSidebar
          nodes={coloredNodes}
          selectedIndex={selectedIndex}
          neighbors={neighbors}
          onSelectNode={handleSelectFromSidebar}
          onSearchResults={handleSearchResults}
          onClearSearch={handleClearSearch}
          hasActiveSearch={searchFilePaths.size > 0}
        />
      </div>
    </div>
  );
}
