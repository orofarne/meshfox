import type { CanvasDoc, CanvasNode, ExtraEdgeDto, NodeType, ServiceStatusDto, VarStatus } from "./types";

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

/** One field of a `form`-lang fence's own `GET /api/form/fields` response
 * — a declared variable's usual `VarStatus` plus whatever `label=`
 * override the field itself carries (falls back to `prompt`/`name` when
 * absent, same as `meshfox_core::form::FormField.label`). */
export type FormFieldStatus = VarStatus & { label?: string };

/**
 * Resolves one `form`-lang fence's own `field var=` list into display-
 * ready status, server-side (the server re-derives the field list itself
 * from the canvas — see `crates/server/src/lib.rs`'s `get_form_fields` —
 * rather than trusting anything the client might claim). `send` is the
 * fence's own `send=` caption, already defaulted to `"Send"` server-side.
 * Addressed by flat `nodeId` (the same possibly-namespaced id `GET
 * /api/canvas` already sends), not a root-relative `path` the way
 * `fetchVars`/`runBlockStream` address a block — this endpoint (like
 * `submitForm`) never needs to know which file on disk owns the node, so
 * there's nothing a path would add.
 */
export async function fetchFormFields(
  nodeId: string,
  block: string,
): Promise<{ send: string; fields: FormFieldStatus[] }> {
  const params = new URLSearchParams({ nodeId, block });
  const res = await fetch(`/api/form/fields?${params}`);
  if (!res.ok) throw new Error(`GET /api/form/fields: ${res.status}`);
  return res.json();
}

/**
 * The submit side of a `form`-lang fence's own Send button (see SPEC.md's
 * "Form fences"): commits `values` into the server's session-lifetime
 * override store (never the on-disk cache — every variable a form targets
 * is implicitly `session`-scoped) and kicks off whichever `autorun` blocks
 * the just-changed values reach. `values`' keys not among this specific
 * form's own declared fields are silently ignored server-side — the same
 * defensive posture `POST /api/vars/configure` already has toward an
 * unrecognized name. `autorunTriggered` is every block address the
 * server just started in the background — pass each to `subscribeRun`
 * (or fold into `liveBlocks` the same way, see `App.tsx`'s
 * `watchAutorunBlock`) to watch it without waiting on a `"run-started"`
 * `/api/watch` event, which exists for a passive tab that didn't submit
 * this form itself.
 */
export async function submitForm(
  nodeId: string,
  block: string,
  values: Record<string, string>,
): Promise<{ saved: number; autorunTriggered: { nodeId: string; block: string }[] }> {
  const res = await fetch("/api/form/submit", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ nodeId, block, values }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/form/submit: ${res.status}`);
  }
  return res.json();
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

/**
 * `layoutHints` — a same-request-only sort hint for the server's own
 * `mdcanvas::reorder_by_position` (see its doc comment), keyed by node id:
 * this tab's own current on-screen position for a node it did *not* just
 * drag/resize this save (still auto-placed). Without it, a lone freshly-
 * positioned node among otherwise-auto siblings always sorts before every
 * one of them regardless of its own `y` (App.tsx's `handleSaveLayout`
 * builds this from `nodes`, which always has a real numeric position for
 * every node, positioned or not). Never persisted as real `x`/`y` on the
 * nodes it's about — purely advisory for this one save's reorder pass.
 */
export async function saveCanvas(
  canvas: CanvasDoc,
  layoutHints?: Record<string, { x: number; y: number }>,
): Promise<void> {
  const res = await fetch("/api/canvas", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...canvas, layoutHints: layoutHints ?? {} }),
  });
  if (!res.ok) throw new Error(`PUT /api/canvas: ${res.status}`);
}

/**
 * One `include` declared directly in the document, resolved to the file it
 * points at but without dumping its content in — powers Source mode's file
 * picker (`includeNodeId` below), alongside the implicit "this document"
 * option that isn't in this list.
 */
export interface IncludeManifestEntry {
  nodeId: string;
  title: string;
  target: string;
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
 * target's own file — see `fetchCanvasSource`) with `text`. The worker cleans
 * uncached output from the primary canvas; callers reload after saving. The
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

export interface SyntaxGrammarEntry {
  /** Filename — also what `fetchSyntaxGrammar` expects. */
  name: string;
  /** `"local"` (`.meshfox/syntax/` next to the canvas) or `"global"`
   * (`~/.meshfox/syntax/`). */
  source: "local" | "global";
}

/**
 * Custom syntax-highlighting grammars available to this canvas — the same
 * `.meshfox/syntax/`/`~/.meshfox/syntax/` repository the TUI's own
 * `syntect::parsing::SyntaxSet` loads from (see `meshfox_core::syntax_dirs`
 * and `crate::syntax_registry` on the Rust side). Used by `shiki.ts` to
 * register anything Shiki's own bundled language set doesn't already cover.
 */
export async function fetchSyntaxList(): Promise<SyntaxGrammarEntry[]> {
  const res = await fetch("/api/syntax");
  if (!res.ok) throw new Error(`GET /api/syntax: ${res.status}`);
  return res.json();
}

/** Raw content (JSON or YAML text) of one entry from `fetchSyntaxList`. */
export async function fetchSyntaxGrammar(name: string): Promise<string> {
  const res = await fetch(`/api/syntax/${encodeURIComponent(name)}`);
  if (!res.ok) throw new Error(`GET /api/syntax/${name}: ${res.status}`);
  return res.text();
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
// one of these per WebSocket text frame /api/run's streamed response sends.
// `started` is (almost) always first; `killed`/`error`/`done`/
// `lock-conflict` are each terminal for the run (no further messages
// follow).
export type RunEvent =
  | { type: "started"; runId: string }
  | { type: "step-start"; nodeId: string; block: string }
  /** Terminal for this one step (the chain keeps going) — this dependency
   * already ran successfully earlier in the same `meshfox view` session and
   * hasn't changed since, so it wasn't actually re-run. Never emitted for
   * the block actually requested, only a pulled-in `deps=`/`from=`
   * dependency. See SPEC.md's "Runnable code fences". `output`/`durationMs`
   * are whatever that earlier real run produced — there's no fresh output
   * from *this* run to show, so the client shows that instead (see
   * `MeshNode.tsx`'s `LiveRunOutput`, typically collapsed by default). */
  | { type: "step-skipped"; nodeId: string; block: string; output: string; durationMs: number }
  /** `stream` says which pipe this line actually came from — the two
   * remain interleaved on this one event stream in roughly their real
   * emission order (`stream_exec::OutputStream`'s own doc comment: two
   * separate pipes, no ordering guarantee between them), same as before
   * this field existed; `App.tsx`'s `"output"` handling uses it only to
   * *additionally* accumulate `stdoutText`/`stderrText` on `LiveBlockState`
   * (`MeshNode.tsx`), for `output="markdown"` mode's live view. */
  | { type: "output"; nodeId: string; block: string; stream: "stdout" | "stderr"; text: string }
  | { type: "step-end"; nodeId: string; block: string; exitCode: number; durationMs: number }
  | { type: "killed"; nodeId: string; block: string }
  | { type: "error"; message: string }
  | { type: "done"; exitCode: number }
  /** Terminal for this one step (chain keeps going), a `service` block's
   * equivalent of `step-end` — "done" for a service is "spawned", not
   * "exited", so there's no `exitCode`/`durationMs` here. **Experimental**,
   * see SPEC.md's "Service blocks (experimental)". */
  | { type: "service-started"; nodeId: string; block: string; pid: number }
  /** Terminal for the whole run (no `done` follows — same as `killed`) —
   * `nodeId`/`block`'s own address (any kind of block, not just
   * `service` — a chain's own dependency can just as easily be the one
   * that's contested) is already locked by another live-or-stale process.
   * Queued-time locking means this is always known before any step
   * actually runs, so it's (almost) always the very first message on the
   * socket — a browser `WebSocket` can't read a pre-upgrade HTTP status/
   * body at all, so the server always completes the upgrade and reports
   * this as a stream event instead (see `crates/server/src/lib.rs`'s own
   * `pump_run_response_into_ws` doc comment) rather than a rejected
   * connection. Show a confirm dialog; on confirm, call `forceRun`. */
  | { type: "lock-conflict"; nodeId: string; block: string; ownerPid: number; ownerDesc: string };

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
  /** Names of `secret`-declared variables (a subset of `vars`' own keys) to
   * persist to the on-disk var cache anyway, in plaintext — see
   * `VarsForm`'s own "save (plaintext)" checkbox. */
  saveSecrets?: string[],
): Promise<void> {
  const params = new URLSearchParams({
    path: path.join(","),
    block,
    persist: String(persist),
    noDeps: String(!withDeps),
    vars: JSON.stringify(vars ?? {}),
    saveSecrets: JSON.stringify(saveSecrets ?? []),
  });
  await openEventSocket(wsUrl(`/api/run?${params}`), onEvent);
}

/** Builds a `ws(s)://`-scheme URL for a path on this same origin — every
 * run-starting endpoint is a WebSocket now, not a plain `fetch()`, but
 * still lives at a relative `/api/...` path the same way the old HTTP
 * calls did, so this just swaps the scheme rather than hardcoding a host. */
function wsUrl(path: string): string {
  const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${window.location.host}${path}`;
}

/** Opens `url` as a WebSocket, calls `onEvent` for every JSON text frame it
 * sends, and resolves once the socket closes — the shared plumbing behind
 * `runBlockStream`/`forceRun`/`runFileStream`/`subscribeRun`, now that all
 * four are WebSocket endpoints instead of HTTP-streamed responses (see
 * `crates/server/src/lib.rs`'s own `pump_run_response_into_ws` doc comment
 * for why even a pre-stream failure — a bad chain, a lock conflict, a
 * missing run to subscribe to — arrives as an ordinary message instead of
 * a rejected connection: a browser `WebSocket` has no way to read a failed
 * handshake's own status/body at all). Only actually rejects for a
 * genuine transport-level failure (the worker isn't listening, a
 * malformed URL) — an `onerror` that fires before the socket ever closes. */
function openEventSocket<T>(url: string, onEvent: (event: T) => void): Promise<void> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const ws = new WebSocket(url);
    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") onEvent(JSON.parse(ev.data) as T);
    };
    ws.onerror = () => {
      if (!settled) {
        settled = true;
        reject(new Error(`${url}: WebSocket error`));
      }
    };
    ws.onclose = () => {
      if (!settled) {
        settled = true;
        resolve();
      }
    };
  });
}

/**
 * The confirm side of a `"lock-conflict"` event, generalized to
 * any block kind (not just `service` — see `crates/server/src/lib.rs`'s
 * own `force_run` doc comment): force-kills whatever the conflict's own
 * lock names, then re-runs the *exact same* request that hit it —
 * `path`/`block`/`persist`/`withDeps`/`vars`/`saveSecrets` all mirror
 * `runBlockStream`'s own arguments for the original run: `force` is the
 * one specific `{nodeId, block}` address the conflict named (not
 * necessarily `block` itself — a chain's own dependency can just as
 * easily be the one that's contested). Streams the same way
 * `runBlockStream` does; can itself resolve with *another* conflict (a
 * different address, or a fresh race) for the caller to offer forcing
 * again.
 */
export async function forceRun(
  path: string[],
  block: string,
  persist: boolean,
  withDeps: boolean,
  onEvent: (event: RunEvent) => void,
  force: { nodeId: string; block: string },
  vars?: Record<string, string>,
  saveSecrets?: string[],
): Promise<void> {
  const params = new URLSearchParams({
    path: path.join(","),
    block,
    persist: String(persist),
    noDeps: String(!withDeps),
    vars: JSON.stringify(vars ?? {}),
    saveSecrets: JSON.stringify(saveSecrets ?? []),
    forceNodeId: force.nodeId,
    forceBlock: force.block,
  });
  await openEventSocket(wsUrl(`/api/run/force?${params}`), onEvent);
}

/** One line of `subscribeRun`'s own streamed NDJSON response — a much
 * smaller vocabulary than `RunEvent` (see `crates/server/src/lib.rs`'s own
 * `SubscribeEvent`): this only ever watches one address's own
 * `run_registry::RunHandle`, independent of whatever chain/request
 * originally started it. */
export type SubscribeEvent =
  | { type: "line"; seq: number; stream: "stdout" | "stderr"; text: string }
  | { type: "done"; outcome: "exited" | "killed"; exitCode?: number };

/**
 * Watches one address's own most recent plain-block run — replays every
 * buffered line at or after `sinceSeq` (`0` for "everything still
 * buffered"), then tails live output until the run's own terminal outcome,
 * at which point the stream ends. Resolves normally (no events at all) if
 * this address has never been run, or its run has since been superseded
 * by a fresh one under a different registry entry — `404` is treated the
 * same as "nothing to show", not an error, since a caller reconciling
 * every block on page load can't tell in advance which ones are actually
 * live.
 */
export async function subscribeRun(
  nodeId: string,
  block: string,
  sinceSeq: number,
  onEvent: (event: SubscribeEvent) => void,
): Promise<void> {
  const params = new URLSearchParams({ nodeId, block, sinceSeq: String(sinceSeq) });
  await openEventSocket(wsUrl(`/api/run/subscribe?${params}`), onEvent);
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
  await openEventSocket(wsUrl(`/api/nodes/${encodeURIComponent(nodeId)}/run`), onEvent);
}

/**
 * Opens a `file` node's target — the web UI's "↗ open" button. Always
 * fire-and-forget (`204`, nothing returned): a plain file goes to the OS's
 * default application for it, best-effort, resolving once the opener has
 * been spawned, not once whatever it opened has itself finished loading. A
 * `.canvas.md` target has no such OS association to hand off to — the
 * server instead asks this worker's own watcher (see
 * `meshfox_server::watcher_protocol`) to get-or-spawn-and-show it, opening
 * the browser tab itself; there's no URL for this call to hand back and
 * `window.open` into a tab any more. Rejects (thrown error) for a non-file
 * node, a node with no target, a target outside the canvas directory, or
 * (canvas targets only) no watcher to ask at all.
 */
export async function openNodeFile(nodeId: string): Promise<void> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(nodeId)}/open`, { method: "POST" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${nodeId}/open: ${res.status}`);
  }
}

/**
 * Opens the directory containing a `file` node's target — the web UI's
 * "show folder" icon. Same fire-and-forget shape as `openNodeFile` above,
 * just always handed off to the OS's default file manager for the
 * containing folder rather than the target itself.
 */
export async function openNodeFileFolder(nodeId: string): Promise<void> {
  const res = await fetch(`/api/nodes/${encodeURIComponent(nodeId)}/open-folder`, { method: "POST" });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/nodes/${nodeId}/open-folder: ${res.status}`);
  }
}

/**
 * Cancels an in-flight run — either by `runId` (from `runBlockStream`'s own
 * `"started"` event, the connection that actually started it) or by
 * address (`{nodeId, block}`, for a tab that only ever knew about it via
 * `/api/run/subscribe`/`/api/run/tty/attach` and so never had a `runId` to
 * begin with — see `crates/server/src/lib.rs`'s own `KillRequest`). A 404
 * (already finished, or an unknown id/address) is treated the same as
 * success: either way, there's nothing left to kill.
 */
export async function killRun(target: string | { nodeId: string; block: string }): Promise<void> {
  const body = typeof target === "string" ? { runId: target } : { nodeId: target.nodeId, block: target.block };
  const res = await fetch("/api/kill", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok && res.status !== 404) {
    throw new Error(`POST /api/kill: ${res.status}`);
  }
}

/**
 * Every `service` block this server process has ever spawned and still
 * knows about — what a freshly-loaded/refreshed tab polls to repopulate
 * the service panel and every node's own badge from. **Experimental**, see
 * SPEC.md's "Service blocks (experimental)".
 */
export async function fetchServices(): Promise<ServiceStatusDto[]> {
  const res = await fetch("/api/services");
  if (!res.ok) throw new Error(`GET /api/services: ${res.status}`);
  return res.json();
}

/** One entry of `fetchActiveRuns`'s own result — see that function's doc
 * comment. */
export interface ActiveRunDto {
  nodeId: string;
  block: string;
  kind: "plain" | "tty";
  status: "running" | "exited" | "killed";
  exitCode?: number;
  uptimeMs: number;
}

/**
 * Every plain-block run and `tty` session this server process currently
 * knows about (running, or the most recent one for that address), whether
 * or not any tab is currently watching it — what a freshly-loaded/
 * reloaded tab reconciles its own live state against on mount (see
 * `App.tsx`'s reconciliation effect) and what `TtySessionsPanel` lists to
 * reattach to. The `kind === "plain"` equivalent of `fetchServices` — see
 * `crates/server/src/lib.rs`'s own `get_active_runs`.
 */
export async function fetchActiveRuns(): Promise<ActiveRunDto[]> {
  const res = await fetch("/api/runs");
  if (!res.ok) throw new Error(`GET /api/runs: ${res.status}`);
  return res.json();
}

export async function fetchServiceLog(
  nodeId: string,
  block: string,
): Promise<{ stream: "stdout" | "stderr"; text: string }[]> {
  const params = new URLSearchParams({ nodeId, block });
  const res = await fetch(`/api/services/log?${params}`);
  if (!res.ok) throw new Error(`GET /api/services/log: ${res.status}`);
  return res.json();
}

/** Kills the service's whole process group and releases its lock file —
 * the registry entry itself stays (now `"stopped"`), not removed. */
export async function stopService(nodeId: string, block: string): Promise<void> {
  const res = await fetch("/api/services/stop", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ nodeId, block }),
  });
  if (!res.ok) throw new Error(`POST /api/services/stop: ${res.status}`);
}

/** Stops the running instance and spawns a fresh one with the exact
 * parameters it was last started with — "local only", never touches
 * anything that depends on this service. */
export async function restartService(nodeId: string, block: string): Promise<{ pid: number }> {
  const res = await fetch("/api/services/restart", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ nodeId, block }),
  });
  if (!res.ok) throw new Error(`POST /api/services/restart: ${res.status}`);
  return res.json();
}

/** The confirm side of a `"lock-conflict"` event: kills whatever
 * process the lock file currently names as owner, releases the lock, and
 * starts the service fresh. `path`/`block`/`vars` mirror `runBlockStream`'s
 * own arguments for the same block. */
export async function forceStartService(
  path: string[],
  block: string,
  vars?: Record<string, string>,
): Promise<{ pid: number }> {
  const res = await fetch("/api/services/force-start", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ path, block, vars: vars ?? {} }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `POST /api/services/force-start: ${res.status}`);
  }
  return res.json();
}

/**
 * Forgets every block's session-freshness record on the server — the next
 * "⛓ run chain" re-runs every pulled-in dependency for real instead of
 * skipping whichever ones still look unchanged since their last run this
 * session (see `RunEvent`'s `"step-skipped"` variant). Purely in-memory on
 * the server side, so this never touches the canvas file itself or any
 * persisted `<!-- meshfox:output ... -->` cache — those are unaffected.
 */
export async function resetSession(): Promise<void> {
  const res = await fetch("/api/session/reset", { method: "POST" });
  if (!res.ok) {
    throw new Error(`POST /api/session/reset: ${res.status}`);
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
 * Opens a long-lived WebSocket to `/api/watch` for as long as this tab stays
 * open, transparently reconnecting (see `WATCH_RECONNECT_DELAYS_MS`)
 * whenever the connection drops. The server counts each open connection as
 * one open tab and, once every one of them has stayed gone past its own
 * grace period, exits on its own (see README's roadmap) — so this is meant
 * to be called once, for the lifetime of the page, not per-request.
 *
 * Each event the server sends carries a `seq` (see
 * `crates/server/src/canvas_events.rs`) — this function tracks the highest
 * one it's seen and passes it back as `?since=` on every reconnect, so a
 * dropped connection can tell "nothing happened while I was gone" from
 * "something did, but I can't see what any more" (the server's own
 * backlog buffer only holds so much) instead of just resuming blind and
 * possibly missing the one change that happened during the gap. Either the
 * initial `"connected"` message reporting `resync: true`, or a mid-stream
 * `"resync"` message (the *live* broadcast falling behind, a different gap
 * than the backlog one), triggers `onChanged()` itself — a full refetch is
 * always a safe way to resolve "can't tell what changed."
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
 * socket can observe the connection die before its own `pagehide` handler
 * (below) has had a chance to mark it as leaving. Retrying instead of
 * reacting immediately gives both cases a chance to resolve themselves: the
 * hibernate case by the retry simply succeeding once the socket is usable
 * again, the refresh case because the retry is scheduled with `setTimeout`
 * on a page that's already being torn down by the navigation, so it never
 * actually fires. Returns a function that stops watching (closes the
 * underlying socket and any pending retry) without itself triggering
 * `onDisconnected`.
 *
 * A reload (or any other navigation away from this tab) also closes the
 * connection out from under the socket, from the browser's side, not this
 * function's own call to `.close()` — indistinguishable, by a close event
 * alone, from the server itself actually having died. `pagehide` fires
 * first in the common case (reload, back/forward, closing the tab), so it's
 * used here to tell "this tab is leaving" apart from "the server is gone":
 * without it, a plain reload would misread its own connection drop as the
 * server having stopped and (see App.tsx's `serverGone`) try to close the
 * very tab that's mid-reload instead of letting it finish. The reconnect
 * retries above are the backstop for when `pagehide` loses that race.
 */
/** `"node-upserted"`/`"node-removed"`/`"nodes-reordered"` — the precise,
 * per-operation counterpart to `"changed"` (see `crates/server/src/lib.rs`'s
 * own `ServerEvent` doc comment): a client that applies these in place
 * never needs the blanket `onChanged` reload for the mutation they
 * describe. `node` is already the exact same shape a `GET /api/canvas`
 * response's own `nodes` array entries are (same server-side type),
 * fully annotated (effective color, constraint status) — App.tsx's own
 * `onNodeOp` handler patches `canvas.nodes` with it directly, no
 * reshaping needed. */
export type NodeOpEvent =
  | { type: "node-upserted"; node: CanvasNode }
  | { type: "node-removed"; nodeId: string; keepChildren: boolean }
  | { type: "nodes-reordered"; parentId: string; childIds: string[] };

export function watchChanges(
  onChanged: () => void,
  onDisconnected: () => void,
  /** A `"run-started"` event — a plain block just started running in this
   * server process, whether or not *this* tab was the one that triggered
   * it (see `crates/server/src/lib.rs`'s `ServerEvent::RunStarted`). In
   * practice, today, only ever fired for an `autorun` block's own
   * server-triggered run (`submit_form`'s own trigger) — the tab that
   * actually clicked Send already learns the same addresses directly from
   * `submitForm`'s response (see `App.tsx`'s `handleSubmitForm`) and
   * doesn't need this; it's for every *other* open tab on the same
   * document. Optional — a caller that doesn't care about autorun-
   * triggered runs elsewhere just omits it. */
  onRunStarted?: (nodeId: string, block: string) => void,
  /** See `NodeOpEvent`'s own doc comment. Optional — a caller that skips
   * this just falls back to `onChanged`'s full reload for these too (see
   * below), same graceful degradation the server's own `ServerEvent` doc
   * comment describes for a client that doesn't know these event types
   * at all yet. */
  onNodeOp?: (op: NodeOpEvent) => void,
): () => void {
  let leaving = false;
  let stopped = false;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let socket: WebSocket | undefined;
  // The highest `seq` seen so far, across reconnects within this page's
  // lifetime — `undefined` until the first event ever arrives, which
  // naturally asks for no backlog at all on the very first connect.
  let lastSeq: number | undefined;
  const markLeaving = () => {
    leaving = true;
  };
  window.addEventListener("pagehide", markLeaving);

  const socketUrl = () => {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    const since = lastSeq !== undefined ? `?since=${lastSeq}` : "";
    return `${proto}//${window.location.host}/api/watch${since}`;
  };

  const connectOnce = (onEstablished: () => void): Promise<void> =>
    new Promise((_resolve, reject) => {
      const ws = new WebSocket(socketUrl());
      socket = ws;
      ws.onopen = () => onEstablished();
      ws.onmessage = (ev) => {
        const event = JSON.parse(ev.data as string) as {
          type: string;
          seq?: number;
          nodeId?: string;
          block?: string;
          resync?: boolean;
          node?: CanvasNode;
          keepChildren?: boolean;
          parentId?: string;
          childIds?: string[];
        };
        if (event.seq !== undefined) lastSeq = event.seq;
        if (event.type === "changed" || (event.type === "connected" && event.resync) || event.type === "resync") {
          onChanged();
        } else if (event.type === "run-started" && event.nodeId !== undefined && event.block !== undefined) {
          onRunStarted?.(event.nodeId, event.block);
        } else if (event.type === "node-upserted" && event.node !== undefined) {
          if (onNodeOp) onNodeOp({ type: "node-upserted", node: event.node });
          else onChanged();
        } else if (
          event.type === "node-removed" &&
          event.nodeId !== undefined &&
          event.keepChildren !== undefined
        ) {
          if (onNodeOp) onNodeOp({ type: "node-removed", nodeId: event.nodeId, keepChildren: event.keepChildren });
          else onChanged();
        } else if (
          event.type === "nodes-reordered" &&
          event.parentId !== undefined &&
          event.childIds !== undefined
        ) {
          if (onNodeOp) onNodeOp({ type: "nodes-reordered", parentId: event.parentId, childIds: event.childIds });
          else onChanged();
        }
      };
      // The stream ending is itself a drop (the server never sends a
      // deliberate "goodbye" event before closing) — reject exactly like a
      // connection error would, so the retry logic below handles both the
      // same way. `onerror` always precedes `onclose` for a WebSocket, so
      // driving the reject from `onclose` alone (not double-rejecting) is
      // enough.
      // Fires for a real drop *and* for this function's own returned
      // `stop()` closing the socket deliberately — `run`'s own `stopped`
      // check (below) is what tells those two apart, not anything here.
      ws.onclose = () => {
        if (socket === ws) socket = undefined;
        reject(new Error("WS /api/watch: closed"));
      };
    });

  const run = (attempt: number) => {
    let established = false;
    connectOnce(() => {
      established = true;
    }).catch(() => {
      if (leaving || stopped) return;
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
    socket?.close();
  };
}
