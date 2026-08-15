import { useMemo } from "react";
import { createPortal } from "react-dom";
import {
  ReactFlow,
  ReactFlowProvider,
  Background,
  Controls,
  type Node,
  type Edge,
  type OnNodesChange,
  type OnEdgesChange,
} from "@xyflow/react";
import { NodeBodyContent, MeshNode, type MeshNodeData } from "./MeshNode";
import { DeletableEdge } from "./DeletableEdge";
import type { CanvasDoc } from "./types";
import { subtreeIds } from "./tree";
import type { ThemePreference } from "./theme";

interface NodeExpandPanelProps {
  node: Node<MeshNodeData>;
  onClose: () => void;
  /** Only needed for a `group` node — see the module doc comment. */
  canvas: CanvasDoc | null;
  nodes: Node<MeshNodeData>[];
  edges: Edge[];
  onNodesChange: OnNodesChange<Node<MeshNodeData>>;
  onEdgesChange: OnEdgesChange<Edge>;
  editMode: boolean;
  /** Creates a new child node under the given parent id — wired to the same
   * handler the main canvas's own "+" buttons use, so a group's title-bar
   * "add child" control (unavailable in here, since the group itself isn't
   * rendered as a node in this view — see below) has an equivalent. */
  onAddChild: (parentId: string) => void;
  /** Mirrors the main canvas's `<ReactFlow colorMode>` (see App.tsx) so this
   * second, independent `<ReactFlow>` instance follows the same manual
   * light/dark override instead of defaulting to `"system"` regardless of
   * it. */
  themePreference: ThemePreference;
}

const nodeTypes = { mesh: MeshNode };
// Same registration as App.tsx's own `<ReactFlow>` — without it, this
// second, independent `<ReactFlow>` instance falls back to React Flow's
// plain default edge component for every member edge, silently losing
// `DeletableEdge`'s own delete button, click-to-open properties panel, and
// per-edge styling (color/dash/arrowheads) the moment a group's members
// are viewed through this panel instead of the main canvas.
const edgeTypes = { extra: DeletableEdge, tree: DeletableEdge };

/**
 * A node's own body, expanded into a floating window — same portal/fixed-
 * overlay shape as `TtyPanel`/`NodeTextEditor` (see index.css's shared
 * `mesh-expand-*` rules), for the same reason: a node's own box on the
 * canvas is small and at the mercy of the current pan/zoom, and reading or
 * running its blocks from there isn't always comfortable. Renders via
 * `NodeBodyContent` — the *exact* same live body `MeshNode` itself shows
 * (run/kill buttons, streaming output), not a separate read-only copy that
 * could drift from it. Available read-only, unlike `NodeTextEditor` (edit
 * mode only) — running a block is always allowed.
 *
 * Unlike `TtyPanel`, a click on the backdrop closes this — there's no live
 * process here whose session a stray click could kill, so it's safe to be
 * as dismissible as `NodeTextEditor`.
 *
 * A `group` node has no body at all (`NodeBodyContent` returns `null` for
 * one) — for that type only, this instead opens a mini sub-canvas of the
 * group's *entire* subtree (every descendant, not just direct members —
 * see `subtreeIds`): a second, independent `<ReactFlow>` (its own
 * `<ReactFlowProvider>` — required for a second concurrent instance) fed a
 * filtered slice of the *same* `nodes`/`edges` state and the *same*
 * change handlers the main canvas uses, rather than a second copy of the
 * graph — so it's a filtered camera on one shared graph: dragging a member
 * here persists exactly like dragging it on the main canvas. A direct
 * member's own `position` is already relative to its group (see App.tsx's
 * `positionFor`), which is exactly what this view's own origin needs, so
 * no coordinate translation is needed for it either — only its `parentId`
 * is stripped (the group itself isn't a node in this view, so there's
 * nothing left for it to compose against). A deeper descendant keeps
 * whatever `position`/`parentId` it already had on the main canvas — those
 * only ever compose against another member that's *also* included here
 * (its own ancestor chain up to the group), so they stay meaningful
 * unmodified.
 *
 * Opening a second panel from *inside* this one (a nested group's own
 * expand button, or a plain node's) isn't wired up yet — a natural
 * follow-up, not attempted here.
 */
export function NodeExpandPanel({
  node,
  onClose,
  canvas,
  nodes,
  edges,
  onNodesChange,
  onEdgesChange,
  editMode,
  onAddChild,
  themePreference,
}: NodeExpandPanelProps) {
  const { data } = node;
  const isGroup = data.nodeType === "group";

  const memberIds = useMemo(() => {
    if (!isGroup || !canvas) return new Set<string>();
    return new Set(subtreeIds(canvas, node.id));
  }, [isGroup, canvas, node.id]);

  const memberNodes = useMemo(
    () =>
      nodes
        .filter((n) => memberIds.has(n.id))
        // Only a *direct* member's own `parentId` points at the group
        // itself, which isn't rendered here — nothing left for it to
        // compose against, so it's stripped (see the doc comment above). A
        // deeper descendant's `parentId`, if any, points at another
        // included member instead and stays untouched.
        .map((n) => (n.parentId === node.id ? { ...n, parentId: undefined } : n)),
    [nodes, memberIds, node.id],
  );
  const memberEdges = useMemo(
    () => edges.filter((e) => memberIds.has(e.source) && memberIds.has(e.target)),
    [edges, memberIds],
  );

  return createPortal(
    <div className="mesh-expand-backdrop" onClick={onClose}>
      <div
        className={isGroup ? "mesh-expand-panel mesh-expand-panel-group" : "mesh-expand-panel"}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mesh-expand-head">
          <span className="mesh-expand-head-title">{data.title}</span>
          {isGroup && editMode && (
            <button
              type="button"
              onClick={() => onAddChild(node.id)}
              title="Add a child node to this group"
            >
              + Add node
            </button>
          )}
          <button type="button" onClick={onClose} title="Close">
            ✕
          </button>
        </div>
        <div className="mesh-expand-body">
          {isGroup ? (
            <ReactFlowProvider>
              <ReactFlow
                nodes={memberNodes}
                edges={memberEdges}
                onNodesChange={onNodesChange}
                onEdgesChange={onEdgesChange}
                nodeTypes={nodeTypes}
                edgeTypes={edgeTypes}
                // Matches the main canvas's own override (see App.tsx's
                // `<ReactFlow>`) — without it, an uncolored edge's
                // arrowhead here falls back to React Flow's flat gray
                // default instead of this app's own accent.
                defaultMarkerColor="var(--accent)"
                nodesDraggable={editMode}
                colorMode={themePreference}
                fitView
              >
                <Background />
                <Controls />
              </ReactFlow>
            </ReactFlowProvider>
          ) : (
            <NodeBodyContent data={data} nodeId={node.id} />
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
