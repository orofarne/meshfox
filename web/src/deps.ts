// Client-side mirror of crates/core/src/deps.rs — the dependency graph over
// runnable code blocks (`deps=` on a fence, see ./fence.ts), independent of
// the node tree. Used here for two read-only, non-authoritative purposes:
// rendering a "this button also runs N other blocks" hint, and drawing
// dependency arrows between nodes. The server is still what actually
// resolves and runs the chain (crates/core/src/deps.rs, via /api/run) —
// this never needs to be byte-for-byte identical, just close enough for
// preview/UI purposes.

import type { CanvasDoc } from "./types";
import { parseBody } from "./fence";

export interface BlockAddr {
  nodeId: string;
  blockName: string;
}

export function addrKey(a: BlockAddr): string {
  return `${a.nodeId}::${a.blockName}`;
}

/** Resolves one raw `deps=` entry (a bare block name, or `node-id/block-name`,
 * either optionally suffixed with `!` — see `core::fence::BlockRef::sync`)
 * against the node that declared it. The trailing `!` only matters to the
 * server's own session-freshness bookkeeping (`meshfox_core::deps::
 * compute_forced_reruns`) — nothing here reads it back, it's just stripped
 * before resolving so the block name itself still matches. Exported for
 * MeshNode's clickable "after: …" links, which need the same resolution
 * just to jump to a block rather than to build the whole graph. */
export function parseBlockRef(raw: string, ownerNodeId: string): BlockAddr {
  const ref = raw.endsWith("!") ? raw.slice(0, -1) : raw;
  const slash = ref.indexOf("/");
  if (slash === -1) return { nodeId: ownerNodeId, blockName: ref };
  return { nodeId: ref.slice(0, slash), blockName: ref.slice(slash + 1) };
}

/** Stable DOM id for a runnable code block, used to scroll/highlight it
 * when a dependency link elsewhere is clicked (see MeshNode). */
export function blockDomId(addr: BlockAddr): string {
  return `mesh-block-${addrKey(addr)}`;
}

interface BlockInfo {
  addr: BlockAddr;
  deps: BlockAddr[];
}

/** Every runnable block in the canvas, keyed by `addrKey`, with its
 * `deps=` resolved to concrete node/block addresses. */
export function buildBlockGraph(canvas: CanvasDoc): Map<string, BlockInfo> {
  const graph = new Map<string, BlockInfo>();
  for (const node of canvas.nodes) {
    for (const seg of parseBody(node.text, node.id)) {
      if (seg.type !== "code") continue;
      const addr = { nodeId: node.id, blockName: seg.name };
      const deps = seg.deps.map((raw) => parseBlockRef(raw, node.id));
      graph.set(addrKey(addr), { addr, deps });
    }
  }
  return graph;
}

export class DepsError extends Error {}

/**
 * Topologically-sorted run order for `target` and everything it
 * transitively depends on (dependencies first, no duplicates, `target`
 * last). Throws `DepsError` on a missing block or a dependency cycle —
 * callers doing anything but a best-effort preview should treat that as
 * "can't preview the chain" rather than a hard failure, since the server
 * (crates/core/src/deps.rs) is what actually enforces this.
 */
export function resolveChain(graph: Map<string, BlockInfo>, target: BlockAddr): BlockAddr[] {
  const order: BlockAddr[] = [];
  const visited = new Set<string>();
  const stack: string[] = [];

  function visit(addr: BlockAddr) {
    const key = addrKey(addr);
    if (visited.has(key)) return;
    const cycleAt = stack.indexOf(key);
    if (cycleAt !== -1) {
      throw new DepsError(`dependency cycle: ${[...stack.slice(cycleAt), key].join(" -> ")}`);
    }
    const info = graph.get(key);
    if (!info) {
      throw new DepsError(`no runnable block named ${JSON.stringify(addr.blockName)} in node ${JSON.stringify(addr.nodeId)}`);
    }
    stack.push(key);
    for (const dep of info.deps) visit(dep);
    stack.pop();
    visited.add(key);
    order.push(addr);
  }

  visit(target);
  return order;
}

