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
import type { CanvasDoc } from "./types";

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
}

const nodeTypes = { mesh: MeshNode };

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
 * group's own direct members: a second, independent `<ReactFlow>` (its own
 * `<ReactFlowProvider>` — required for a second concurrent instance) fed a
 * filtered slice of the *same* `nodes`/`edges` state and the *same*
 * change handlers the main canvas uses, rather than a second copy of the
 * graph — so it's a filtered camera on one shared graph: dragging a member
 * here persists exactly like dragging it on the main canvas. A member's
 * own `position` is already relative to its group (see App.tsx's
 * `positionFor`), which is exactly what this view's own origin needs, so
 * no coordinate translation is needed either — only `parentId` is
 * stripped (the group itself isn't a node in this view, so there's
 * nothing left for it to compose against).
 *
 * v1 scope: only *direct* structural children — a member's own further
 * children (if it has any) stay out of this mini view and keep rendering
 * normally on the main canvas; opening a second panel from *inside* this
 * one (a nested group, or a plain node's own expand button) isn't wired
 * up yet either. Both are natural follow-ups, not attempted here.
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
}: NodeExpandPanelProps) {
  const { data } = node;
  const isGroup = data.nodeType === "group";

  const memberIds = useMemo(() => {
    if (!isGroup || !canvas) return new Set<string>();
    return new Set(canvas.nodes.filter((n) => n.parent === node.id).map((n) => n.id));
  }, [isGroup, canvas, node.id]);

  const memberNodes = useMemo(
    () =>
      nodes
        .filter((n) => memberIds.has(n.id))
        // The group itself isn't rendered here, so a member's own
        // `parentId` (pointing at it) has nothing to compose against —
        // its `position` is used directly as this view's own absolute
        // frame instead (see the doc comment above).
        .map((n) => ({ ...n, parentId: undefined })),
    [nodes, memberIds],
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
                nodesDraggable={editMode}
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
