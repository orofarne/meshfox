import { useState } from "react";
import { createPortal } from "react-dom";
import { BaseEdge, EdgeLabelRenderer, getBezierPath, getSmoothStepPath, type EdgeProps } from "@xyflow/react";
import type { ExtraEdgeDto } from "./types";
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
  /** "Delete" button tooltip in the properties panel — differs by what
   * `onDelete` actually does (plain removal vs. promote-another-edge). */
  title: string;
  /** Whether the properties panel's "Delete" button renders at all — an
   * extra edge is always deletable; a structural edge only when it has
   * another declared incoming edge to promote in its place (see App.tsx's
   * `candidates`), same accessibility rule this always had, just relocated
   * (see `DeletableEdge`'s own doc comment for why an on-canvas midpoint
   * button doesn't work well for this). */
  canDelete: boolean;
  /** True only for an extra (`meshfox:edge`) edge — enables the curved
   * bezier routing (see the "non-rectangular" requirement this exists for)
   * and the properties panel's full field set (color/line style/
   * arrowheads/tags, via `onUpdate`). A structural edge's panel only ever
   * has its own label to edit (via `onUpdateLabel`) — see SPEC.md, it has
   * no color/style/arrowhead attributes of its own. */
  editable?: boolean;
  label?: string;
  color?: string;
  style?: ExtraEdgeDto["style"];
  arrowStart?: ExtraEdgeDto["arrowStart"];
  arrowEnd?: ExtraEdgeDto["arrowEnd"];
  tags?: string[];
  /** Perpendicular offset (flow-space px) from the straight source→target
   * line — set only for an extra edge that shares its node pair with
   * another (a mutual/multiple link — see App.tsx's `parallelOffsets`),
   * which would otherwise render two curves exactly on top of each other,
   * indistinguishable and unclickable for anything but whichever painted
   * last. 0 (or absent, the common case) renders exactly as before this
   * existed — see `getParallelBezierPath`. */
  parallelOffset?: number;
  /** Extra edge only: persists a full style patch. */
  onUpdate?: (patch: Partial<Omit<ExtraEdgeDto, "from">>) => void;
  /** Structural edge only: persists just its own label. */
  onUpdateLabel?: (label: string) => void;
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
 * extra (`meshfox:edge`) edge, a curved bezier instead — a deliberately
 * different, non-rectangular shape so it always reads as "an authored
 * cross-reference", never as another nesting line.
 *
 * Neither kind has an on-canvas delete control anymore — both used to,
 * but a floating button sitting right where two edges cross (routinely
 * the case: an extra edge can connect any two nodes anywhere on the
 * canvas, and even a structural edge's own midpoint is no exception once
 * enough of them converge) ends up buried under whichever one painted
 * last, effectively unclickable. `EdgeEditorPanel` (reached by clicking
 * the edge itself, in edit mode) is immune to that — only one can ever be
 * open at a time, and it's always the topmost thing on screen — so
 * deleting lives there now instead, alongside an extra edge's full style
 * fields or a structural edge's own label. Either kind still gets its
 * `label` shown at the midpoint whenever set, in any mode — it's diagram
 * content, not an editing control.
 */
export function DeletableEdge({
  id,
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
  const curved = !!edgeData?.editable;
  const parallelOffset = edgeData?.parallelOffset ?? 0;
  const [path, labelX, labelY] = curved
    ? parallelOffset !== 0
      ? getParallelBezierPath(sourceX, sourceY, targetX, targetY, parallelOffset)
      : getBezierPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition })
    : getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition });

  const [open, setOpen] = useState(false);
  const canOpen = !!(edgeData?.editMode && (edgeData.editable ? edgeData.onUpdate : edgeData.onUpdateLabel));

  return (
    <>
      <BaseEdge id={id} path={path} style={style} markerEnd={markerEnd} markerStart={markerStart} />
      {canOpen && (
        // A wide, invisible click target on top of the (thin) visible path —
        // "click the arrow" to open its editor, without requiring pixel-
        // perfect precision on the actual stroke.
        <path
          d={path}
          fill="none"
          stroke="transparent"
          strokeWidth={16}
          className="nodrag nopan mesh-edge-hit"
          style={{ cursor: "pointer", pointerEvents: "stroke" }}
          onClick={() => setOpen((o) => !o)}
        />
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
 * style patch. A structural edge gets only its own text, via
 * `data.onUpdateLabel` — it has no color/style/arrowhead attributes of
 * its own (see SPEC.md). Either way, "Delete" only renders when
 * `data.canDelete` — always true for an extra edge, only true for a
 * structural one with somewhere else to reparent to.
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

  const handleCancel = () => onClose();

  const handleOk = () => {
    if (isExtra) {
      data.onUpdate?.({
        label: label.trim() === "" ? undefined : label,
        color: color.trim() === "" ? undefined : color,
        style: lineStyle,
        arrowStart,
        arrowEnd,
        tags,
      });
    } else {
      // Unlike the extra-edge branch above (a full-array `extraParents`
      // replace either way, so always sent), this only calls `onUpdate`
      // when the label actually changed — the server side (`update_node`)
      // treats "not sent" and "sent empty" differently (leave untouched
      // vs. clear), so sending it unconditionally on every "ok" would
      // needlessly touch the file even when nothing here was edited.
      const trimmed = label.trim();
      if (trimmed !== (data.label ?? "")) data.onUpdateLabel?.(trimmed);
    }
    onClose();
  };

  const handleDelete = () => {
    data.onDelete();
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
              <TagEditor tags={tags} onChange={setTags} />
            </label>
          </>
        )}
        <div className="vars-modal-actions">
          <button type="button" onClick={handleCancel}>
            cancel
          </button>
          <button type="submit" onClick={handleOk}>
            ok
          </button>
        </div>
        {data.canDelete && (
          <button
            type="button"
            className="mesh-edge-editor-delete node-settings-delete-button"
            onClick={handleDelete}
            title={data.title}
          >
            Delete arrow
          </button>
        )}
      </div>
    </div>,
    document.body,
  );
}
