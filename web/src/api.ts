import type { CanvasDoc, ExtraEdgeDto, NodeType, VarStatus } from "./types";

export async function fetchCanvas(): Promise<CanvasDoc> {
  const res = await fetch("/api/canvas");
  if (!res.ok) throw new Error(`GET /api/canvas: ${res.status}`);
  return res.json();
}

/**
 * Only the declared `meshfox:var`s `block`'s own chain actually
 * references (via `env=` — a block that declares none gets back an empty
 * list, regardless of how many variables the document declares), each
 * with its current resolve-without-prompting status (env/cache/default —
 * no overrides) — see SPEC.md's "Variables". Call before running a block
 * to find out whether anything still needs asking (`resolved: false`);
 * pass whatever the user answers as `runBlockStream`'s `vars` argument.
 */
export async function fetchVars(path: string[], block: string, withDeps: boolean): Promise<VarStatus[]> {
  const params = new URLSearchParams({ path: path.join(","), block, noDeps: String(!withDeps) });
  const res = await fetch(`/api/vars?${params}`);
  if (!res.ok) throw new Error(`GET /api/vars: ${res.status}`);
  return res.json();
}

/**
 * Every declared *non-secret* `meshfox:var` in the whole document, in
 * declaration order, regardless of which (if any) block's `env=`
 * references it — the browser counterpart to `meshfox configure`, unlike
 * `fetchVars` which is scoped to one block's own chain. Each entry's
 * `resolved`/`value` reflect its current env/cache/default status (no
 * overrides), same as `fetchVars`.
 */
export async function fetchConfigureVars(): Promise<VarStatus[]> {
  const res = await fetch("/api/vars/configure");
  if (!res.ok) throw new Error(`GET /api/vars/configure: ${res.status}`);
  return res.json();
}

/**
 * Saves `answers` (declared non-secret variable name -> value) to the
 * on-disk cache — every entry is written, even one left unchanged from
 * its current suggestion, same as `meshfox configure` always confirming
 * whatever's answered. Doesn't run anything.
 */
export async function saveConfigureVars(answers: Record<string, string>): Promise<void> {
  const res = await fetch("/api/vars/configure", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ vars: answers }),
  });
  if (!res.ok) throw new Error(`POST /api/vars/configure: ${res.status}`);
}

/**
 * Replaces the document's whole set of declared `meshfox:option` names
 * (see SPEC.md's "Options") with exactly `options`, in the given order —
 * an empty array removes every declaration. The write path behind the
 * toolbar's "options" modal; unlike `meshfox:var` (never written by any
 * endpoint), an option is a bare presence flag with nothing to prompt
 * for, so there's no reason not to let the UI toggle it directly.
 */
export async function updateOptions(options: string[]): Promise<CanvasDoc> {
  const res = await fetch("/api/options", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ options }),
  });
  if (!res.ok) throw new Error(`PUT /api/options: ${res.status}`);
  return res.json();
}

export async function saveCanvas(canvas: CanvasDoc): Promise<void> {
  const res = await fetch("/api/canvas", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(canvas),
  });
  if (!res.ok) throw new Error(`PUT /api/canvas: ${res.status}`);
}

/**
 * One `include` reachable from the document (however deeply nested,
 * however the primary document reached it), resolved to the file it
 * points at but without splicing its content in — powers Source mode's
 * file picker (`includeNodeId` below), alongside the implicit "this
 * document" option that isn't in this list. `depth` is 0 for an include
 * declared directly in the primary document, 1 for one nested inside a
 * depth-0 include's own target, and so on — enough to indent a flat list
 * into a tree client-side. `isCanvas` is `false` for a plain-Markdown
 * target (nothing to open in Source mode as its own file — see
 * `NodeSettings`' read-only note on that case) or a broken/cyclic one.
 */
export interface IncludeManifestEntry {
  nodeId: string;
  title: string;
  target: string;
  depth: number;
  isCanvas: boolean;
}

export async function fetchIncludes(): Promise<IncludeManifestEntry[]> {
  const res = await fetch("/api/includes");
  if (!res.ok) throw new Error(`GET /api/includes: ${res.status}`);
  return res.json();
}

/** The raw Markdown text of the document itself, verbatim — what the
 * toolbar's "Source" mode edits by default. Pass an `IncludeManifestEntry`'s
 * `nodeId` (from `fetchIncludes`) to read that include's own target file
 * instead. */
export async function fetchCanvasSource(includeNodeId?: string): Promise<string> {
  const url = includeNodeId ? `/api/canvas/raw?include=${encodeURIComponent(includeNodeId)}` : "/api/canvas/raw";
  const res = await fetch(url);
  if (!res.ok) throw new Error(`GET /api/canvas/raw: ${res.status}`);
  return res.text();
}

/**
 * Overwrites the whole document (or, with `includeNodeId`, an include
 * target's own file — see `fetchCanvasSource`) with `text`, verbatim. The
 * server rejects (422, nothing written) anything that doesn't parse — the
 * thrown error's message is the parser's, suitable to show right next to
 * Source mode's Save button so an invalid edit is never silently lost or
 * half-applied.
 */
export async function saveCanvasSource(text: string, includeNodeId?: string): Promise<void> {
  const url = includeNodeId ? `/api/canvas/raw?include=${encodeURIComponent(includeNodeId)}` : "/api/canvas/raw";
  const res = await fetch(url, {
    method: "PUT",
    headers: { "content-type": "text/plain" },
    body: text,
  });
  if (!res.ok) {
    const msg = await res.text();
    throw new Error(msg || `PUT /api/canvas/raw: ${res.status}`);
  }
}

/**
 * Adds a new, empty-bodied child node under `parentId`, titled `title` —
 * lands as the last item in the parent's existing subtree, with no
 * position set (the web client's own auto-layout, see `./autolayout.ts`,
 * places it same as any other position-less node, until it's dragged into
 * a real one). Returns the fresh canvas (same shape
 * `fetchCanvas` returns) so the caller can just `setCanvas` with it
 * directly.
 */
export async function createNode(parentId: string, title: string): Promise<CanvasDoc> {
  const res = await fetch("/api/nodes", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ parentId, title }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes: ${res.status}`);
  }
  return res.json();
}

/** Only the fields actually present are changed — see `updateNode`. */
export interface NodePatch {
  title?: string;
  nodeType?: NodeType;
  color?: string;
  /** New link target for a `file`/`link` node (replaces its whole body). */
  target?: string;
  /** New raw Markdown body for a `text` node. */
  text?: string;
  /** Full replacement list of extra incoming edges (`meshfox:edge`) — omit
   * to leave them untouched, pass `[]` to remove them all. */
  extraParents?: ExtraEdgeDto[];
  /** file-node display mode — see `CanvasNode.display`. */
  display?: "link" | "code";
  /** file-node syntax-highlighting language hint — see `CanvasNode.lang`. */
  lang?: string;
  /** file-node interpreter — see `CanvasNode.interpreter`. */
  interpreter?: string;
  /** link-node social preview toggle — see `CanvasNode.preview`. */
  preview?: boolean;
  /** Structural-edge label — see `CanvasNode.edgeLabel`. Omit to leave it
   * untouched; an explicit `""` clears it back to unset (see this field's
   * own handling in the server's `update_node`) — unlike `fold`, there's
   * no separate sentinel needed here since a caller only ever sends this
   * key at all when the label actually changed (see DeletableEdge.tsx). */
  edgeLabel?: string;
  /** Full replacement list of tags — omit to leave them untouched, pass
   * `[]` to clear them. */
  tags?: string[];
  /** Per-node fold-state override — see `CanvasNode.fold`. Omit to leave
   * it untouched; otherwise a string sentinel (not a plain boolean,
   * matching the server's own `UpdateNodeRequest.fold`): `"true"`/
   * `"false"` sets an explicit override, `"default"` clears it back to
   * following the document's own default. Plain JSON `null` can't stand
   * in for "clear this back to unset" here — it's indistinguishable from
   * "omitted" to the server's usual `Option<T>` handling — hence the
   * sentinel string. */
  fold?: "true" | "false" | "default";
}

/**
 * Applies `patch` to node `id` — title/type/color/target/text/extraParents
 * are all independently optional, so a caller only ever sends what it
 * actually changed. The server validates the fully-patched document parses
 * before saving anything (e.g. `nodeType: "group"` on a node with a
 * non-empty body is rejected, 422, with nothing written) — surfaces as a
 * thrown error carrying the server's message, same as `saveCanvas`.
 */
export async function updateNode(id: string, patch: NodePatch): Promise<CanvasDoc> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}`, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(patch),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `PATCH /api/nodes/${id}: ${res.status}`);
  }
  return res.json();
}

/**
 * Deletes `id` — the root is rejected by the server (422) rather than
 * producing a rootless document. `mode` picks what happens to `id`'s direct
 * children: `"subtree"` (default) deletes them too, along with every
 * descendant (`mdcanvas::delete_node`); `"reparent"` promotes them to `id`'s
 * own parent instead, leaving their own subtrees otherwise untouched
 * (`mdcanvas::delete_node_reparent_children`). Either way, any
 * `meshfox:edge` elsewhere that pointed at `id` itself is dropped too.
 */
export async function deleteNode(id: string, mode: "subtree" | "reparent" = "subtree"): Promise<CanvasDoc> {
  const params = mode === "reparent" ? "?children=reparent" : "";
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}${params}`, { method: "DELETE" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `DELETE /api/nodes/${id}: ${res.status}`);
  }
  return res.json();
}

export interface NodeFileContent {
  content: string;
  /** `true` if the file was larger than the server's preview cap and
   * `content` is only its leading portion. */
  truncated: boolean;
}

/**
 * Reads a `file` node's target off disk, fresh, for its `display="code"`
 * preview — never cached client-side, since the underlying file can change
 * between renders. Rejects (thrown error) for a non-file node, a node with
 * no target, a target outside the canvas directory, a missing file, or one
 * that looks binary — the caller falls back to the plain link view in every
 * one of those cases.
 */
export async function fetchNodeFileContent(id: string): Promise<NodeFileContent> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}/file-content`);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `GET /api/nodes/${id}/file-content: ${res.status}`);
  }
  return res.json();
}

export interface LinkPreview {
  title?: string;
  description?: string;
  image?: string;
}

/**
 * Fetches (or returns the server's already-cached) OpenGraph preview for
 * `url` — used by `LinkPreviewCard` for a `link` node with `preview: true`.
 * Never throws for a fetch/SSRF failure on the server side (that's just
 * `{ preview: null }`, meaning "nothing to show"); only throws for an
 * actual request-level failure (network error, non-2xx from the endpoint
 * itself).
 */
export async function fetchLinkPreview(url: string): Promise<LinkPreview | null> {
  const res = await fetch(`/api/link-preview?url=${encodeURIComponent(url)}`);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `GET /api/link-preview: ${res.status}`);
  }
  const data: { preview: LinkPreview | null } = await res.json();
  return data.preview;
}

/**
 * Deletes `id`'s structural (nesting) parent edge, promoting its existing
 * extra edge from `newParentId` to take its place — `newParentId` must
 * already be one of `id`'s extra parents (see `CanvasNode.extraParents`),
 * the server rejects (422) anything else, same as it does for a cycle
 * (`newParentId` being `id` itself or one of its own descendants) or `id`
 * being the root (see `mdcanvas::reparent_node`).
 */
export async function reparentNode(id: string, newParentId: string): Promise<CanvasDoc> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}/reparent`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ newParentId }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${id}/reparent: ${res.status}`);
  }
  return res.json();
}

/**
 * Drops `id`'s own authored `x`/`y`/`w`/`h`, reverting it to auto-placement
 * (`mdcanvas::set_node_meta` with position/size fields unset) — every other
 * field (color/type/tags/...) is preserved exactly. The web UI's own
 * per-node counterpart to the toolbar's whole-document "Auto-layout"
 * button (`clearLayout` below) — see `MeshNode.tsx`'s ↺ button, shown only
 * for a node that actually has a position to clear.
 */
export async function clearNodeLayout(id: string): Promise<CanvasDoc> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}/clear-layout`, { method: "POST" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${id}/clear-layout: ${res.status}`);
  }
  return res.json();
}

/**
 * Moves `id`'s whole subtree to sit immediately before or after another
 * sibling under the same structural parent (`mdcanvas::move_sibling`) — an
 * auto-placed node's only lever for changing its own order among siblings,
 * since it has no `x`/`y` to drag (see `MeshNode.tsx`'s ↑/↓ buttons, shown
 * only for one of those). Pass exactly one of `before`/`after`. The server
 * rejects (thrown error) two nodes that aren't siblings.
 */
export async function moveSibling(
  id: string,
  target: { before: string } | { after: string },
): Promise<CanvasDoc> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}/move`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(target),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${id}/move: ${res.status}`);
  }
  return res.json();
}

/**
 * Changes `id`'s own id to `newId` — rewrites every reference the server
 * tracks structurally (other nodes' `parent=`/`meshfox:edge from=`), plus
 * best-effort text rewrites of `deps="id/block"` fence references
 * elsewhere in the document. The server rejects (thrown error) an empty
 * `newId`, one containing a `"` character, or one already used by another
 * node — nothing is written in any of those cases.
 */
export async function renameNodeId(id: string, newId: string): Promise<CanvasDoc> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}/rename-id`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ newId }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${id}/rename-id: ${res.status}`);
  }
  return res.json();
}

/**
 * Drops `id`'s own explicit id, handing it back to the parser's title-slug
 * fallback (no `id=` attribute in the `meshfox:node` comment at all, same
 * as a hand-written one that never had one) — the "leave the ID field
 * empty" case in `NodeSettings`. Can't fail the way `renameNodeId` can
 * (empty/invalid/colliding): the derived id is always a fresh slug of the
 * node's own title, deduplicated server-side. Returns the id the node
 * actually ends up with — usually unchanged (an untouched id is already
 * `slug(title)`), but the caller (same "id might have just changed out
 * from under this component" situation `renameNodeId` already has) needs
 * to know for sure rather than assume.
 */
export async function clearNodeId(id: string): Promise<{ id: string; doc: CanvasDoc }> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(id)}/clear-id`, { method: "POST" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${id}/clear-id: ${res.status}`);
  }
  const body: { id: string; canvas: CanvasDoc } = await res.json();
  return { id: body.id, doc: body.canvas };
}

// Mirrors crates/server/src/lib.rs's `RunEvent` (JSON shape, camelCase) —
// one of these per line of /api/run's streamed `application/x-ndjson`
// response body. `started` is always first; `killed`/`error`/`done` are
// each terminal for the run (no further lines follow).
export type RunEvent =
  | { type: "started"; runId: string }
  | { type: "step-start"; nodeId: string; block: string }
  /** Terminal for this one step (the chain keeps going) — this dependency
   * already ran successfully earlier in the same `meshfox view` session and
   * hasn't changed since, so it wasn't actually re-run. Never emitted for
   * the block actually requested, only a pulled-in `deps=`/`from=`
   * dependency. See SPEC.md's "Runnable code fences". */
  | { type: "step-skipped"; nodeId: string; block: string }
  | { type: "output"; nodeId: string; block: string; text: string }
  | { type: "step-end"; nodeId: string; block: string; exitCode: number; durationMs: number }
  | { type: "killed"; nodeId: string; block: string }
  | { type: "error"; message: string }
  | { type: "done"; exitCode: number };

/**
 * Running is always allowed. `persist` controls whether a `cache`d block's
 * output actually gets written into the file — pass `false` (e.g. outside
 * Edit mode) to see the result without touching anything on disk.
 * `withDeps` controls whether `block`'s `deps=` chain runs first (the "⛓
 * run chain" button) or just `block` itself (the plain "run" button).
 *
 * The response streams as it happens (see SPEC.md's "Runnable code
 * fences") — `onEvent` is called once per line, in order, as each arrives,
 * not all at once at the end. Resolves once the stream closes; rejects
 * only for a failure before any of that started (chain resolution — a
 * dangling block, a cycle — reported as a normal HTTP error status by the
 * server since nothing has run yet at that point). A failure *after*
 * streaming began shows up as an `"error"` (or `"killed"`) event instead,
 * not a rejection — `onEvent` is where those need handling.
 */
export async function runBlockStream(
  path: string[],
  block: string,
  persist: boolean,
  withDeps: boolean,
  onEvent: (event: RunEvent) => void,
  vars?: Record<string, string>,
): Promise<void> {
  const res = await fetch("/api/run", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ path, block, persist, noDeps: !withDeps, vars: vars ?? {} }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/run: ${res.status}`);
  }
  if (!res.body) {
    throw new Error("POST /api/run: response had no body to stream");
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffered = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffered += decoder.decode(value, { stream: true });
    let newlineAt: number;
    while ((newlineAt = buffered.indexOf("\n")) >= 0) {
      const line = buffered.slice(0, newlineAt);
      buffered = buffered.slice(newlineAt + 1);
      if (line.trim()) onEvent(JSON.parse(line) as RunEvent);
    }
  }
}

/**
 * Runs a runnable `file` node's `interpreter target` (see
 * `CanvasNode.interpreter`) — the counterpart to `runBlockStream` for a node
 * that has no fenced code of its own, just a target file on disk. Streams
 * the same `RunEvent` shape (`nodeId`/`block` both set to `nodeId` itself,
 * same "the block shares its node's own id" convention a `text` node's sole
 * implicit block already uses), registered the same way in the server's
 * run registry — `killRun` works on it unchanged. No `withDeps`/`persist`:
 * a `file` node has no `deps=` chain or cache to opt into.
 */
export async function runFileStream(nodeId: string, onEvent: (event: RunEvent) => void): Promise<void> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(nodeId)}/run`, { method: "POST" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${nodeId}/run: ${res.status}`);
  }
  if (!res.body) {
    throw new Error(`POST /api/nodes/${nodeId}/run: response had no body to stream`);
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffered = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffered += decoder.decode(value, { stream: true });
    let newlineAt: number;
    while ((newlineAt = buffered.indexOf("\n")) >= 0) {
      const line = buffered.slice(0, newlineAt);
      buffered = buffered.slice(newlineAt + 1);
      if (line.trim()) onEvent(JSON.parse(line) as RunEvent);
    }
  }
}

/**
 * Opens a `file` node's target in the OS's default application for it (the
 * web UI's "↗ open" button) — best-effort, resolves once the opener has
 * been spawned, not once whatever it opened has itself finished loading.
 * Rejects (thrown error) for a non-file node, a node with no target, or a
 * target outside the canvas directory.
 */
export async function openNodeFile(nodeId: string): Promise<void> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(nodeId)}/open`, { method: "POST" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${nodeId}/open: ${res.status}`);
  }
}

/**
 * Cancels an in-flight run started by `runBlockStream` (`runId` comes from
 * that stream's first `"started"` event) — kills whichever block is
 * currently executing and stops the rest of its dependency chain. A 404
 * (already finished, or an unknown id) is treated the same as success:
 * either way, there's nothing left to kill.
 */
export async function killRun(runId: string): Promise<void> {
  const res = await fetch("/api/kill", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ runId }),
  });
  if (!res.ok && res.status !== 404) {
    throw new Error(`POST /api/kill: ${res.status}`);
  }
}

/**
 * Clears every non-group node's stored `x`/`y`/`width`/`height` back to
 * unset, reverting the whole document to auto-placed (see
 * `./autolayout.ts`) — irreversible except by undoing the file change some
 * other way. Returns the fresh canvas (same shape `fetchCanvas` returns),
 * ready to `setCanvas` with directly.
 */
export async function clearLayout(): Promise<CanvasDoc> {
  const res = await fetch("/api/canvas/clear-layout", { method: "POST" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/canvas/clear-layout: ${res.status}`);
  }
  return res.json();
}

/**
 * Delays (ms) between reconnect attempts after `/api/watch` drops — see
 * `watchChanges` below. Cumulative sum (~9.6s) deliberately lands just under
 * the server's own `AUTO_EXIT_GRACE` (10s, see server's `lib.rs`): as long
 * as reconnecting succeeds within that window, the server never even
 * considers itself abandoned, so both sides agree on when a drop was real.
 */
const WATCH_RECONNECT_DELAYS_MS = [200, 400, 800, 1600, 3200, 3200];

/**
 * Opens a long-lived connection to `/api/watch` (NDJSON, one line per event)
 * for as long as this tab stays open, transparently reconnecting (see
 * `WATCH_RECONNECT_DELAYS_MS`) whenever the connection drops. The server
 * counts each open connection as one open tab and, once every one of them
 * has stayed gone past its own grace period, exits on its own (see
 * README's roadmap) — so this is meant to be called once, for the lifetime
 * of the page, not per-request.
 *
 * `onChanged` fires for each `"changed"` event: the on-disk file changed
 * from underneath the server (an external edit), so the caller should
 * reload. `onDisconnected` fires only once every reconnect attempt has
 * failed — that's this function's best guess that the server process itself
 * has actually stopped, since there's nothing left on the other end to keep
 * it open, as opposed to a connection that merely dropped and can be
 * re-established. Treating every single drop as fatal used to close the tab
 * too eagerly in two situations that are both just a transient drop, not a
 * dead server: waking a sleeping/hibernated laptop (the loopback socket can
 * come back looking reset even though the server process never exited), and
 * — on Firefox specifically — refreshing the tab, where the outgoing page's
 * fetch can observe the connection die before its own `pagehide` handler
 * (below) has had a chance to mark it as leaving. Retrying instead of
 * reacting immediately gives both cases a chance to resolve themselves: the
 * hibernate case by the retry simply succeeding once the socket is usable
 * again, the refresh case because the retry is scheduled with `setTimeout`
 * on a page that's already being torn down by the navigation, so it never
 * actually fires. Returns a function that stops watching (aborts the
 * underlying request and any pending retry) without itself triggering
 * `onDisconnected`.
 *
 * A reload (or any other navigation away from this tab) also closes the
 * connection out from under the fetch, from the browser's side, not this
 * function's own `AbortController` — indistinguishable, by error alone,
 * from the server itself actually having died. `pagehide` fires first in
 * the common case (reload, back/forward, closing the tab), so it's used
 * here to tell "this tab is leaving" apart from "the server is gone":
 * without it, a plain reload would misread its own connection drop as the
 * server having stopped and (see App.tsx's `serverGone`) try to close the
 * very tab that's mid-reload instead of letting it finish. The reconnect
 * retries above are the backstop for when `pagehide` loses that race.
 */
export function watchChanges(onChanged: () => void, onDisconnected: () => void): () => void {
  const controller = new AbortController();
  let leaving = false;
  let stopped = false;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  const markLeaving = () => {
    leaving = true;
  };
  window.addEventListener("pagehide", markLeaving);

  const connectOnce = async (onEstablished: () => void): Promise<void> => {
    const res = await fetch("/api/watch", { signal: controller.signal });
    if (!res.ok || !res.body) {
      throw new Error(`GET /api/watch: ${res.status}`);
    }
    // A response is in hand — this attempt reached the server, so any
    // future drop is a fresh problem and should restart the backoff from
    // its shortest delay rather than resume wherever a much earlier,
    // unrelated attempt left off.
    onEstablished();
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffered = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffered += decoder.decode(value, { stream: true });
      let newlineAt: number;
      while ((newlineAt = buffered.indexOf("\n")) >= 0) {
        const line = buffered.slice(0, newlineAt);
        buffered = buffered.slice(newlineAt + 1);
        if (!line.trim()) continue;
        const event = JSON.parse(line) as { type: string };
        if (event.type === "changed") onChanged();
      }
    }
    // The stream ending is itself a drop (the server never sends a
    // deliberate "goodbye" event) — fall through to the retry logic below
    // exactly like a network error would.
    throw new Error("GET /api/watch: stream ended");
  };

  const run = (attempt: number) => {
    let established = false;
    connectOnce(() => {
      established = true;
    }).catch(() => {
      if (leaving || stopped || controller.signal.aborted) return;
      const nextAttempt = established ? 0 : attempt;
      if (nextAttempt >= WATCH_RECONNECT_DELAYS_MS.length) {
        onDisconnected();
        return;
      }
      retryTimer = setTimeout(() => run(nextAttempt + 1), WATCH_RECONNECT_DELAYS_MS[nextAttempt]);
    });
  };
  run(0);

  return () => {
    stopped = true;
    window.removeEventListener("pagehide", markLeaving);
    if (retryTimer !== undefined) clearTimeout(retryTimer);
    controller.abort();
  };
}
