import { parseBody } from './fence.ts';
import type { CanvasNode } from './types';
import type { LiveBlockState } from './MeshNode';

/** Keep displayed output across document edits, scoped to the worker and
 * addresses that still exist. Parameterized applications belong to their
 * source block even though they have distinct run addresses. */
export function retainLiveBlocks(
  node: CanvasNode,
  serverSession: string,
  previous?: { serverSession: string; liveBlocks: Record<string, LiveBlockState> },
): Record<string, LiveBlockState> {
  if (!previous || previous.serverSession !== serverSession) return {};
  const names = new Set(parseBody(node.text, node.id)
    .flatMap(seg => seg.type === 'code' ? [seg.name] : []));
  if (node.type === 'file') names.add(node.id);
  return Object.fromEntries(Object.entries(previous.liveBlocks).filter(([address]) =>
    names.has(address) || names.has(address.split('[')[0])));
}
