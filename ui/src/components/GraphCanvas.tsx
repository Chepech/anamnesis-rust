import React, { useRef, useEffect, useCallback } from "react";

export interface GraphNode {
  x: number; y: number;
  path: string;
  label: string;
  snippet: string;
  color: string;
  folder: string;
  ext: string;
  tags: string;
  mtime: number;
  vector: number[];
}

export interface GraphEdge { a: number; b: number; strength: number; }

export type RenderMode = "idle" | "search" | "selected";

interface Props {
  nodes: GraphNode[];
  edges: GraphEdge[];
  renderMode: RenderMode;
  highlightSet: Set<number>;
  selectedIndex: number | null;
  neighborSet: Set<number>;
  onNodeClick: (index: number | null) => void;
  onNodeHover: (index: number | null) => void;
  panTarget: { x: number; y: number; zoom: number } | null;
  onPanComplete: () => void;
}

export function GraphCanvas({ nodes, edges, renderMode, highlightSet, selectedIndex, neighborSet, onNodeClick, onNodeHover, panTarget, onPanComplete }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);

  const stateRef = useRef({
    nodes, edges, renderMode, highlightSet, selectedIndex, neighborSet,
    pan: { x: 0, y: 0 }, zoom: 1,
    dragging: false, dragStart: { x: 0, y: 0 }, panStart: { x: 0, y: 0 },
    hovered: null as number | null,
    animating: false,
  });
  stateRef.current.nodes = nodes;
  stateRef.current.edges = edges;
  stateRef.current.renderMode = renderMode;
  stateRef.current.highlightSet = highlightSet;
  stateRef.current.selectedIndex = selectedIndex;
  stateRef.current.neighborSet = neighborSet;

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const { nodes, edges, pan, zoom, renderMode, highlightSet, selectedIndex, neighborSet, hovered } = stateRef.current;
    const W = canvas.clientWidth, H = canvas.clientHeight;
    if (!W || !H) return;

    ctx.clearRect(0, 0, W * devicePixelRatio, H * devicePixelRatio);
    if (nodes.length === 0) return;

    const pad = 24;
    const scaleX = (W - 2 * pad) * zoom;
    const scaleY = (H - 2 * pad) * zoom;
    const sx = (wx: number) => pad + (wx + pan.x) * scaleX;
    const sy = (wy: number) => pad + (wy + pan.y) * scaleY;

    const r0 = Math.max(3, Math.min(7, zoom * 5));

    // Draw edges
    for (const e of edges) {
      const aHighlight = highlightSet.has(e.a) || e.a === selectedIndex || neighborSet.has(e.a);
      const bHighlight = highlightSet.has(e.b) || e.b === selectedIndex || neighborSet.has(e.b);
      const a = nodes[e.a], b = nodes[e.b];

      let alpha: number;
      let width: number;
      let color = "rgba(150,150,160,";

      if (renderMode === "idle") {
        alpha = e.strength * 0.35;
        width = e.strength * 1.5;
      } else if (renderMode === "search") {
        alpha = (aHighlight && bHighlight) ? 0.25 : 0.03;
        width = (aHighlight && bHighlight) ? e.strength * 1.5 : 0.5;
      } else {
        // selected mode
        const isSelectedEdge = (e.a === selectedIndex && neighborSet.has(e.b)) || (e.b === selectedIndex && neighborSet.has(e.a));
        if (isSelectedEdge) {
          alpha = 1;
          width = e.strength * 3;
          color = "rgba(255,106,31,";
        } else {
          alpha = 0.03;
          width = 0.5;
        }
      }

      ctx.beginPath();
      ctx.moveTo(sx(a.x), sy(a.y));
      ctx.lineTo(sx(b.x), sy(b.y));
      ctx.strokeStyle = color + alpha + ")";
      ctx.lineWidth = width;
      ctx.stroke();
    }

    // Draw nodes
    for (let i = 0; i < nodes.length; i++) {
      const node = nodes[i];
      const x = sx(node.x), y = sy(node.y);
      const isHovered = i === hovered;
      const isSelected = i === selectedIndex;
      const isNeighbor = neighborSet.has(i);
      const isHighlighted = highlightSet.has(i);

      let radius: number;
      let opacity: number;

      if (renderMode === "idle") {
        radius = isHovered ? r0 * 1.8 : r0;
        opacity = isHovered ? 1 : 0.8;
      } else if (renderMode === "search") {
        if (isHighlighted) {
          radius = isHovered ? r0 * 1.9 : r0 * 1.6;
          opacity = 1;
        } else {
          radius = r0 * 0.7;
          opacity = 0.15;
        }
      } else {
        // selected
        if (isSelected) {
          radius = r0 * 2.0;
          opacity = 1;
        } else if (isNeighbor) {
          radius = isHovered ? r0 * 1.6 : r0 * 1.4;
          opacity = 1;
        } else {
          radius = r0 * 0.7;
          opacity = 0.2;
        }
      }

      ctx.beginPath();
      ctx.arc(x, y, radius, 0, Math.PI * 2);
      ctx.fillStyle = node.color;
      ctx.globalAlpha = opacity;
      ctx.fill();

      // Glow ring for search highlights
      if (renderMode === "search" && isHighlighted) {
        ctx.beginPath();
        ctx.arc(x, y, radius + 2, 0, Math.PI * 2);
        ctx.strokeStyle = "rgba(255,106,31,0.6)";
        ctx.lineWidth = 2;
        ctx.globalAlpha = 0.6;
        ctx.stroke();
      }

      // Selected node ring
      if (isSelected) {
        ctx.beginPath();
        ctx.arc(x, y, radius + 2, 0, Math.PI * 2);
        ctx.strokeStyle = "#ffffff";
        ctx.lineWidth = 2;
        ctx.globalAlpha = 0.9;
        ctx.stroke();
      }

      // Neighbor ring
      if (renderMode === "selected" && isNeighbor && !isSelected) {
        ctx.beginPath();
        ctx.arc(x, y, radius + 2, 0, Math.PI * 2);
        ctx.strokeStyle = "rgba(157,125,247,0.7)";
        ctx.lineWidth = 1;
        ctx.setLineDash([3, 3]);
        ctx.globalAlpha = 0.7;
        ctx.stroke();
        ctx.setLineDash([]);
      }

      // Hover label
      if (isHovered && (renderMode === "idle" || isHighlighted || isSelected || isNeighbor)) {
        ctx.globalAlpha = 1;
        ctx.fillStyle = "rgba(232,232,239,0.9)";
        ctx.font = `${Math.max(10, zoom * 11)}px Inter, sans-serif`;
        ctx.fillText(node.label, x + radius + 4, y + 4);
      }
    }
    ctx.globalAlpha = 1;
  }, []);

  // Resize canvas to DPR
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = devicePixelRatio ?? 1;
    const ro = new ResizeObserver(() => {
      const w = canvas.clientWidth, h = canvas.clientHeight;
      if (!w || !h) return;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      const ctx = canvas.getContext("2d");
      ctx?.scale(dpr, dpr);
      draw();
    });
    ro.observe(canvas);
    return () => ro.disconnect();
  }, [draw]);

  useEffect(() => { draw(); }, [nodes, edges, renderMode, highlightSet, selectedIndex, neighborSet, draw]);

  // Pan-to-node animation
  useEffect(() => {
    if (!panTarget) return;
    const s = stateRef.current;
    if (s.animating) return;
    s.animating = true;

    const startPan = { ...s.pan };
    const startZoom = s.zoom;
    const targetZoom = Math.max(panTarget.zoom, s.zoom);
    // To center node (nx, ny) in viewport: pan = 0.5/zoom - nodeCoord
    const targetPan = { x: 0.5 / targetZoom - panTarget.x, y: 0.5 / targetZoom - panTarget.y };

    const duration = 300;
    const startTime = performance.now();

    const animate = (now: number) => {
      const elapsed = now - startTime;
      const t = Math.min(1, elapsed / duration);
      const ease = 1 - Math.pow(1 - t, 3);

      s.pan = { x: startPan.x + (targetPan.x - startPan.x) * ease, y: startPan.y + (targetPan.y - startPan.y) * ease };
      s.zoom = startZoom + (targetZoom - startZoom) * ease;
      draw();

      if (t < 1) {
        requestAnimationFrame(animate);
      } else {
        s.animating = false;
        onPanComplete();
      }
    };
    requestAnimationFrame(animate);
  }, [panTarget, draw, onPanComplete]);

  // Mouse events
  useEffect(() => {
    const canvas = canvasRef.current;
    const tooltip = tooltipRef.current;
    if (!canvas) return;

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (stateRef.current.animating) return;
      const rect = canvas.getBoundingClientRect();
      const pad = 24;
      const { zoom, pan } = stateRef.current;
      const W = canvas.clientWidth, H = canvas.clientHeight;
      const mx = (e.clientX - rect.left - pad) / ((W - 2 * pad) * zoom);
      const my = (e.clientY - rect.top - pad) / ((H - 2 * pad) * zoom);
      const factor = e.deltaY < 0 ? 1.12 : 0.9;
      const newZoom = Math.max(0.2, Math.min(20, zoom * factor));
      stateRef.current.pan = { x: mx - (mx - pan.x) * (newZoom / zoom), y: my - (my - pan.y) * (newZoom / zoom) };
      stateRef.current.zoom = newZoom;
      draw();
    };

    const onDown = (e: MouseEvent) => {
      if (e.button !== 0 || stateRef.current.animating) return;
      stateRef.current.dragging = true;
      stateRef.current.dragStart = { x: e.clientX, y: e.clientY };
      stateRef.current.panStart = { ...stateRef.current.pan };
      canvas.style.cursor = "grabbing";
    };

    const hitTest = (cx: number, cy: number): number | null => {
      const { nodes, pan, zoom } = stateRef.current;
      const W = canvas.clientWidth, H = canvas.clientHeight;
      const pad = 24;
      const scaleX = (W - 2 * pad) * zoom, scaleY = (H - 2 * pad) * zoom;
      const sx = (wx: number) => pad + (wx + pan.x) * scaleX;
      const sy = (wy: number) => pad + (wy + pan.y) * scaleY;

      for (let i = 0; i < nodes.length; i++) {
        if (Math.hypot(cx - sx(nodes[i].x), cy - sy(nodes[i].y)) < 10) return i;
      }
      return null;
    };

    const onMove = (e: MouseEvent) => {
      const rect = canvas.getBoundingClientRect();
      const cx = e.clientX - rect.left, cy = e.clientY - rect.top;
      const { dragging, dragStart, panStart, zoom, animating } = stateRef.current;
      const W = canvas.clientWidth, H = canvas.clientHeight;
      const pad = 24;

      if (dragging) {
        stateRef.current.pan = {
          x: panStart.x + (e.clientX - dragStart.x) / ((W - 2 * pad) * zoom),
          y: panStart.y + (e.clientY - dragStart.y) / ((H - 2 * pad) * zoom),
        };
        draw();
        return;
      }

      if (animating) return;

      const found = hitTest(cx, cy);
      if (found !== stateRef.current.hovered) {
        stateRef.current.hovered = found;
        onNodeHover(found);
        draw();
      }

      if (found !== null && tooltip) {
        const node = stateRef.current.nodes[found];
        const parentRect = wrapRef.current?.getBoundingClientRect();
        tooltip.style.display = "flex";
        tooltip.style.left = `${e.clientX - (parentRect?.left ?? 0) + 14}px`;
        tooltip.style.top = `${e.clientY - (parentRect?.top ?? 0) + 14}px`;
        tooltip.textContent = "";
        const title = document.createElement("span");
        title.className = "tooltip-title";
        title.textContent = node.label;
        const snippet = document.createElement("span");
        snippet.className = "tooltip-snippet";
        snippet.textContent = node.snippet + "…";
        tooltip.appendChild(title);
        tooltip.appendChild(snippet);
      } else if (tooltip) {
        tooltip.style.display = "none";
      }
    };

    const onUp = (e: MouseEvent) => {
      const wasDragging = stateRef.current.dragging;
      const dragDist = Math.hypot(e.clientX - stateRef.current.dragStart.x, e.clientY - stateRef.current.dragStart.y);
      stateRef.current.dragging = false;
      canvas.style.cursor = "crosshair";

      if (!wasDragging) return;
      if (dragDist > 5) return;

      const rect = canvas.getBoundingClientRect();
      const cx = e.clientX - rect.left, cy = e.clientY - rect.top;
      if (cx < 0 || cy < 0 || cx > rect.width || cy > rect.height) return;
      const clicked = hitTest(cx, cy);
      onNodeClick(clicked);
    };

    canvas.addEventListener("wheel", onWheel, { passive: false });
    canvas.addEventListener("mousedown", onDown);
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("mousedown", onDown);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [draw, onNodeClick, onNodeHover]);

  return (
    <div className="graph-canvas-wrap" ref={wrapRef}>
      <canvas className="graph-canvas" ref={canvasRef} />
      <div className="graph-tooltip" ref={tooltipRef} style={{ display: "none" }} />
    </div>
  );
}
