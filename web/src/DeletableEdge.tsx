import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { BaseEdge, EdgeLabelRenderer, Position, getBezierPath, getSmoothStepPath, useReactFlow, useStore, useViewport, type EdgeProps } from "@xyflow/react";
import type { ExtraEdgeDto } from "./types";
import type { EdgeSide } from "./edgePorts";
import { TagEditor } from "./TagEditor";

/** Carried on a deletable edge's own `data` — everything `DeletableEdge`
 * needs, set per-edge in App.tsx's edge-building effect. Used both for
 * extra (`meshfox:edge`) edges, where `onDelete` just drops the edge
 * (`removeExtraEdge`), and for a structural (nesting) edge, where
 * `onDelete` instead promotes another declared incoming edge to take its
 * place (`requestReparentEdge` — see App.tsx). */
export interface DeletableEdgeData {
  editMode: boolean;
  onDelete: () => void;
  /** "Delete" button tooltip in the edge toolbar — differs by what
   * `onDelete` actually does (plain removal vs. promote-another-edge). */
  title: string;
  /** Whether the edge toolbar's "Delete" button renders at all — an
   * extra edge is always deletable; a structural edge only when it has
   * another declared incoming edge to promote in its place (see App.tsx's
   * `candidates`), same accessibility rule this always had. */
  canDelete: boolean;
  /** True only for an extra (`meshfox:edge`) edge — enables obstacle-avoiding
   * routing with rounded corners (falling back to a bezier when blocked)
   * and the properties panel's full field set (color/line style/
   * arrowheads/tags, via `onUpdate`). A structural edge's panel only ever
   * has label and route controls, but no color/style/arrowhead attributes. */
  editable?: boolean;
  label?: string;
  color?: string;
  style?: ExtraEdgeDto["style"];
  arrowStart?: ExtraEdgeDto["arrowStart"];
  arrowEnd?: ExtraEdgeDto["arrowEnd"];
  tags?: string[];
  /** Every distinct tag already used anywhere in the document — offered as
   * suggestions by this edge's own `TagEditor` (see App.tsx's
   * `documentTags`). Extra-edge-only, like `tags` itself. */
  existingTags?: string[];
  /** Perpendicular offset (flow-space px) for the fallback bezier — set
   * only for an extra edge that shares its node pair with
   * another (a mutual/multiple link — see App.tsx's `parallelOffsets`),
   * which would otherwise render two curves exactly on top of each other,
   * indistinguishable and unclickable for anything but whichever painted
   * last. 0 (or absent, the common case) renders exactly as before this
   * existed — see `getParallelBezierPath`. */
  parallelOffset?: number;
  /** Displacement along the source/target node's side from its centered
   * routing handle. Computed with all edges on that side in App.tsx. */
  sourceOffset?: number;
  targetOffset?: number;
  /** Derived from all visible extra edges for this render; never persisted. */
  routedPath?: [string, number, number];
  routedPoints?: { x: number; y: number }[];
  routeFailed?: boolean;
  via?: { x: number; y: number }[];
  sourceSide?: ExtraEdgeDto["sourceSide"];
  targetSide?: ExtraEdgeDto["targetSide"];
  /** Extra edge only: persists a full style patch. */
  onUpdate?: (patch: Partial<Omit<ExtraEdgeDto, "from">>) => void;
  onUpdateRoute?: (patch: { sourceSide?: "left" | "right" | "top" | "bottom" | "auto"; targetSide?: "left" | "right" | "top" | "bottom" | "auto"; via?: { x: number; y: number }[]; label?: string }) => void;
  [key: string]: unknown;
}

/** Quadratic bezier from `(sourceX,sourceY)` to `(targetX,targetY)` with an
 * explicit perpendicular offset applied to the source→target line's own
 * midpoint — `getBezierPath`'s own `curvature` option only controls how
 * round a curve is *along* that same line, it has no way to shift the
 * whole thing sideways, which is what telling apart two edges between the
 * exact same pair of points needs (see `DeletableEdgeData.parallelOffset`).
 * Returns `[path, labelX, labelY]`, the same shape `getBezierPath` does,
 * so callers don't need to branch on which one they got. */
function getParallelBezierPath(
  sourceX: number,
  sourceY: number,
  targetX: number,
  targetY: number,
  offset: number,
): [string, number, number] {
  const dx = targetX - sourceX;
  const dy = targetY - sourceY;
  const dist = Math.hypot(dx, dy) || 1;
  const nx = -dy / dist;
  const ny = dx / dist;
  const midX = (sourceX + targetX) / 2 + nx * offset;
  const midY = (sourceY + targetY) / 2 + ny * offset;
  return [`M${sourceX},${sourceY} Q${midX},${midY} ${targetX},${targetY}`, midX, midY];
}

/**
 * Renders a plain `smoothstep` edge (structural `tree` edges — same
 * right-angle path shape the indented-tree layout expects) or, for an
 * extra (`meshfox:edge`) edge, a rounded obstacle-avoiding path instead — a deliberately
 * different shape so it always reads as "an authored
 * cross-reference", never as another nesting line.
 *
 * Selecting an edge in edit mode shows route handles and a small toolbar
 * above the canvas. The toolbar opens settings, resets the route, or deletes
 * the edge when deletion is allowed. Labels remain visible in either mode.
 */
export function DeletableEdge({
  id,
  source,
  target,
  selected,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  style,
  markerEnd,
  markerStart,
  data,
}: EdgeProps) {
  const edgeData = data as DeletableEdgeData | undefined;
  const { screenToFlowPosition, flowToScreenPosition } = useReactFlow();
  const renderer = useStore((state) => state.domNode?.querySelector(".react-flow__renderer"));
  useViewport(); // Reposition the portal controls when the canvas pans or zooms.
  const [draggingRoute, setDraggingRoute] = useState(false);
  const [draggingEndpoint, setDraggingEndpoint] = useState<{ x: number; y: number } | null>(null);
  const curved = !!edgeData?.editable;
  const parallelOffset = edgeData?.parallelOffset ?? 0;
  const sx = sourceX + ((sourcePosition === Position.Top || sourcePosition === Position.Bottom) ? (edgeData?.sourceOffset ?? 0) : 0);
  const sy = sourceY + ((sourcePosition === Position.Left || sourcePosition === Position.Right) ? (edgeData?.sourceOffset ?? 0) : 0);
  const tx = targetX + ((targetPosition === Position.Top || targetPosition === Position.Bottom) ? (edgeData?.targetOffset ?? 0) : 0);
  const ty = targetY + ((targetPosition === Position.Left || targetPosition === Position.Right) ? (edgeData?.targetOffset ?? 0) : 0);
  const originX = edgeData?.routedPoints?.[0]?.x ?? sx;
  const originY = edgeData?.routedPoints?.[0]?.y ?? sy;
  const fromStored = (points: { x: number; y: number }[]) => points.map((p) => ({ x: originX + p.x, y: originY + p.y }));
  const toStored = (points: { x: number; y: number }[]) => points.map((p) => ({ x: Math.round(p.x - originX), y: Math.round(p.y - originY) }));
  const [draftVia, setDraftVia] = useState(() => fromStored(edgeData?.via ?? []));
  const draftRef = useRef(draftVia);
  draftRef.current = draftVia;
  useEffect(() => { setDraftVia(fromStored(edgeData?.via ?? [])); }, [edgeData?.via, originX, originY]);
  const [path, labelX, labelY] = curved
    ? edgeData?.routedPath ?? (parallelOffset !== 0
      ? getParallelBezierPath(sx, sy, tx, ty, parallelOffset)
      : getBezierPath({ sourceX: sx, sourceY: sy, sourcePosition, targetX: tx, targetY: ty, targetPosition }))
    : edgeData?.routedPath ?? getSmoothStepPath({ sourceX: sx, sourceY: sy, sourcePosition, targetX: tx, targetY: ty, targetPosition });

  const [open, setOpen] = useState(false);
  const canOpen = !!(edgeData?.editMode && (edgeData.editable ? edgeData.onUpdate : edgeData.onUpdateRoute));
  const controlsVisible = !!(canOpen && selected);
  const localPoint = (point: { x: number; y: number }) => {
    const screen = flowToScreenPosition(point);
    const rect = renderer?.getBoundingClientRect();
    return { x: screen.x - (rect?.left ?? 0), y: screen.y - (rect?.top ?? 0) };
  };
  const toolbarAnchorY = Math.min(
    labelY,
    ...(edgeData?.routedPoints?.map((point) => point.y) ?? [sy, ty]),
  );

  const sideAt = (nodeId: string, clientX: number, clientY: number): EdgeSide | null => {
    const node = renderer?.querySelector(`.react-flow__node[data-id="${CSS.escape(nodeId)}"]`);
    if (!node) return null;
    const box = node.getBoundingClientRect();
    if (clientX < box.left - 48 || clientX > box.right + 48 || clientY < box.top - 48 || clientY > box.bottom + 48) return null;
    const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(max, value));
    const distances: [EdgeSide, number][] = [
      ["left", Math.hypot(clientX - box.left, clientY - clamp(clientY, box.top, box.bottom))],
      ["right", Math.hypot(clientX - box.right, clientY - clamp(clientY, box.top, box.bottom))],
      ["top", Math.hypot(clientY - box.top, clientX - clamp(clientX, box.left, box.right))],
      ["bottom", Math.hypot(clientY - box.bottom, clientX - clamp(clientX, box.left, box.right))],
    ];
    return distances.sort((a, b) => a[1] - b[1])[0][0];
  };

  const dragEndpoint = (end: "source" | "target", event: React.PointerEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const move = (e: PointerEvent) => setDraggingEndpoint(screenToFlowPosition({ x: e.clientX, y: e.clientY }));
    const up = (e: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setDraggingEndpoint(null);
      const side = sideAt(end === "source" ? source : target, e.clientX, e.clientY);
      if (!side) return;
      if (end === "target") {
        edgeData?.onUpdateRoute?.({ targetSide: side });
        return;
      }
      // Waypoints are stored relative to the source port. Keep them in the
      // same canvas positions when that port moves to a different side.
      const node = renderer?.querySelector(`.react-flow__node[data-id="${CSS.escape(source)}"]`);
      const box = node?.getBoundingClientRect();
      if (!box) return;
      const screenPort = {
        x: side === "left" ? box.left : side === "right" ? box.right : box.left + box.width / 2,
        y: side === "top" ? box.top : side === "bottom" ? box.bottom : box.top + box.height / 2,
      };
      const nextOrigin = screenToFlowPosition(screenPort);
      edgeData?.onUpdateRoute?.({
        sourceSide: side,
        via: draftRef.current.map((point) => ({ x: Math.round(point.x - nextOrigin.x), y: Math.round(point.y - nextOrigin.y) })),
      });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up, { once: true });
  };

  const addWaypoint = (clientX: number, clientY: number) => {
    const flow = screenToFlowPosition({ x: clientX, y: clientY });
    const point = { x: Math.round(flow.x), y: Math.round(flow.y) };
    const path = edgeData?.routedPoints ?? [];
    const closestIndex = (p: { x: number; y: number }) => path.reduce((best, candidate, index) =>
      Math.hypot(candidate.x - p.x, candidate.y - p.y) < Math.hypot(path[best].x - p.x, path[best].y - p.y) ? index : best, 0);
    const position = path.length ? closestIndex(point) : 0;
    const insertAt = path.length ? draftRef.current.filter((p) => closestIndex(p) < position).length : draftRef.current.length;
    const next = [...draftRef.current];
    next.splice(insertAt, 0, point);
    draftRef.current = next;
    setDraftVia(next);
    edgeData?.onUpdateRoute?.({ via: toStored(next) });
  };

  const dragSegment = (point: { x: number; y: number }, event: React.PointerEvent) => {
    const path = edgeData?.routedPoints ?? [];
    const closestIndex = (p: { x: number; y: number }) => path.reduce((best, candidate, index) =>
      Math.hypot(candidate.x - p.x, candidate.y - p.y) < Math.hypot(path[best].x - p.x, path[best].y - p.y) ? index : best, 0);
    const position = path.length ? closestIndex(point) : 0;
    const insertAt = path.length ? draftRef.current.filter((p) => closestIndex(p) < position).length : draftRef.current.length;
    const next = [...draftRef.current];
    next.splice(insertAt, 0, { x: Math.round(point.x), y: Math.round(point.y) });
    draftRef.current = next;
    setDraftVia(next);
    dragWaypoint(insertAt, event);
  };

  const dragWaypoint = (index: number, event: React.PointerEvent) => {
    event.preventDefault();
    event.stopPropagation();
    setDraggingRoute(true);
    const move = (e: PointerEvent) => {
      const flow = screenToFlowPosition({ x: e.clientX, y: e.clientY });
      const next = draftRef.current.map((p, i) => i === index ? { x: Math.round(flow.x), y: Math.round(flow.y) } : p);
      draftRef.current = next;
      setDraftVia(next);
    };
    const up = () => {
      setDraggingRoute(false);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      edgeData?.onUpdateRoute?.({ via: toStored(draftRef.current) });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up, { once: true });
  };

  return (
    <>
      <BaseEdge id={id} path={path} style={style} markerEnd={markerEnd} markerStart={markerStart} />
      {draggingRoute && draftVia.length > 0 && <path
        d={`M${sx},${sy} ${draftVia.map((p) => `L${p.x},${p.y}`).join(" ")} L${tx},${ty}`}
        fill="none" stroke="var(--accent)" strokeWidth={2} strokeDasharray="4 4" pointerEvents="none" />}
      {canOpen && (
        // A wide, invisible click target on top of the (thin) visible path —
        // "click the arrow" to select it, without requiring pixel-
        // perfect precision on the actual stroke.
        <path
          d={path}
          fill="none"
          stroke="transparent"
          strokeWidth={16}
          className="nodrag nopan mesh-edge-hit"
          style={{ cursor: "pointer", pointerEvents: "stroke" }}
          onDoubleClick={(e) => { if (controlsVisible) { e.stopPropagation(); addWaypoint(e.clientX, e.clientY); } }}
        />
      )}
      {controlsVisible && renderer && edgeData && createPortal(
        <div className="mesh-edge-controls-layer">
          <div className="mesh-node-toolbar mesh-edge-toolbar nodrag nopan" data-testid="edge-toolbar"
            style={{ left: Math.max(72, Math.min(renderer.clientWidth - 72, localPoint({ x: labelX, y: labelY }).x)), top: Math.max(48, localPoint({ x: labelX, y: toolbarAnchorY }).y - 20) }}>
            <button type="button" className="mesh-node-icon-button" title="Arrow settings" aria-label="Arrow settings" onClick={() => setOpen(true)}>⚙</button>
            <button type="button" className="mesh-node-icon-button" title="Reset route and attachment sides to automatic" aria-label="Reset arrow route" onClick={() => {
              draftRef.current = [];
              setDraftVia([]);
              edgeData.onUpdateRoute?.({ via: [], sourceSide: "auto", targetSide: "auto" });
            }}>↺</button>
            {edgeData.canDelete && <button type="button" className="mesh-node-icon-button mesh-node-delete-icon" title={edgeData.title} aria-label="Delete arrow" onClick={edgeData.onDelete}>🗑</button>}
            {edgeData.routeFailed && <span role="alert" title="A waypoint is blocked by a node">⚠</span>}
          </div>
          {(["source", "target"] as const).map((end) => {
            const point = localPoint(end === "source" ? { x: sx, y: sy } : { x: tx, y: ty });
            return <button key={end} type="button" className={`mesh-edge-route-handle mesh-edge-endpoint mesh-edge-endpoint-${end} nodrag nopan`}
              aria-label={`Drag ${end} to a side of its node`} title={`Drag ${end} to a side of its node`}
              style={{ left: point.x, top: point.y }} onPointerDown={(event) => dragEndpoint(end, event)} />;
          })}
          {draftVia.map((point, index) => {
            const local = localPoint(point);
            return <button key={`waypoint-${index}`} type="button" className="mesh-edge-route-handle mesh-edge-waypoint nodrag nopan"
              aria-label={`Drag route point ${index + 1}`} title="Drag; double-click to remove"
              style={{ left: local.x, top: local.y }} onPointerDown={(event) => dragWaypoint(index, event)}
              onDoubleClick={(event) => {
                event.stopPropagation();
                const next = draftRef.current.filter((_, i) => i !== index);
                draftRef.current = next;
                setDraftVia(next);
                edgeData.onUpdateRoute?.({ via: toStored(next) });
              }} />;
          })}
          {edgeData.routedPoints?.slice(1).map((point, index) => {
            const previous = edgeData.routedPoints![index];
            if (Math.hypot(point.x - previous.x, point.y - previous.y) < 40) return null;
            const middle = { x: (point.x + previous.x) / 2, y: (point.y + previous.y) / 2 };
            const local = localPoint(middle);
            return <button key={`segment-${index}`} type="button" className="mesh-edge-route-handle mesh-edge-segment nodrag nopan"
              aria-label={`Drag route segment ${index + 1}`} title="Drag to bend route"
              style={{ left: local.x, top: local.y }} onPointerDown={(event) => dragSegment(middle, event)} />;
          })}
          {draggingEndpoint && <span className="mesh-edge-route-handle mesh-edge-endpoint mesh-edge-endpoint-preview" style={{ left: localPoint(draggingEndpoint).x, top: localPoint(draggingEndpoint).y }} />}
        </div>, renderer,
      )}
      {(edgeData?.label || (edgeData?.tags && edgeData.tags.length > 0)) && (
        <EdgeLabelRenderer>
          <div
            className="mesh-edge-label-badge"
            style={{
              position: "absolute",
              pointerEvents: "none",
              transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)`,
            }}
          >
            {edgeData!.label}
            {edgeData!.tags?.map((t) => (
              <span className="mesh-tag-chip" key={t}>
                {t}
              </span>
            ))}
          </div>
        </EdgeLabelRenderer>
      )}
      {open && canOpen && <EdgeEditorPanel data={edgeData!} onClose={() => setOpen(false)} />}
    </>
  );
}

/** JSON Canvas colors are either a hex string or a preset `"1"`–`"6"` — same
 * shortcut `NodeSettings`' own color field offers, so an edge's color picks
 * from the exact same palette as a node's. */
const COLOR_SWATCHES = ["", "1", "2", "3", "4", "5", "6"];

/**
 * One edge's properties: a centered modal, same ok/cancel-only-commits
 * shape as `NodeSettings` (reuses its `.vars-modal`/`.vars-modal-field`/
 * `.vars-modal-actions` styling directly) — every field here is a local
 * draft until "ok"; "cancel" (or clicking the backdrop) discards it
 * outright. This used to autosave on a debounce instead, anchored to the
 * edge's own midpoint — abandoned for two reasons: a canvas reload after
 * every keystroke's autosave remounted this component fresh (a brand new
 * React element, since the whole `edges` array gets rebuilt from the
 * reloaded doc), silently resetting `open` back to `false` mid-edit, and
 * anchoring a `position: fixed` panel to a specific edge required its own
 * screen-space tracking (`flowToScreenPosition`, reactive to pan/zoom)
 * that a plain centered modal doesn't need at all.
 *
 * An extra (`meshfox:edge`) edge (`data.editable`) gets the full field
 * set — text/color/line style/arrowheads/tags, via `data.onUpdate`'s
 * style patch. A structural edge gets text and attachment sides; it has no
 * color/style/arrowhead attributes of its own (see SPEC.md).
 */
function EdgeEditorPanel({ data, onClose }: { data: DeletableEdgeData; onClose: () => void }) {
  const isExtra = !!data.editable;
  const [label, setLabel] = useState(data.label ?? "");
  const [color, setColor] = useState(data.color ?? "");
  const [lineStyle, setLineStyle] = useState<NonNullable<ExtraEdgeDto["style"]>>(data.style ?? "dashed");
  const [arrowStart, setArrowStart] = useState<NonNullable<ExtraEdgeDto["arrowStart"]>>(
    data.arrowStart ?? "none",
  );
  const [arrowEnd, setArrowEnd] = useState<NonNullable<ExtraEdgeDto["arrowEnd"]>>(
    data.arrowEnd ?? "arrow",
  );
  const [tags, setTags] = useState<string[]>(data.tags ?? []);
  const [sourceSide, setSourceSide] = useState<"auto" | NonNullable<ExtraEdgeDto["sourceSide"]>>(data.sourceSide ?? "auto");
  const [targetSide, setTargetSide] = useState<"auto" | NonNullable<ExtraEdgeDto["targetSide"]>>(data.targetSide ?? "auto");

  const handleCancel = () => onClose();

  const handleOk = () => {
    if (isExtra) {
      const patch: Partial<Omit<ExtraEdgeDto, "from">> = {};
      if (label.trim() !== (data.label ?? "")) patch.label = label.trim() || undefined;
      if (color.trim() !== (data.color ?? "")) patch.color = color.trim() || undefined;
      if (lineStyle !== (data.style ?? "dashed")) patch.style = lineStyle;
      if (arrowStart !== (data.arrowStart ?? "none")) patch.arrowStart = arrowStart;
      if (arrowEnd !== (data.arrowEnd ?? "arrow")) patch.arrowEnd = arrowEnd;
      if (JSON.stringify(tags) !== JSON.stringify(data.tags ?? [])) patch.tags = tags;
      if (sourceSide !== (data.sourceSide ?? "auto")) patch.sourceSide = sourceSide === "auto" ? null : sourceSide;
      if (targetSide !== (data.targetSide ?? "auto")) patch.targetSide = targetSide === "auto" ? null : targetSide;
      if (Object.keys(patch).length) data.onUpdate?.(patch);
    } else {
      // Unlike the extra-edge branch above (a full-array `extraParents`
      // replace either way, so always sent), this only calls `onUpdate`
      // when the label actually changed — the server side (`update_node`)
      // treats "not sent" and "sent empty" differently (leave untouched
      // vs. clear), so sending it unconditionally on every "ok" would
      // needlessly touch the file even when nothing here was edited.
      const trimmed = label.trim();
      const patch: Parameters<NonNullable<DeletableEdgeData["onUpdateRoute"]>>[0] = {};
      if (trimmed !== (data.label ?? "")) patch.label = trimmed;
      if (sourceSide !== (data.sourceSide ?? "auto")) patch.sourceSide = sourceSide;
      if (targetSide !== (data.targetSide ?? "auto")) patch.targetSide = targetSide;
      if (Object.keys(patch).length) data.onUpdateRoute?.(patch);
    }
    onClose();
  };

  return createPortal(
    <div className="vars-modal-backdrop" onClick={handleCancel}>
      <div className="vars-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Arrow properties</h3>
        <label className="vars-modal-field">
          <span>Text</span>
          <input
            type="text"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="arrow label"
            autoFocus
          />
        </label>
        {isExtra && (
          <>
            <label className="vars-modal-field">
              <span>Color</span>
              <input
                type="text"
                value={color}
                onChange={(e) => setColor(e.target.value)}
                placeholder="hex, e.g. #ff8800, or a preset 1–6"
              />
              <div className="mesh-edge-editor-swatches">
                {COLOR_SWATCHES.map((c) => (
                  <button
                    type="button"
                    key={c || "none"}
                    className="node-settings-swatch"
                    data-swatch={c || "none"}
                    title={c || "no color"}
                    onClick={() => setColor(c)}
                  />
                ))}
              </div>
            </label>
            <label className="vars-modal-field">
              <span>Line style</span>
              <select value={lineStyle} onChange={(e) => setLineStyle(e.target.value as typeof lineStyle)}>
                <option value="solid">solid</option>
                <option value="dashed">dashed</option>
                <option value="dotted">dotted</option>
              </select>
            </label>
            <label className="vars-modal-field">
              <span>Arrow start</span>
              <select value={arrowStart} onChange={(e) => setArrowStart(e.target.value as typeof arrowStart)}>
                <option value="none">none</option>
                <option value="arrow">arrow</option>
              </select>
            </label>
            <label className="vars-modal-field">
              <span>Arrow end</span>
              <select value={arrowEnd} onChange={(e) => setArrowEnd(e.target.value as typeof arrowEnd)}>
                <option value="none">none</option>
                <option value="arrow">arrow</option>
              </select>
            </label>
            <label className="vars-modal-field">
              <span>Tags</span>
              <TagEditor tags={tags} onChange={setTags} suggestions={data.existingTags ?? []} />
            </label>
          </>
        )}
        <label className="vars-modal-field">
          <span>From side</span>
          <select value={sourceSide} onChange={(e) => setSourceSide(e.target.value as typeof sourceSide)}>
            <option value="auto">auto</option><option value="left">left</option><option value="right">right</option><option value="top">top</option><option value="bottom">bottom</option>
          </select>
        </label>
        <label className="vars-modal-field">
          <span>To side</span>
          <select value={targetSide} onChange={(e) => setTargetSide(e.target.value as typeof targetSide)}>
            <option value="auto">auto</option><option value="left">left</option><option value="right">right</option><option value="top">top</option><option value="bottom">bottom</option>
          </select>
        </label>
        <div className="vars-modal-actions">
          <button type="button" onClick={handleCancel}>
            cancel
          </button>
          <button type="submit" onClick={handleOk}>
            ok
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
