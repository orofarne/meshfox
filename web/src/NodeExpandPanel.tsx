import { createPortal } from "react-dom";
import type { Node } from "@xyflow/react";
import { NodeBodyContent, type MeshNodeData } from "./MeshNode";

interface NodeExpandPanelProps {
  node: Node<MeshNodeData>;
  onClose: () => void;
}

/**
 * A node's own body, expanded into a floating window — same portal/fixed-
 * overlay shape as `TtyPanel`/`NodeTextEditor` (see index.css's shared
 * `mesh-expand-*` rules), for the same reason: a node's own box on the
 * canvas is small and at the mercy of the current pan/zoom, and reading or
 * running its blocks from there isn't always comfortable. Renders via
 * `NodeBodyContent` — the *exact* same live body `MeshNode` itself shows
 * (run/kill buttons, streaming output, the deps rail), not a separate
 * read-only copy that could drift from it. Available read-only, unlike
 * `NodeTextEditor` (edit mode only) — running a block is always allowed.
 *
 * Unlike `TtyPanel`, a click on the backdrop closes this — there's no live
 * process here whose session a stray click could kill, so it's safe to be
 * as dismissible as `NodeTextEditor`.
 */
export function NodeExpandPanel({ node, onClose }: NodeExpandPanelProps) {
  const { data } = node;
  return createPortal(
    <div className="mesh-expand-backdrop" onClick={onClose}>
      <div className="mesh-expand-panel" onClick={(e) => e.stopPropagation()}>
        <div className="mesh-expand-head">
          <span className="mesh-expand-head-title">{data.title}</span>
          <button type="button" onClick={onClose} title="Close">
            ✕
          </button>
        </div>
        <div className="mesh-expand-body">
          <NodeBodyContent data={data} nodeId={node.id} />
        </div>
      </div>
    </div>,
    document.body,
  );
}
