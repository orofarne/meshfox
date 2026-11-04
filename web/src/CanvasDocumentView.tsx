import type { MouseEvent } from "react";
import { NodeBodyContent, type MeshNodeData } from "./MeshNode";
import type { CanvasNode } from "./types";

/** One link in a section's "Leads to" / "Referenced from" list. */
interface DocLink {
  id: string;
  title: string;
  label?: string;
}

interface DocLinks {
  out: DocLink[];
  in: DocLink[];
}

/**
 * Turns the graph's edges into per-node text links, so the flat document has
 * no arrows to follow by eye. Every `meshfox:edge` (an `extraParents` entry)
 * is listed on both ends. A structural parent→child edge is already the
 * heading hierarchy, so it's listed only when it carries an `edgeLabel`
 * (otherwise a root with thirty children would announce thirty links).
 */
export function collectDocLinks(nodes: readonly CanvasNode[]): Map<string, DocLinks> {
  const titles = new Map(nodes.map((n) => [n.id, n.title]));
  const links = new Map<string, DocLinks>(nodes.map((n) => [n.id, { out: [], in: [] }]));
  const add = (from: string, to: string, label?: string) => {
    const a = links.get(from);
    const b = links.get(to);
    if (!a || !b) return;
    a.out.push({ id: to, title: titles.get(to) ?? to, label });
    b.in.push({ id: from, title: titles.get(from) ?? from, label });
  };
  for (const n of nodes) {
    if (n.parent && n.edgeLabel) add(n.parent, n.id, n.edgeLabel);
    for (const e of n.extraParents ?? []) add(e.from, n.id, e.label);
  }
  return links;
}

/** Scrolls to a section and moves keyboard/screen-reader focus to its heading. */
function jumpTo(e: MouseEvent, id: string) {
  e.preventDefault();
  const heading = document.getElementById(`doc-h-${id}`);
  if (!heading) return;
  heading.scrollIntoView({ block: "start" });
  heading.focus();
}

function LinkList({ heading, items }: Readonly<{ heading: string; items: DocLink[] }>) {
  if (items.length === 0) return null;
  return (
    <div className="mesh-doc-links">
      <strong>{heading}</strong>
      <ul>
        {items.map((l, i) => (
          <li key={`${l.id}-${i}`}>
            <a href={`#doc-${l.id}`} onClick={(e) => jumpTo(e, l.id)}>
              {l.label ? `${l.label}: ${l.title}` : l.title}
            </a>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * The whole canvas as one read-only page: a table of contents, then every
 * node in tree order as a section (heading, the same live body a card shows,
 * and its edges as text links). Positions and sizes play no part.
 */
export function CanvasDocumentView({
  canvasNodes,
  dataById,
}: Readonly<{ canvasNodes: readonly CanvasNode[]; dataById: ReadonlyMap<string, MeshNodeData> }>) {
  const links = collectDocLinks(canvasNodes);
  const sections = canvasNodes.filter((n) => dataById.has(n.id));
  const rootTitle = sections[0]?.title ?? "Document";
  return (
    <main className="mesh-document mesh-canvas-document" aria-label={rootTitle}>
      <div className="mesh-document-content">
        {sections.length > 1 && (
          <nav aria-label="Contents" className="mesh-doc-toc">
            <ul>
              {sections.map((n) => (
                <li key={n.id} style={{ paddingLeft: `${Math.max(0, n.level - 1) * 1.1}rem` }}>
                  <a href={`#doc-${n.id}`} onClick={(e) => jumpTo(e, n.id)}>
                    {n.title}
                  </a>
                </li>
              ))}
            </ul>
          </nav>
        )}
        {sections.map((n) => {
          const data = dataById.get(n.id)!;
          const Heading = `h${Math.min(Math.max(n.level, 1), 6)}` as "h1";
          const l = links.get(n.id);
          return (
            <section key={n.id} id={`doc-${n.id}`} className="mesh-doc-section" data-node-id={n.id} aria-labelledby={`doc-h-${n.id}`}>
              <Heading id={`doc-h-${n.id}`} tabIndex={-1}>
                {n.title}
              </Heading>
              <NodeBodyContent data={data} nodeId={n.id} />
              <LinkList heading="Leads to" items={l?.out ?? []} />
              <LinkList heading="Referenced from" items={l?.in ?? []} />
            </section>
          );
        })}
      </div>
    </main>
  );
}
