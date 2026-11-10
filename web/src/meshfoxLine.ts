/**
 * Colors meshfox's *own* lines in a block's output, the way `meshfox run`
 * does in a terminal (bold cyan `==>` banners, green/red step results).
 *
 * A native `meshfox` call fence relays the child canvas's steps as ordinary
 * text lines (`==> child.canvas.md → build`, `<== build (exit 0)`), with the
 * child's raw event attached as `child` — the text stays plain so logs and
 * `meshfox status` read cleanly, and each client decides how to show it.
 * Program output (`"output"` events) is never touched: whatever ANSI it
 * carries itself is `AnsiText`'s job.
 */

const BANNER = "\x1b[1;36m";
const OK = "\x1b[32m";
const FAIL = "\x1b[31m";
const RESET = "\x1b[0m";

type ChildMeta = { canvas: string; port: number; event: unknown };

/** The event at the bottom of a chain of nested calls: a call inside a call
 * relays its child's line again, carrying that child's own meta. */
function innermostEvent(child: ChildMeta): { type?: string; exitCode?: number } | undefined {
  let event = child.event as { type?: string; exitCode?: number; child?: ChildMeta } | undefined;
  while (event && typeof event === "object" && event.child && typeof event.child === "object") {
    event = event.child.event as typeof event;
  }
  return event && typeof event === "object" ? event : undefined;
}

/** `text` wrapped in the SGR color meshfox's CLI would use for that line, or
 * unchanged when it is not one of meshfox's own lines (or already colored). */
export function colorMeshfoxLine(text: string, child: ChildMeta | undefined): string {
  if (!child || !text || text.includes("\x1b")) return text;
  const event = innermostEvent(child);
  switch (event?.type) {
    case "step-start":
    case "step-skipped":
    case "service-started":
      return `${BANNER}${text}${RESET}`;
    case "step-end":
      return `${event.exitCode === 0 ? OK : FAIL}${text}${RESET}`;
    default:
      return text;
  }
}
