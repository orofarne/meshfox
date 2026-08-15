import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  BaseEdge,
  EdgeLabelRenderer,
  getBezierPath,
  getSmoothStepPath,
  useReactFlow,
  useViewport,
  type EdgeProps,
} from "@xyflow/react";
import type { ExtraEdgeDto } from "./types";
import { TagEditor } from "./TagEditor";

/** Carried on a deletable edge's own `data` — everything `DeletableEdge`
 * needs to render its delete control, set per-edge in App.tsx's
 * edge-building effect. Used both for extra (`meshfox:edge`) edges, where
 * `onDelete` just drops the edge (`removeExtraEdge`), and for a structural
 * (nesting) edge that happens to have another incoming edge to fall back
 * on, where `onDelete` instead promotes that other edge to take its place
 * (`requestReparent` — see App.tsx). */
export interface DeletableEdgeData {
  editMode: boolean;
  onDelete: () => void;
  /** Button tooltip — differs by what `onDelete` actually does (plain
   * removal vs. promote-another-edge). */
  title: string;
  /** True only for an extra (`meshfox:edge`) edge — enables the curved
   * bezier routing (see the "non-rectangular" requirement this exists for)
   * and the on-canvas style editor. A structural `tree` edge stays a plain
   * right-angle connector with just the delete control. */
  editable?: boolean;
  label?: string;
  color?: string;
  style?: ExtraEdgeDto["style"];
  arrowStart?: ExtraEdgeDto["arrowStart"];
  arrowEnd?: ExtraEdgeDto["arrowEnd"];
  tags?: string[];
  /** Persists a style patch — only present when `editable` is true. */
  onUpdate?: (patch: Partial<Omit<ExtraEdgeDto, "from">>) => void;
  [key: string]: unknown;
}

/**
 * Renders a plain `smoothstep` edge (structural `tree` edges — same
 * right-angle path shape the indented-tree layout expects) or, for an
 * extra (`meshfox:edge`) edge, a curved bezier instead — a deliberately
 * different, non-rectangular shape so it always reads as "an authored
 * cross-reference", never as another nesting line.
 *
 * A structural (`tree`) edge's only delete control is the small "×" button
 * at its own midpoint (visible only in edit mode) — it has no properties
 * panel of its own to hold one instead. An extra edge's midpoint "×" is
 * dropped in favor of a "Delete" button inside its properties panel (see
 * `EdgeEditorPanel`) instead: a floating button sitting right where two
 * edges cross (routinely the case for a `meshfox:edge`, which can connect
 * any two nodes anywhere on the canvas) ends up buried under whichever one
 * painted last, effectively unclickable — the panel doesn't have that
 * problem, since only one can ever be open at a time and it's always the
 * topmost thing on screen (see that component's own doc comment). It also
 * still gets its `label`, shown at the midpoint whenever set (any mode —
 * it's diagram content, not an editing control), and, in edit mode, a
 * click-to-open for that same panel by clicking the edge itself.
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
  const [path, labelX, labelY] = curved
    ? getBezierPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition })
    : getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition });

  const [open, setOpen] = useState(false);
  const canEdit = !!(edgeData?.editable && edgeData.editMode && edgeData.onUpdate);

  return (
    <>
      <BaseEdge id={id} path={path} style={style} markerEnd={markerEnd} markerStart={markerStart} />
      {canEdit && (
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
      {edgeData?.editMode && (
        <EdgeLabelRenderer>
          <div
            className="mesh-edge-toolbar nodrag nopan"
            style={{
              position: "absolute",
              transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)`,
            }}
          >
            {(edgeData.label || (edgeData.tags && edgeData.tags.length > 0)) && (
              <span className="mesh-edge-label-badge">
                {edgeData.label}
                {edgeData.tags?.map((t) => (
                  <span className="mesh-tag-chip" key={t}>
                    {t}
                  </span>
                ))}
              </span>
            )}
            {/* An extra edge's own delete control lives in its properties
             * panel instead (see `EdgeEditorPanel`'s own "Delete" button) —
             * a structural `tree` edge has no such panel, so this stays its
             * only way to delete. */}
            {!edgeData.editable && (
              <button
                type="button"
                className="mesh-edge-delete"
                onClick={edgeData.onDelete}
                title={edgeData.title}
              >
                ×
              </button>
            )}
          </div>
        </EdgeLabelRenderer>
      )}
      {!edgeData?.editMode && (edgeData?.label || (edgeData?.tags && edgeData.tags.length > 0)) && (
        <EdgeLabelRenderer>
          <div
            className="mesh-edge-label-badge mesh-edge-label-readonly"
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
      {open && canEdit && (
        <EdgeEditorPanel data={edgeData!} x={labelX} y={labelY} onClose={() => setOpen(false)} />
      )}
    </>
  );
}

/** JSON Canvas colors are either a hex string or a preset `"1"`–`"6"` — same
 * shortcut `NodeSettings`' own color field offers, so an edge's color picks
 * from the exact same palette as a node's. */
const COLOR_SWATCHES = ["", "1", "2", "3", "4", "5", "6"];

const AUTOSAVE_DELAY_MS = 500;

/** Inline "click the arrow" editor for one extra edge's label/color/line
 * style/arrowheads — every field auto-saves (debounced), same convention
 * `NodeSettings` uses for a node's own fields, just scoped to this one
 * popover instead of a modal.
 *
 * Portaled straight to `document.body` (`x`/`y` are flow-space coordinates,
 * converted to screen ones below via `flowToScreenPosition`) rather than
 * rendered through `EdgeLabelRenderer` in place, like every other floating
 * overlay in this app (`NodeExpandPanel`/`TtyPanel`/`NodeTextEditor`) —
 * `EdgeLabelRenderer`'s own container sits *inside* `.react-flow__viewport`,
 * which is capped at a fixed z-index of its own; a node's floating "+"
 * button (`NodeToolbar`, see MeshNode.tsx) portals *outside* that viewport
 * entirely and gets elevated well above it the moment its node is selected
 * or dragged, so no z-index set on this panel itself could ever have
 * outranked it — confirmed the hard way (see the bug this fixed: the
 * button visibly bled through this panel's own background). `useViewport`
 * has no direct effect on the JSX below; it exists purely so this
 * component re-renders (and so re-reads `flowToScreenPosition`, which
 * itself isn't reactive) on every pan/zoom, keeping the panel glued to its
 * edge instead of drifting from it the moment the canvas moves. */
function EdgeEditorPanel({
  data,
  x,
  y,
  onClose,
}: {
  data: DeletableEdgeData;
  x: number;
  y: number;
  onClose: () => void;
}) {
  const { flowToScreenPosition } = useReactFlow();
  useViewport();
  const { x: screenX, y: screenY } = flowToScreenPosition({ x, y });
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

  const buildPatch = (): Partial<Omit<ExtraEdgeDto, "from">> => ({
    label: label.trim() === "" ? undefined : label,
    color: color.trim() === "" ? undefined : color,
    style: lineStyle,
    arrowStart,
    arrowEnd,
    tags,
  });

  const isFirstRender = useRef(true);
  const pendingSave = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (isFirstRender.current) {
      isFirstRender.current = false;
      return;
    }
    pendingSave.current = setTimeout(() => {
      data.onUpdate?.(buildPatch());
      pendingSave.current = null;
    }, AUTOSAVE_DELAY_MS);
    return () => {
      if (pendingSave.current) clearTimeout(pendingSave.current);
    };
    // Deliberately keyed on the field values themselves, not `data` (a new
    // object every parent render) — this should only re-fire when the user
    // actually edited something.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [label, color, lineStyle, arrowStart, arrowEnd, tags]);

  const handleClose = () => {
    if (pendingSave.current) {
      clearTimeout(pendingSave.current);
      data.onUpdate?.(buildPatch());
    }
    onClose();
  };

  // No `onClose()` call needed here (unlike `handleClose` above) — removing
  // the edge unmounts this panel along with it, once `data.onDelete()`
  // drops it from `App.tsx`'s own edges. Any pending autosave is dropped
  // rather than flushed first, same reasoning: about to be deleted, so
  // there's nothing left for that write to matter to.
  const handleDelete = () => {
    if (pendingSave.current) {
      clearTimeout(pendingSave.current);
    }
    data.onDelete();
  };

  return createPortal(
    <div
      className="mesh-edge-editor nodrag nopan"
      style={{
        position: "fixed",
        transform: `translate(-50%, -100%) translate(${screenX}px, ${screenY - 14}px)`,
      }}
      onClick={(e) => e.stopPropagation()}
    >
      <button type="button" className="mesh-edge-editor-close" onClick={handleClose} title="Close">
        ×
      </button>
      <label className="mesh-edge-editor-field">
        <span>Text</span>
        <input
          type="text"
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="arrow label"
          autoFocus
        />
      </label>
      <label className="mesh-edge-editor-field">
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
      <label className="mesh-edge-editor-field">
        <span>Line style</span>
        <select value={lineStyle} onChange={(e) => setLineStyle(e.target.value as typeof lineStyle)}>
          <option value="solid">solid</option>
          <option value="dashed">dashed</option>
          <option value="dotted">dotted</option>
        </select>
      </label>
      <label className="mesh-edge-editor-field">
        <span>Arrow start</span>
        <select value={arrowStart} onChange={(e) => setArrowStart(e.target.value as typeof arrowStart)}>
          <option value="none">none</option>
          <option value="arrow">arrow</option>
        </select>
      </label>
      <label className="mesh-edge-editor-field">
        <span>Arrow end</span>
        <select value={arrowEnd} onChange={(e) => setArrowEnd(e.target.value as typeof arrowEnd)}>
          <option value="none">none</option>
          <option value="arrow">arrow</option>
        </select>
      </label>
      <label className="mesh-edge-editor-field">
        <span>Tags</span>
        <TagEditor tags={tags} onChange={setTags} />
      </label>
      <button
        type="button"
        className="mesh-edge-editor-delete node-settings-delete-button"
        onClick={handleDelete}
        title={data.title}
      >
        Delete arrow
      </button>
    </div>,
    document.body,
  );
}
