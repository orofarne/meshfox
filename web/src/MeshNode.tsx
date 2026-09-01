import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Handle, NodeResizer, NodeToolbar, Position, useReactFlow, type NodeProps } from "@xyflow/react";
import ReactMarkdown, { defaultUrlTransform, type Components, type UrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkImageAttrs from "./remarkImageAttrs";
import remarkSubSup from "./remarkSubSup";
import remarkGfmAlerts from "./remarkGfmAlerts";
import { highlightToHtml } from "./shiki";
import { BUTTON_LANG, defaultBlock, parseBody, type BodySegment, type CodeSegment, type ConstraintSegment } from "./fence";
import { parseBlockRef, blockDomId } from "./deps";
import { AnsiText } from "./AnsiText";
import { NodeTextEditor } from "./NodeTextEditor";
import { fetchNodeFileContent, fetchLinkPreview, type LinkPreview } from "./api";
import type { ConstraintStatusDto, NodeType } from "./types";

/**
 * A relative `![](...)`/link target inside a node's rendered body normally
 * resolves against the canvas file's own directory (see the server's
 * `serve_canvas_relative_file`) — wrong once this node's body was spliced
 * in from an `include` target that lives in a different directory (see
 * `MeshNodeData.assetBase`, set from `crates/core/src/canvas.rs`'s
 * `Node.asset_base`). Rewrites such a target to `/api/include-asset`,
 * which resolves it against `assetBase` instead (still guarded server-side
 * against arbitrary directories — see that handler's own doc comment).
 * Left untouched: absolute URLs (`scheme:`), root-relative paths (`/...`),
 * bare anchors (`#...`), and anything when `assetBase` is unset (every
 * node that wasn't spliced in from an include, same as before).
 */
function resolveAssetHref(href: string | undefined, assetBase: string | undefined): string | undefined {
  if (!href || !assetBase) return href;
  if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("/") || href.startsWith("#")) return href;
  return `/api/include-asset?dir=${encodeURIComponent(assetBase)}&file=${encodeURIComponent(href)}`;
}

/**
 * `react-markdown`'s own `defaultUrlTransform` strips any URL scheme
 * outside `http(s)`/`irc(s)`/`mailto`/`xmpp` down to `""` — a sensible
 * default (blocks a `javascript:`/`data:text/html` link), but it also
 * silently blanks a `data:image/...;base64,...` `<img src>` (TODO.canvas.md:
 * "Base64 image"), which is exactly the wire format `Node.effectiveColor`'s
 * sibling scheme uses on purpose. Scoped to `key === "src"` — react-markdown
 * calls this per-attribute with the *property* name, not the tag, but `src`
 * is only ever an image's own source here (never a link's `href`, which
 * stays under the strict default) — so a pasted image renders, without
 * loosening what a plain link is allowed to point at.
 */
const allowDataImageUrls: UrlTransform = (url, key, node) => {
  if (key === "src" && url.startsWith("data:image/")) return url;
  return defaultUrlTransform(url);
};

/**
 * A fenced code block embedded in plain prose (not a runnable/constraint
 * fence — those are split out of the "markdown" segment entirely by
 * `fence.ts::parseBody` and highlighted separately, see `RunnableCodeBlock`/
 * `ConstraintFenceBlock` below) — react-markdown's own default rendering
 * for these is a bare, unhighlighted `<pre><code>`, the exact bug
 * TODO.canvas.md's "Подсветка синтаксиса в блоках кода в webui не
 * работает" was about, just for the one case that fix never actually
 * covered (a plain example snippet in the middle of a node's write-up,
 * with no `name=`/`cache=`/`constraint` marking it a *runnable* fence).
 * Routes it through the same `HighlightedCode`/Shiki path as every other
 * read-only code block instead. `className` is react-markdown's own
 * `language-xxx` (from the fence's info string) on a block-level `code`;
 * absent entirely for inline code (`` `foo` ``), which is left as the
 * plain default — nothing to highlight, and it was never part of this bug.
 */
function MarkdownCodeBlock({ className, children }: { className?: string; children?: React.ReactNode }) {
  const lang = /language-(\S+)/.exec(className ?? "")?.[1];
  if (!lang) return <code className={className}>{children}</code>;
  return <HighlightedCode code={String(children).replace(/\n$/, "")} lang={lang} />;
}

/**
 * Builds this file's `<ReactMarkdown>` `components` — same shape
 * everywhere (a link in a node's rendered body opens in a new tab rather
 * than navigating the canvas itself away from the app; `rel=` is the
 * standard `target="_blank"` companion, since the opened page otherwise
 * gets a live `window.opener` back to this one), just with `img`/`a`
 * targets resolved against `assetBase` first (see `resolveAssetHref`).
 * `node` is react-markdown's own mdast node for the element, not a real
 * DOM attribute — destructured out so it never reaches the native tag.
 *
 * `pre` is a plain passthrough: in CommonMark/GFM output `<pre>` only ever
 * wraps a fenced `code` block, and `MarkdownCodeBlock`/`HighlightedCode`
 * already supplies its own wrapper — keeping the default `<pre>` around it
 * too would double up (and, for the plain-`<pre><code>` loading-state
 * fallback, nest one `<pre>` inside another).
 */
function makeMarkdownComponents(assetBase: string | undefined): Components {
  return {
    a: ({ node: _node, href, ...props }) => (
      <a {...props} href={resolveAssetHref(href, assetBase)} target="_blank" rel="noopener noreferrer" />
    ),
    img: ({ node: _node, src, ...props }) => <img {...props} src={resolveAssetHref(src, assetBase)} />,
    pre: ({ children }) => <>{children}</>,
    code: ({ node: _node, className, children }) => (
      <MarkdownCodeBlock className={className}>{children}</MarkdownCodeBlock>
    ),
  };
}

/** `assetBase` is never set for the draft text `NodeBodyPreview` renders
 * (an unsaved edit, not a resolved include), so it always resolves the
 * same as before includes existed. */
const markdownComponents = makeMarkdownComponents(undefined);

/**
 * Live state of one block's most recent run, from `App.tsx`'s handling of
 * `runBlockStream`'s events (see `./api.ts`'s `RunEvent`) — replaces both
 * the old `running` flag and `transientOutputs`, since a streaming run has
 * more states than "not running" / "done" (queued behind an earlier chain
 * step, actively streaming output, killed mid-flight).
 */
export interface LiveBlockState {
  /** `"queued"` is a client-side preview (see `deps.ts`'s `resolveChain`)
   * shown the instant a run is requested, before the server has actually
   * confirmed this block is even part of the chain — reconciled against
   * real `step-start`/`step-end` events once those arrive. */
  /** `"skipped"` — a `"step-skipped"` event: this dependency already ran
   * successfully earlier in the session and hasn't changed since, so the
   * server didn't actually re-run it (see SPEC.md's "Runnable code
   * fences"). Never the status of the block actually requested, only a
   * pulled-in dependency.
   *
   * `"blocked"` — never actually run at all: still `"queued"` when the rest
   * of its chain's run ended (a `step-end` with a non-zero exit, a `killed`,
   * or a network failure — see App.tsx's `executeRun`/`blockStuckQueued`),
   * so the server never got around to a `step-start`/`step-skipped` for it.
   * Purely a client-side inference — the server itself has no such status,
   * it just stops sending events for a chain it gave up on partway
   * through. */
  status: "queued" | "running" | "done" | "killed" | "skipped" | "blocked";
  /** Set (only for the block whose button was actually clicked) when that
   * click was "⛓ run chain" rather than plain "run" — lets the two buttons'
   * labels change independently: whichever one was clicked shows
   * "running…"/"queued…", while the other stays disabled but keeps its
   * static label instead of also flipping (see App.tsx's `handleRun`). */
  viaChain?: boolean;
  exitCode?: number;
  /** Accumulated output so far — grows line by line while `status` is
   * `"running"`. Not persisted anywhere; a page reload loses it (same as
   * the old `transientOutputs` did), same rationale: it was never written
   * to the file unless `cache` + Edit mode, in which case the eventual
   * canvas reload picks up the real cached copy instead. */
  text: string;
  /** Just this run's stdout lines so far, kept separately from `text`
   * (merged stdout+stderr) as each `"output"` event's own `stream` tag
   * says which one it was — mirrors `core::output::ExecOutput::stdout`/
   * `.stderr`, needed for the same reason: `output="markdown"` mode
   * (`RunOutput`'s `MarkdownOutput`) renders stdout as Markdown and stderr
   * as its own plain-text block, and merged `text` alone can't be split
   * back apart once interleaved. `undefined` until the first `"output"`
   * event of a run arrives (cleared to `undefined`, not `""`, at
   * `"step-start"` — see App.tsx — so a block with no stdout at all this
   * run reads as "nothing yet", not "empty markdown"). */
  stdoutText?: string;
  /** Just this run's stderr lines so far — see `stdoutText` above. */
  stderrText?: string;
  /** Set once `status` becomes `"running"` — what `onKill` needs to
   * identify *this* run to the server. Cleared once the block is no
   * longer the actively-streaming step. */
  runId?: string;
  /** `Date.now()` when this step's own `"step-start"` event arrived —
   * client-side wall-clock, used only to tick a live elapsed-time counter
   * while `status === "running"` (see `LiveElapsed`). Not meant to be
   * precise to the millisecond (network latency between the server
   * actually starting the process and this event arriving isn't
   * accounted for) — the authoritative figure is `durationMs`, set once
   * `"step-end"` actually reports it. */
  startedAt?: number;
  /** The server's own authoritative wall-clock duration for this step's
   * process, in milliseconds — from `"step-end"`'s `durationMs` (mirrors
   * `core::output::ExecOutput::duration_ms`, the same figure a `cache`d
   * block's persisted output header carries after a reload). Set once
   * `status` becomes `"done"`/`"killed"`. */
  durationMs?: number;
}

export interface MeshNodeData {
  /** The node's own stable id (`meshfox:node id="..."`) — the same handle
   * `meshfox:edge from=`/`deps=`/the CLI's node-id path address it by,
   * otherwise invisible in the rendered canvas. */
  id: string;
  title: string;
  level: number;
  nodeType: NodeType;
  /** True when position/size came from the web client's own auto-layout
   * (see `./autolayout.ts`) rather than the file — not authored, nothing
   * to feel bad about dragging away. */
  suggested: boolean;
  /** A depth-≥2 auto-placed node's content-driven height is capped here
   * (see `autolayout.ts`) — applied directly to this component's own root
   * element (not the surrounding React Flow node wrapper `App.tsx` sizes):
   * a `max-height` on the wrapper doesn't establish a *definite* height
   * for this element's own `height: 100%` to resolve against (confirmed
   * directly — Chromium treats a `max-height`-clamped auto height as still
   * indefinite for descendants' percentage-height purposes), which
   * silently defeated `.mesh-node-body`'s `overflow: auto` entirely:
   * content just kept growing instead of scrolling. Applying the cap here
   * instead means this element establishes its own definite (clamped)
   * height directly, which its `flex: 1; min-height: 0` body can actually
   * shrink against. Undefined for anything that isn't a capped auto node
   * (root/depth-1, or any node with a real, authored height). */
  maxHeight?: number;
  /** Read-only by default — dragging, resizing, and saving layout are
   * gated on this until the user clicks "Edit". Running blocks is always
   * allowed; this only controls whether a run persists its output. */
  editMode: boolean;
  /** Live/just-finished run state for blocks in *this* node, keyed by
   * block name — only present for blocks touched by an in-flight or
   * not-yet-reloaded-away run; absent means "show the cached `seg.output`
   * from the file, if any". */
  liveBlocks: Record<string, LiveBlockState>;
  /** This node itself is folded — for a node with real body content, that
   * means shrinking to a compact title-only row (no body, no run buttons);
   * for an already-title-only node (empty body — see `isTitleOnly` at its
   * one render site below), its own row never changes either way, since
   * there was nothing in it to hide to begin with. Either way, every
   * descendant (if it has any) is hidden entirely too (see App.tsx's
   * `foldedNodeIds`/`visibleNodeIds`) — which is the *only* effect folding
   * a title-only node has, and exactly why `hasChildren` below gates
   * whether it even gets a toggle: with no children, folding one would do
   * nothing at all. A session preference (this component's own click just
   * flips `foldedNodeIds`, persisted to localStorage, never written to the
   * file) — but the *default* it starts from on a canvas's first-ever open
   * is the document's own, via a node's `fold=` attribute or the whole
   * document's `unfold` option (see `App.tsx`'s `resolveDefaultFold`,
   * SPEC.md's "Options" section). */
  folded: boolean;
  /** Whether this node has any structural children at all — irrelevant to
   * whether a node with real body content gets a fold toggle (it always
   * does, see `folded` above), but the one thing that decides it for an
   * already-title-only node: folding one hides only its subtree (its own
   * row is identical either way), so with no children there's nothing a
   * toggle could do, and the title-only render branch below hides it in
   * that case (mirrors the TUI's own `has_children`-gated marker, just
   * scoped to this one node shape rather than every node). */
  hasChildren: boolean;
  onToggleFold: () => void;
  /** True for the one node keyboard nav (j/k/h/l/Enter, see App.tsx)
   * currently has focus on — distinct from React Flow's own mouse-driven
   * `selected` (NodeResizer visibility, multi-select), so a keypress never
   * silently changes what's selected for resize/multi-op purposes.
   * Undefined/false for every other node. */
  focused?: boolean;
  /** App.tsx's search bar's current query, trimmed — only set while the
   * bar is open and non-empty. Drives this node's own substring
   * highlighting (see `useSearchHighlight`); this component doesn't care
   * whether *this* node is actually among the matches, it just highlights
   * whatever occurrences of the string happen to be in its own rendered
   * text. Empty/undefined the rest of the time, which clears any
   * highlight this node was carrying. */
  searchQuery?: string;
  /** Which occurrence of `searchQuery`, 0-based, *within this node's own*
   * title+text (title occurrences first — App.tsx's `searchMatches` counts
   * them in that same order), is the one the search bar is currently
   * stopped on. Only meaningful — and only ever set — for the one node
   * that's also `focused`: it's what `useSearchHighlight` scrolls/pans to,
   * rather than always the first occurrence it finds, since a query that
   * appears more than once in one node is that many separate stops, not
   * one (see App.tsx's own `searchMatches` doc comment). `undefined`
   * outside active search navigation. */
  searchCurrentOccurrence?: number;
  target?: string;
  /** Results of every embedded ` ```starlark constraint ` fence in this
   * node's own body, in document order (see `ConstraintStatusDto`) — matched
   * to its rendered `ConstraintSegment` purely by position, both being built
   * from the same document order. Absent or empty means this node has no
   * constraint fences (nothing to roll up into the title-bar badge below),
   * not "0 constraints, 0 passing". */
  constraintResults?: ConstraintStatusDto[];
  /** file-node display mode — `"code"` shows a read-only, syntax-highlighted
   * preview of the target file's own content instead of a plain link. */
  display?: "link" | "code";
  /** file-node syntax-highlighting language hint for `display: "code"`;
   * absent means auto-detect from the target's file extension. */
  lang?: string;
  /** file-node only: an executable (e.g. "python") to run against
   * `target` — present makes the node runnable via the title bar's "▷ run"
   * button (see `App.tsx`'s `executeFileRun`). */
  interpreter?: string;
  /** link-node only: shows an OpenGraph social preview card (title/
   * description/image) below the link — see `LinkPreviewCard`. */
  preview?: boolean;
  text: string;
  /** Absolute directory a relative `img`/link target in `text` should
   * resolve against, when it's not the canvas file's own directory — set
   * when this node's body was spliced in from an `include` target that
   * lives elsewhere on disk (see `resolveAssetHref`, and
   * `crates/core/src/canvas.rs`'s `Node.asset_base`). `undefined` for
   * every node that wasn't, which resolves exactly as before includes
   * carried this field. */
  assetBase?: string;
  /** `true` when `text` is actually a plain-Markdown `include` target's
   * transcluded content, not this node's own real text — there's no
   * well-defined way to write a per-node body edit here back to the real
   * target file, so the title bar's pencil button opens Source mode
   * (already scoped to this node's own id, which doubles as the
   * include's `nodeId`) instead of the normal inline editor. See
   * `crates/core/src/canvas.rs`'s `Node.plain_markdown_include`. */
  plainMarkdownInclude?: boolean;
  /** JSON Canvas color — either a hex string or a preset `"1"`-`"6"` (see
   * `resolveNodeColor`) — `undefined`/empty means no color was set. */
  color?: string;
  /** `color`, or, when unset, a fallback derived from this node's tags
   * against the document's `meshfox:tag-color` defaults — see
   * `CanvasNode.effectiveColor`. Rendering (the title-bar accent below)
   * uses this, not `color` — `color` alone stays the raw value NodeSettings
   * edits. */
  effectiveColor?: string;
  /** Free-form labels, shown as small chips under the title — purely
   * descriptive, no structural meaning. */
  tags?: string[];
  /** `withDeps` picks which button was clicked: the plain "run" (false,
   * skips `deps=`) or "⛓ run chain" (true, runs the chain first). */
  onRun: (blockName: string, withDeps: boolean) => void;
  onKill: (blockName: string) => void;
  /** `tty` blocks only — opens a `TtyPanel` (a real interactive terminal)
   * instead of streaming captured output the way `onRun` does. Same
   * `withDeps` meaning: plain "run" (false) vs "⛓ run chain" (true).
   * `autoclose` mirrors the block's own `CodeSegment.autoclose` — whether
   * `TtyPanel` should close itself the instant the process exits, rather
   * than the default of staying open until closed by hand. */
  onRunTty: (blockName: string, withDeps: boolean, autoclose: boolean) => void;
  /** A constraint fence's Starlark isn't a `run`nable fence (only bash/sh
   * are), so each one gets this instead of a Run button: re-fetches the
   * whole canvas, which re-evaluates every constraint server-side (see
   * App.tsx's `load`) and refreshes this node's badge/messages. */
  onRecheckConstraint: () => void;
  /** Creates a new child node under this one — the "+" button (inline in
   * the title bar for a group, floating at the right edge otherwise). */
  onAddChild: () => void;
  /** Moves this node's own heading immediately before/after its previous/
   * next sibling (`mdcanvas::move_sibling`, via `App.tsx`'s
   * `handleMoveSibling`) — an auto-placed node's only lever for changing
   * its own order among siblings, since it has no `x`/`y` to drag; a
   * positioned one gets a new order automatically (`reorder_by_position`)
   * just by being dragged, so its own ↑/↓ buttons stay hidden instead.
   * `undefined` when there's no such sibling to move past (already
   * first/last among its parent's children, or this node isn't
   * auto-placed at all) — hides the corresponding button entirely rather
   * than showing it disabled. */
  onMoveUp?: () => void;
  onMoveDown?: () => void;
  /** Drops this node's own authored `x`/`y`/`w`/`h`, reverting it to
   * auto-placement (`mdcanvas::set_node_meta`, via `App.tsx`'s
   * `handleClearNodeLayout`) — the per-node counterpart to the toolbar's
   * whole-document "Auto-layout" button. `undefined` for an already
   * auto-placed (`suggested`) node — hides the button entirely, since
   * there's nothing authored there to clear. */
  onClearLayout?: () => void;
  /** Opens this node's settings modal (title/type/color/target/edges). */
  onOpenSettings: () => void;
  /** file-node only — opens `target` in the OS's default application for
   * it (the title bar's "↗ open" button). */
  onOpenFile: () => void;
  /** Opens this node's body in a floating window (`NodeExpandPanel`) — same
   * live content (run/kill buttons, streaming output) as the inline box,
   * just bigger and not at the mercy of the canvas's current pan/zoom.
   * Available read-only, unlike the edit-mode-only actions above. */
  onExpand: () => void;
  /** Persists a full replacement of this node's raw Markdown body — the
   * inline text editor's auto-save. */
  onSaveText: (text: string) => void;
  /** True for exactly one render, right after "add child" + NodeSettings'
   * "ok" on a text-type node (see App.tsx's `autoOpenTextEditorNodeId`) —
   * opens this node's inline body editor immediately instead of making the
   * user click the ✏ button themselves, since typing the body is almost
   * always the very next thing after naming a freshly-created node.
   * `undefined`/`false` the rest of the time. Paired with
   * `onAutoOpenTextEditorConsumed`, which this component calls the moment
   * it's acted on `true` — App.tsx clears its own tracking state in
   * response, so this never re-fires for the same creation. */
  autoOpenTextEditor?: boolean;
  onAutoOpenTextEditorConsumed?: () => void;
  /** `plainMarkdownInclude` nodes only — opens Source mode scoped to this
   * node's own id (the include's `nodeId`) instead of the inline text
   * editor, since that's the only place this content can actually be
   * edited (see `plainMarkdownInclude`'s own doc comment). */
  onOpenSourceMode: (includeNodeId: string) => void;
  /** `false` for the root — it can't be deleted (there'd be nothing left
   * to re-root the document at), so the title bar's trash button is
   * hidden entirely rather than shown disabled. */
  canDelete: boolean;
  /** Opens the delete-confirm dialog for this node — handled up in `App`
   * (like `onOpenSettings`), not locally: a `MeshNode` lives inside a
   * `transform`-positioned React Flow node wrapper, which becomes a CSS
   * containing block for any `position: fixed` descendant, so a modal
   * rendered here couldn't actually paint above sibling nodes. */
  onRequestDelete: () => void;
  [key: string]: unknown;
}

/** `file`/`link` nodes' title-bar type marker. Hand-drawn inline SVG rather
 * than the 📎/🔗 emoji this used to be: those are color-emoji-presentation
 * codepoints with no monochrome fallback in most font stacks, so unlike
 * this app's other symbol-character icons they can't be steered back to a
 * crisp vector glyph via `font-variant-emoji: text` (see index.css's `body`
 * rule) — a color-emoji glyph is a bitmap, and React Flow's zoom (a CSS
 * `transform: scale()`, not a font-size change) scales that bitmap into
 * pixelated noise at a low enough zoom. An SVG sized in `em` and colored via
 * `currentColor` scales cleanly at any zoom, same as the surrounding text. */
/** The ▸/▾ fold marker — mirrors the TUI's own `▾`/`▸` row marker (see
 * README's "Terminal viewer" section) as closely as a spatial canvas
 * allows: folded shrinks this node to a title-only row and, if it has any,
 * hides its whole subtree too (see App.tsx's `foldedNodeIds`). Rendered on
 * every node with real body content — a leaf still gets one, same as any
 * node with children, since its own body can be just as unwieldy as a
 * whole subtree — but an already-title-only node (empty-bodied `text`
 * node — see `isTitleOnly` at its normal render site below) only gets one
 * when it has children: its own row is this exact compact single-line
 * layout whether folded or not (nothing there to hide), so folding one
 * with no children would be a total no-op with a button to show for it —
 * see that render site's own gate on `MeshNodeData.hasChildren`. */
// How far the pointer may move between `mousedown` and `mouseup` on a
// node's title text and still count as a click-to-toggle-fold rather than
// the start of a text-selection drag (see `MeshNode`'s `handleTitleClick`).
const TITLE_CLICK_MOVE_THRESHOLD = 5;

function FoldToggle({
  folded,
  onToggle,
  foldedTitle = "Unfold this subtree",
  unfoldedTitle = "Fold this subtree",
}: {
  folded: boolean;
  onToggle: () => void;
  /** Override for a non-node caller (a code block's own collapse toggle —
   * see `ConstraintFenceBlock`/`RunnableCodeBlock`) — same ▸/▾ marker and
   * button styling, just not literally "a subtree" there. */
  foldedTitle?: string;
  unfoldedTitle?: string;
}) {
  return (
    <button
      type="button"
      className="mesh-node-icon-button mesh-node-fold-toggle nodrag"
      onClick={onToggle}
      title={folded ? foldedTitle : unfoldedTitle}
    >
      {folded ? "▸" : "▾"}
    </button>
  );
}

function TypeIcon({ type }: { type: NodeType }) {
  if (type === "file") {
    return (
      <svg className="mesh-node-type-icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M15 5.5v9a3.5 3.5 0 1 1-7 0V7a2 2 0 1 1 4 0v7.5" />
      </svg>
    );
  }
  if (type === "link") {
    return (
      <svg className="mesh-node-type-icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M9 15l6-6" />
        <path d="M10.5 6.5l1-1a3.5 3.5 0 0 1 5 5l-1 1" />
        <path d="M13.5 17.5l-1 1a3.5 3.5 0 0 1-5-5l1-1" />
      </svg>
    );
  }
  return null;
}

/** Rolls up every embedded constraint fence's result in a node into one
 * pass/fail summary for the title-bar pill — `undefined` when the node has
 * no constraint fences at all (nothing to show), `ok: true` only when every
 * one of them passed. Each failing fence's messages are prefixed with its
 * own `label` in the rolled-up tooltip, since a node's badge can now be
 * covering more than one check. */
function aggregateConstraintStatus(results: ConstraintStatusDto[] | undefined): ConstraintStatusDto | undefined {
  if (!results || results.length === 0) return undefined;
  const failing = results.filter((r) => !r.ok);
  return {
    label: "",
    ok: failing.length === 0,
    messages: failing.flatMap((r) => r.messages.map((m) => `${r.label}: ${m}`)),
  };
}

/** Small aggregate pass/fail pill for a node's title bar, covering every
 * embedded constraint fence in its body — green check when all of them
 * raised no violations, red cross (with every failing one's messages as a
 * tooltip) otherwise. Renders nothing for a node with no constraint fences
 * (see `aggregateConstraintStatus`). */
function ConstraintBadge({ status }: { status: ConstraintStatusDto | undefined }) {
  if (!status) return null;
  return (
    <span
      className={
        status.ok
          ? "mesh-node-constraint-badge mesh-node-constraint-badge-ok"
          : "mesh-node-constraint-badge mesh-node-constraint-badge-fail"
      }
      title={status.ok ? "Constraints pass" : status.messages.join("\n")}
    >
      {status.ok ? "✓" : "✗"}
    </span>
  );
}

/** Shown in a node's title bar whenever any of its own blocks (fenced code
 * blocks by name, or a runnable `file` node's own run, keyed under its own
 * id — see `MeshNodeData.liveBlocks`'s doc comment) has `status ===
 * "running"`, so a busy node stays visible as busy even folded or scrolled
 * out of view of its actual output. Not shown for `"queued"` — that's
 * "about to run", not "running", and each runnable block's own button
 * already surfaces that via its "queued…" label. */
function RunningSpinner() {
  return <span className="mesh-node-running-spinner" title="A block in this node is running" />;
}

/** Shown in a node's title bar whenever any of its own blocks' most recent
 * run (see `RunningSpinner`'s doc comment for the same `liveBlocks` lookup)
 * ended badly — killed, or `"done"` with a non-zero exit code — so a failure
 * stays visible even for a folded or scrolled-out-of-view node, the same way
 * `RunningSpinner` keeps a busy one visible. Cleared the same way the live
 * state itself is: a fresh run of that block, or the next canvas reload. */
function FailedBadge() {
  return <span className="mesh-node-failed-badge" title="A block in this node failed" />;
}

/** A pixel of slack for the "already at the scroll boundary" checks below —
 * `scrollTop`/`scrollLeft` can be fractional (subpixel layout, non-integer
 * zoom) while `scrollHeight`/`clientHeight` round differently, so a strict
 * `<`/`>` comparison right at the boundary can flip-flop by less than a
 * pixel and never quite agree that the end has been reached. */
const SCROLL_EPSILON = 1;

/** Whether `el` has somewhere to actually move along *the* axis this wheel
 * event is asking for — both that it overflows there and that its current
 * scroll position isn't already at the end in that direction. Only one
 * axis is ever considered: whichever of `deltaX`/`deltaY` is larger in
 * magnitude, the same "dominant axis" a real scroll gesture has. A real
 * mouse or trackpad's "vertical" scroll is rarely a mathematically pure
 * `deltaX: 0` — a couple of stray horizontal units riding along with an
 * otherwise-vertical gesture (see the Playwright test using
 * `wheel(2, 40)`) is normal, physical noise. Checking both axes
 * independently, as an earlier version of this function did, meant that
 * sliver of horizontal noise alone was enough for an element with
 * horizontal room (however much of the *vertical* gesture's real intent
 * got lost) to claim the whole event — silently swallowing the pan the
 * user was actually doing, while creeping the content sideways by a
 * couple of imperceptible pixels per tick. Picking one axis avoids that:
 * a mostly-vertical wheel only ever competes for vertical room, a
 * mostly-horizontal one only for horizontal, regardless of whatever noise
 * rides along on the other. */
function canScrollAlong(el: Element, e: WheelEvent): boolean {
  if (Math.abs(e.deltaY) >= Math.abs(e.deltaX)) {
    if (e.deltaY === 0) return false;
    // `overflowY` must actually be `auto`/`scroll` for `el` to be a real
    // scroll container at all — without this, an *inline* element (e.g.
    // `<code>` inside a `pre>code` fence, `overflow-y: visible` by
    // default) always reports `clientHeight === 0` (inline boxes have no
    // client box) while `scrollHeight` still measures real content
    // extent, so the raw comparison below finds "room to scroll" on
    // literally any inline element with enough text — even though it
    // isn't scrollable at all and the wheel should fall through to its
    // actual scrollable ancestor (`pre` here) or the canvas pan beneath
    // it. Confirmed directly: this is what made a wheel landing on the
    // `<code>` text node inside a horizontally-scrolling fence get
    // silently swallowed instead of panning.
    if (!isScrollableOverflow(getComputedStyle(el).overflowY)) return false;
    return e.deltaY < 0 ? el.scrollTop > SCROLL_EPSILON : el.scrollTop < el.scrollHeight - el.clientHeight - SCROLL_EPSILON;
  }
  if (!isScrollableOverflow(getComputedStyle(el).overflowX)) return false;
  return e.deltaX < 0
    ? el.scrollLeft > SCROLL_EPSILON
    : el.scrollLeft < el.scrollWidth - el.clientWidth - SCROLL_EPSILON;
}

function isScrollableOverflow(overflow: string): boolean {
  return overflow === "auto" || overflow === "scroll";
}

/** A node's body/code-preview area used to unconditionally opt out of
 * React Flow's zoom-on-scroll (the `nowheel` class) so a wheel over long
 * content scrolls it instead of zooming the canvas — but that's a static
 * class name, blind to whether the content actually overflows. A short
 * body has nothing to scroll, so it just as unconditionally ate every
 * wheel event for nothing, making the canvas feel "stuck" under the
 * cursor. This instead only swallows the event (stopping it from
 * bubbling up to React Flow's zoom pane) when there's genuinely something
 * under the cursor to scroll along the direction this specific event
 * actually asks for (see `canScrollAlong` — e.g. a wide code fence,
 * `.mesh-node-body pre { overflow-x: auto }`, scrolls horizontally on its
 * own even when the surrounding body doesn't overflow vertically at all,
 * and a purely vertical wheel over it shouldn't be swallowed just because
 * *some* axis of *something* in the chain overflows), and walking up from
 * the actual event target rather than trusting `currentTarget` alone,
 * since the element that actually has the overflow (that same `pre`) is
 * usually a descendant of the wrapper this handler is attached to, not the
 * wrapper itself. Finds nothing scrollable in that chain → left alone, so
 * the wheel pans the canvas exactly as if the cursor were over blank space.
 *
 * Takes a real `WheelEvent`, not a React `SyntheticEvent` — see
 * `useStopWheelIfScrollable` below for why that distinction matters. */
function stopWheelIfScrollable(e: WheelEvent) {
  let el: Element | null = e.target as Element;
  while (el) {
    if (canScrollAlong(el, e)) {
      e.stopPropagation();
      return;
    }
    if (el === e.currentTarget) return;
    el = el.parentElement;
  }
}

/** Attaches `stopWheelIfScrollable` as a genuine native `wheel` listener on
 * the returned ref's element, instead of via React's `onWheel` prop. React
 * delegates `onWheel` through a single listener on the app's root DOM node —
 * React Flow's own pan/zoom handling (d3-zoom, attached with a real
 * `addEventListener("wheel", …)` directly on `.react-flow__pane`) sits
 * *between* that root and any node's body in the DOM, so during the
 * browser's actual bubble phase the pane's listener always fires first,
 * before the event ever reaches React's delegated dispatch. Calling
 * `e.stopPropagation()` from a React `onWheel` handler was therefore always
 * too late to stop the pane from panning — it stops propagation to
 * ancestors *of the React root*, not to this closer, already-fired native
 * listener. Adding the listener straight to this element instead makes it
 * run — and be able to actually stop the event — before the pane's
 * listener does, the same fix any React app reaches for when a
 * non-React library's own native listener needs to be preempted.
 *
 * A callback ref, not a `useRef` object read inside a `useEffect(..., [])` —
 * the latter only ever runs once, right after this component's *first*
 * commit, so it silently attaches nothing at all for a caller whose
 * wheel-listened element isn't there yet on that first render (e.g.
 * `FileCodePreview` below: its wheelRef'd div only exists once its async
 * file fetch resolves and `state.status` flips from `"loading"` to
 * `"ready"` — at the first-and-only run, `ref.current` was still `null`,
 * and a plain-ref effect never gets a second chance to notice it showed up
 * later). Confirmed directly this was why wheeling over a file node's code
 * preview always fell through to pan the canvas instead of ever scrolling
 * the preview itself, however much it overflowed. A callback ref re-runs
 * on every mount/unmount of the actual underlying DOM node, however many
 * renders apart, which is exactly the "attach once the element genuinely
 * exists" semantics this needs. */
function useStopWheelIfScrollable<T extends HTMLElement>(): (node: T | null) => void {
  return useCallback((node: T | null) => {
    if (!node) return;
    node.addEventListener("wheel", stopWheelIfScrollable, { passive: true });
    return () => node.removeEventListener("wheel", stopWheelIfScrollable);
  }, []);
}

const SEARCH_HIGHLIGHT_NAME = "mesh-search-match";
const SEARCH_CURRENT_HIGHLIGHT_NAME = "mesh-search-match-current";

/** Two `Highlight`s (the CSS Custom Highlight API's own type — a `Set`-like
 * bag of `Range`s that `::highlight(name)` in index.css paints without
 * touching the DOM), each shared by every `MeshNode` instance, registered
 * once at module load. Shared rather than one per node because a
 * `::highlight()` name has to be a literal identifier already written
 * into the stylesheet — there's no way to mint one per node id at runtime
 * and still have anything style it. Each node just owns its own slice of
 * Ranges within these shared bags (added/removed independently in
 * `useSearchHighlight` below), so nothing about sharing them risks one
 * node's matches bleeding into another's.
 *
 * Split in two, not one, so the search bar's *current* occurrence reads
 * as visually distinct from every other match, not just another same-
 * colored highlight indistinguishable from the rest: `searchHighlight`
 * covers every occurrence search matched, `searchCurrentHighlight` covers
 * only the one `useSearchHighlight` is actively centering/panning to (at
 * most one `Range`, ever, across the whole app — see its own doc
 * comment), painted on top via a higher `.priority` (index.css's
 * `::highlight(mesh-search-match-current)` alone would otherwise have to
 * out-specificity the plain one some other, arbitrary way).
 *
 * `null` on a browser without the API at all (e.g. an older Firefox) —
 * every call site below checks this and just no-ops instead, so the rest
 * of search (reveal/focus/pan/fold-unfold, see App.tsx's
 * `revealAndFocus`) still works regardless of highlight support. */
const searchHighlight: Highlight | null =
  typeof CSS !== "undefined" && "highlights" in CSS && typeof Highlight !== "undefined" ? new Highlight() : null;
const searchCurrentHighlight: Highlight | null = searchHighlight ? new Highlight() : null;
if (searchHighlight && searchCurrentHighlight) {
  searchCurrentHighlight.priority = 1;
  CSS.highlights.set(SEARCH_HIGHLIGHT_NAME, searchHighlight);
  CSS.highlights.set(SEARCH_CURRENT_HIGHLIGHT_NAME, searchCurrentHighlight);
}

/**
 * Every scrollable ancestor of `range` up to (and including) `root` gets a
 * chance to bring it into its own view first, in both axes — same idea as
 * `Element.scrollIntoView()`, just walked by hand since a `Range` (not
 * necessarily backed by a single element) can't call that itself. Vertical
 * is the common case (`.mesh-node-body`'s own `overflow: auto`, a long
 * body capped by autolayout.ts's depth-≥2 `max-height`); horizontal
 * matters for a cached/live run output block's own `<pre>` (also
 * `overflow: auto`), whose `white-space: pre` content never wraps a long
 * line the way prose does. Written generally rather than hardcoded to
 * either one specific class, since any nested scroll container between
 * the match and `root` should count, on whichever axis it actually
 * overflows.
 *
 * That alone isn't always enough: a node with no such cap (root/depth-1 —
 * see autolayout.ts) just grows to fit its content, so a long enough one
 * has nothing to scroll *inside* at all — internally it's already
 * "fully shown", but the match can still be well outside the browser's
 * own visible viewport if the node itself is taller (or wider) than the
 * screen. Once every internal scroll above has done what it can, this
 * checks the range's final on-screen position against the window itself
 * and, if it's still out of view, pans the canvas via React Flow's own
 * `setCenter` — converting the range's *current* screen position to flow
 * coordinates first (`screenToFlowPosition`), then centering on that
 * point, the same mechanism App.tsx's `revealAndFocus` already uses for
 * centering a whole node's box, just aimed at this one occurrence's own
 * position within it instead. */
function ensureRangeVisible(
  range: Range,
  root: HTMLElement,
  rf: { setCenter: (x: number, y: number, opts?: { zoom?: number; duration?: number }) => void; getZoom: () => number; screenToFlowPosition: (pos: { x: number; y: number }) => { x: number; y: number } },
) {
  // `getBoundingClientRect()` (what `rangeRect`/`ancestorRect` below are
  // built from) reports *screen*-space pixels — already scaled by
  // whatever the canvas's current zoom is (a `transform: scale()` on
  // `.react-flow__viewport`, an ancestor of every node here). `scrollTop`/
  // `scrollLeft`/`clientHeight`/`clientWidth` are the opposite: always in
  // the *element's own* unscaled CSS pixels, same regardless of zoom.
  // Centering purely with screen-space deltas — this used to compute
  // `clientHeight / 2` directly (an unscaled value) against `rangeRect`/
  // `ancestorRect` (scaled ones) — silently mixed the two, correct only
  // by coincidence right at zoom 1. Dividing the final screen-space delta
  // by `zoom` before adding it to `scrollTop`/`scrollLeft` is what
  // actually converts it into the unscaled units scrolling expects;
  // using `ancestorRect.height`/`.width` (already screen-space, matching
  // `rangeRect`) instead of `clientHeight`/`clientWidth` keeps every term
  // in that same delta in one consistent space. Confirmed directly this
  // was wrong: a zoomed-out-enough canvas (more content than
  // `clickFitViewAndWait` alone ever exercised) landed a "centered" match
  // hundreds of screen pixels off — including, at an extreme enough zoom,
  // entirely outside the very ancestor it was supposedly centered within.
  const zoom = rf.getZoom();
  let ancestor = range.startContainer.parentElement;
  while (ancestor && root.contains(ancestor)) {
    if (ancestor.scrollHeight > ancestor.clientHeight) {
      const rangeRect = range.getBoundingClientRect();
      const ancestorRect = ancestor.getBoundingClientRect();
      if (rangeRect.top < ancestorRect.top || rangeRect.bottom > ancestorRect.bottom) {
        ancestor.scrollTop +=
          (rangeRect.top - ancestorRect.top - ancestorRect.height / 2 + rangeRect.height / 2) / zoom;
      }
    }
    // A cached/live run output block (`<pre>`, `overflow: auto`) never
    // wraps its content (`white-space: pre`, same as any real terminal
    // output) — a long enough line needs horizontal scrolling to bring a
    // match on it into view, same idea as the vertical case just above.
    // `getBoundingClientRect()` reports a range's geometric position
    // regardless of whatever clips it (clipping is a paint-time thing,
    // not a layout one) — so skipping this half doesn't just leave a
    // horizontally-scrolled-off match unscrolled, it also *poisons* the
    // window-visibility check and canvas-pan target below with an x
    // coordinate that was never actually the match's true on-screen
    // position, wherever this element's own clipped box happens to sit
    // (confirmed directly against README.md's own cached `meshfox -h`
    // output — the "3rd match" in `mcp` landed the camera on empty
    // canvas because the pan target's x came from this exact gap).
    if (ancestor.scrollWidth > ancestor.clientWidth) {
      const rangeRect = range.getBoundingClientRect();
      const ancestorRect = ancestor.getBoundingClientRect();
      if (rangeRect.left < ancestorRect.left || rangeRect.right > ancestorRect.right) {
        ancestor.scrollLeft +=
          (rangeRect.left - ancestorRect.left - ancestorRect.width / 2 + rangeRect.width / 2) / zoom;
      }
    }
    ancestor = ancestor.parentElement;
  }

  const rect = range.getBoundingClientRect();
  const inViewport =
    rect.top >= 0 && rect.left >= 0 && rect.bottom <= window.innerHeight && rect.right <= window.innerWidth;
  if (inViewport) return;
  const flowPoint = rf.screenToFlowPosition({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 });
  rf.setCenter(flowPoint.x, flowPoint.y, { zoom: rf.getZoom(), duration: 400 });
}

// How many consecutive, identically-positioned frames `waitForStableRect`
// needs before it trusts a range's on-screen position — see that
// function's own doc comment.
const STABLE_RECT_FRAMES_REQUIRED = 3;
// Hard cap on how long `waitForStableRect` will keep waiting for a
// position that never settles (e.g. some unrelated animation genuinely
// never stops) — `onSettled` still fires with whatever the position is by
// then, rather than never firing at all.
const STABLE_RECT_MAX_FRAMES = 30;

/**
 * Calls `onSettled` once `range`'s own `getBoundingClientRect()` stops
 * changing between animation frames (`STABLE_RECT_FRAMES_REQUIRED` in a
 * row, within `RECT_EPSILON`px — floating-point paint jitter, not a real
 * change), or once `STABLE_RECT_MAX_FRAMES` have passed either way.
 *
 * Exists because a single fixed delay (this used to be two nested
 * `requestAnimationFrame`s) isn't enough for a real, deep canvas: a newly-
 * unfolded node first mounts with an *estimated* height, and
 * autolayout.ts's real measurement — a `ResizeObserver`-driven reflow in
 * App.tsx (`measuredSignature`) — can take several of its own render
 * passes to settle, not just one, when unfolding reveals a whole subtree
 * rather than a single node (confirmed directly against README.md's own
 * "MCP server" section, several levels deep: two fixed frames settled a
 * small two-level test fixture fine but still occasionally landed the
 * camera on a position the real page hadn't actually reached yet). Rather
 * than guess a bigger fixed number and still be wrong for an even deeper
 * cascade, this just watches the actual quantity that matters — this
 * range's own screen position — and waits until *it* stops moving.
 *
 * Returns a cancel function (used from the calling effect's own cleanup,
 * same as a plain `requestAnimationFrame` id would need).
 */
function waitForStableRect(range: Range, onSettled: () => void): () => void {
  const RECT_EPSILON = 0.5;
  let cancelled = false;
  let frameId: number | undefined;
  let prev: DOMRect | null = null;
  let stableStreak = 0;
  let frameCount = 0;
  function closeEnough(a: DOMRect, b: DOMRect): boolean {
    return (
      Math.abs(a.top - b.top) < RECT_EPSILON &&
      Math.abs(a.left - b.left) < RECT_EPSILON &&
      Math.abs(a.bottom - b.bottom) < RECT_EPSILON &&
      Math.abs(a.right - b.right) < RECT_EPSILON
    );
  }
  function tick() {
    if (cancelled) return;
    const rect = range.getBoundingClientRect();
    stableStreak = prev && closeEnough(prev, rect) ? stableStreak + 1 : 0;
    prev = rect;
    frameCount++;
    if (stableStreak >= STABLE_RECT_FRAMES_REQUIRED || frameCount >= STABLE_RECT_MAX_FRAMES) {
      onSettled();
      return;
    }
    frameId = requestAnimationFrame(tick);
  }
  frameId = requestAnimationFrame(tick);
  return () => {
    cancelled = true;
    if (frameId !== undefined) cancelAnimationFrame(frameId);
  };
}

/**
 * Highlights every occurrence of `data.searchQuery` inside `rootRef`'s own
 * rendered text (title, tags, and — unless folded/still being edited —
 * body: prose, code, cached/live run output, ANSI output, all of it,
 * since this walks the actual live DOM rather than any one content type's
 * own source) via `searchHighlight` above, and — only for the current
 * search candidate (`data.focused`, set by App.tsx's `revealAndFocus`
 * alongside the unfold it already does) — brings this node's *current*
 * occurrence (`data.searchCurrentOccurrence`, App.tsx's own 0-based index
 * within this node's title+text, in the same order this function's own
 * DOM walk finds them: title before body) into view via
 * `ensureRangeVisible` above, not just whichever occurrence happened to be
 * first — a query appearing more than once in one node is that many
 * separate stops (see App.tsx's own `searchMatches` doc comment), each
 * with its own scroll/pan target. That same current occurrence's `Range`
 * also goes into `searchCurrentHighlight`, on top of (not instead of)
 * `searchHighlight`, so it reads as visually distinct from every other
 * match rather than just another same-colored highlight — the only cue
 * beforehand was this node's own `data-focused` outline, useless for
 * telling apart two occurrences *within* the one focused node.
 *
 * Deliberately a DOM `Range` overlay, not real `<mark>` elements spliced
 * into the tree by hand — the body this scans is whatever
 * `ReactMarkdown`/Shiki/`AnsiText` last rendered, still fully owned and
 * reconciled by React; wrapping its text nodes here would fight React's
 * own diffing the next time any of that content actually changes (a live
 * run streaming more output, a re-fetch after `recheck`, ...).
 *
 * `skip` is `true` while this node's own inline text editor is open
 * (`editingText` in `MeshNode`) — its content is a live edit buffer, not
 * rendered prose, and not what search matched against to begin with.
 */
function useSearchHighlight(
  rootRef: React.RefObject<HTMLElement | null>,
  data: Pick<MeshNodeData, "searchQuery" | "searchCurrentOccurrence" | "focused" | "folded" | "text" | "title">,
  skip: boolean,
) {
  // `useReactFlow()`'s own returned object/methods aren't guaranteed
  // referentially stable across renders — read via a ref updated every
  // render instead of putting them in the effect's own dependency array,
  // so a change in *their* identity alone never re-triggers this (which
  // would otherwise risk re-scrolling/re-panning on renders that have
  // nothing to do with search at all).
  const rf = useReactFlow();
  const rfRef = useRef(rf);
  rfRef.current = rf;

  useEffect(() => {
    if (!searchHighlight) return;
    const root = rootRef.current;
    const query = skip ? undefined : data.searchQuery?.trim();
    if (!root || !query) return;
    const lowerQuery = query.toLowerCase();
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const ranges: Range[] = [];
    let textNode: Node | null;
    while ((textNode = walker.nextNode())) {
      const text = textNode.textContent ?? "";
      const lowerText = text.toLowerCase();
      let from = 0;
      let idx: number;
      while ((idx = lowerText.indexOf(lowerQuery, from)) !== -1) {
        const range = new Range();
        range.setStart(textNode, idx);
        range.setEnd(textNode, idx + query.length);
        ranges.push(range);
        from = idx + query.length;
      }
    }
    for (const r of ranges) searchHighlight.add(r);

    let currentRange: Range | undefined;
    // Waits for `target`'s own on-screen position to actually settle
    // before measuring/panning to it — see `waitForStableRect`'s own doc
    // comment for why a fixed frame delay isn't enough (a node that just
    // got unfolded this same render mounts with an *estimated* height
    // first; autolayout.ts's real measurement can take several of its own
    // render passes to land, not just one or two, for a deep enough
    // cascade).
    let cancelSettle: (() => void) | undefined;
    if (data.focused && ranges.length > 0) {
      const occurrence = Math.min(Math.max(data.searchCurrentOccurrence ?? 0, 0), ranges.length - 1);
      currentRange = ranges[occurrence];
      searchCurrentHighlight?.add(currentRange);
      const target = currentRange;
      cancelSettle = waitForStableRect(target, () => ensureRangeVisible(target, root, rfRef.current));
    }

    return () => {
      for (const r of ranges) searchHighlight.delete(r);
      if (currentRange) searchCurrentHighlight?.delete(currentRange);
      cancelSettle?.();
    };
  }, [rootRef, data.searchQuery, data.searchCurrentOccurrence, data.focused, data.folded, data.text, data.title, skip]);
}

/** JSON Canvas's six numbered color presets — same hex values as
 * `NodeSettings`' own swatch buttons, so what you pick there is exactly
 * what renders here. A color that isn't one of these six is a literal hex
 * string, passed through unchanged. */
const COLOR_PRESETS: Record<string, string> = {
  "1": "#c22b2b",
  "2": "#d9822b",
  "3": "#d9c02b",
  "4": "#3d9e4f",
  "5": "#3d6ef5",
  "6": "#a05dd1",
};

export function resolveNodeColor(color: string | undefined): string | undefined {
  if (!color) return undefined;
  return COLOR_PRESETS[color] ?? color;
}

/** How long the target block's highlight flash stays visible after jumping
 * to it — long enough to catch the eye, short enough not to linger. */
const JUMP_HIGHLIGHT_MS = 1200;

/** Read-only, syntax-highlighted view of one fenced block's own source —
 * Shiki (see `./shiki.ts`), fed a fence's already-in-hand `code` string
 * directly instead of fetching one from the server. Previously
 * `RunnableCodeBlock` rendered this as a bare `<pre><code>{seg.code}</code>
 * </pre>` with no highlighting wired up at all (TODO.canvas.md: "Подсветка
 * синтаксиса в блоках кода в webui не работает") — falls back to that same
 * plain rendering while the grammar is still loading, or when `lang`
 * doesn't match anything Shiki (bundled or a local/global custom grammar)
 * knows.
 */
function HighlightedCode({ code, lang }: { code: string; lang: string }) {
  const [html, setHtml] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    highlightToHtml(code, lang).then((h) => {
      if (!cancelled) setHtml(h);
    });
    return () => {
      cancelled = true;
    };
  }, [code, lang]);

  if (html === null) {
    return (
      <div className="mesh-code-block-source nodrag nopan">
        <pre>
          <code>{code}</code>
        </pre>
      </div>
    );
  }
  return (
    <div
      className="mesh-code-block-source mesh-shiki nodrag nopan"
      // eslint-disable-next-line react/no-danger -- Shiki's own trusted HTML output, not user input
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

function RunnableCodeBlock({ seg, data, nodeId }: { seg: CodeSegment; data: MeshNodeData; nodeId: string }) {
  // A `button` fence has no real code of its own to show/edit — its whole
  // point is a prominent shortcut to its `deps=` chain (see SPEC.md's
  // "Button fences") — so it gets an entirely different, much smaller
  // rendering instead of the ordinary code-editor block below.
  if (seg.lang === BUTTON_LANG) {
    return <ButtonBlock seg={seg} data={data} nodeId={nodeId} />;
  }
  // Expanded by default (unlike a constraint fence — see
  // `ConstraintFenceBlock`): a runnable block's own code and output are
  // usually the point of reading a node at all. The run/chain/kill buttons
  // in the head stay available either way, so collapsing one doesn't stop
  // it from being run — just hides its source and output until reopened.
  const [expanded, setExpanded] = useState(true);
  const live = data.liveBlocks[seg.name];
  const queued = live?.status === "queued";
  const running = live?.status === "running";
  const busy = queued || running;
  const chainBusy = busy && live?.viaChain;
  const runBusy = busy && !live?.viaChain;
  const hasDeps = seg.deps.length > 0;
  const { setCenter, getNode, getZoom } = useReactFlow();

  const cacheHint = !seg.cache
    ? "output is not cached"
    : data.editMode
      ? "output will be cached in the canvas file"
      : "output won't be saved — click Edit to persist it";
  const chainTitle = hasDeps
    ? `runs its dependency chain first, then this block (${cacheHint}): ${seg.deps.join(", ")} → ${seg.name}`
    : cacheHint;
  const runTitle = hasDeps
    ? `runs only this block, skipping its deps= chain (${cacheHint})`
    : cacheHint;

  // Jumps to a dependency's own block: pans/centers its owning node (if
  // it's a different one) via React Flow, then scrolls to and briefly
  // highlights the block itself inside that node's body.
  const jumpTo = (raw: string) => {
    const target = parseBlockRef(raw, nodeId);
    if (target.nodeId !== nodeId) {
      const flowNode = getNode(target.nodeId);
      if (flowNode) {
        const w = flowNode.measured?.width ?? flowNode.width ?? 280;
        const h = flowNode.measured?.height ?? flowNode.height ?? 160;
        setCenter(flowNode.position.x + w / 2, flowNode.position.y + h / 2, {
          zoom: getZoom(),
          duration: 400,
        });
      }
    }
    const el = document.getElementById(blockDomId(target));
    if (!el) return;
    el.scrollIntoView({ behavior: "smooth", block: "center" });
    el.classList.add("mesh-code-block-flash");
    window.setTimeout(() => el.classList.remove("mesh-code-block-flash"), JUMP_HIGHLIGHT_MS);
  };

  const runLabel = !runBusy ? `run ${seg.name}` : queued ? "queued…" : `running ${seg.name}…`;
  const chainLabel = !chainBusy ? `⛓ run chain: ${seg.name}` : queued ? "queued…" : "running chain…";
  // `tty` opens a `TtyPanel` (a real terminal) instead of streaming
  // captured output into this node's body — a separate handler, since
  // it's not the same "queued/running/killed in liveBlocks" state machine
  // at all (see App.tsx's `onRunTty` vs `onRun`).
  const runHandler = seg.tty
    ? () => data.onRunTty(seg.name, false, seg.autoclose)
    : () => data.onRun(seg.name, false);
  const chainHandler = seg.tty
    ? () => data.onRunTty(seg.name, true, seg.autoclose)
    : () => data.onRun(seg.name, true);

  return (
    <div className="mesh-code-block" id={blockDomId({ nodeId, blockName: seg.name })}>
      <div className="mesh-code-block-head">
        <FoldToggle
          folded={!expanded}
          onToggle={() => setExpanded((e) => !e)}
          foldedTitle="Show the code"
          unfoldedTitle="Hide the code"
        />
        <span className="mesh-code-lang">
          {seg.lang}
          {seg.interpreter && <span className="mesh-code-interpreter"> #!{seg.interpreter}</span>}
        </span>
        {seg.tty && (
          <span className="mesh-tty-badge" title="Runs in a real interactive terminal, not captured/streamed output">
            tty
          </span>
        )}
        <button
          disabled={queued || running}
          onClick={runHandler}
          title={seg.tty ? "Opens an interactive terminal for this block" : runTitle}
        >
          {runLabel}
        </button>
        {hasDeps && (
          <button
            className="mesh-run-chain"
            disabled={queued || running}
            onClick={chainHandler}
            title={seg.tty ? "Runs its dependency chain first, then opens an interactive terminal for this block" : chainTitle}
          >
            {chainLabel}
          </button>
        )}
        {running && (
          <button
            type="button"
            className="mesh-kill-button"
            onClick={() => data.onKill(seg.name)}
            title="Kill this run — terminates the process (and anything it spawned), and stops the rest of its dependency chain, in case it's hung"
          >
            ⏹ kill
          </button>
        )}
      </div>
      {expanded && (
        <>
          {hasDeps && (
            <div className="mesh-code-deps">
              after:{" "}
              {seg.deps.map((raw, i) => (
                <span key={raw}>
                  {i > 0 && ", "}
                  <button
                    type="button"
                    className="mesh-dep-link"
                    onClick={() => jumpTo(raw)}
                    title={`jump to ${raw}`}
                  >
                    {raw}
                  </button>
                </span>
              ))}
            </div>
          )}
          <HighlightedCode code={seg.code} lang={seg.lang} />
          {!seg.tty && <RunOutput seg={seg} live={live} assetBase={data.assetBase} />}
        </>
      )}
    </div>
  );
}

/** Renders a `` ```button `` fence (see SPEC.md's "Button fences") as a
 * single prominent button, with no card/border around it — just the
 * button itself (plus a kill button alongside it while running), in place
 * of the ordinary bordered code-editor block. There's no separate
 * `label=` attribute: the fence's own body *is* its caption (falling back
 * to its `name` when blank), never executed as code — clicking it only
 * ever runs its `deps=` chain (the equivalent of "⛓ run chain" — there's
 * no "just this block" to run, since it has no real code of its own). */
function ButtonBlock({ seg, data, nodeId }: { seg: CodeSegment; data: MeshNodeData; nodeId: string }) {
  const live = data.liveBlocks[seg.name];
  const queued = live?.status === "queued";
  const running = live?.status === "running";
  const busy = queued || running;
  const caption = seg.code.trim() || seg.name;
  const buttonLabel = !busy ? caption : queued ? "queued…" : "running…";

  return (
    <div className="mesh-button-block" id={blockDomId({ nodeId, blockName: seg.name })}>
      <button
        type="button"
        className="mesh-run-button"
        disabled={busy}
        onClick={() => data.onRun(seg.name, true)}
        title={seg.deps.length > 0 ? `runs its dependency chain: ${seg.deps.join(", ")} → ${seg.name}` : caption}
      >
        {buttonLabel}
      </button>
      {running && (
        <button
          type="button"
          className="mesh-kill-button"
          onClick={() => data.onKill(seg.name)}
          title="Kill this run — terminates the process (and anything it spawned), and stops the rest of its dependency chain, in case it's hung"
        >
          ⏹ kill
        </button>
      )}
    </div>
  );
}

/** Mirrors `core::output::format_duration_ms` — kept in sync by hand (same
 * split this codebase already uses for every other Rust/TS syntax pair, see
 * SPEC.md's "Formal grammar" intro): `"842ms"` under a second, `"2.3s"`
 * under a minute, `"1m 05s"` beyond that. */
function formatDurationMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const totalSeconds = Math.round(ms / 1000);
  if (totalSeconds < 60) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(totalSeconds / 60)}m ${String(totalSeconds % 60).padStart(2, "0")}s`;
}

/** A ticking elapsed-time counter for a still-`"running"` step — like
 * Livebook's own live cell timer. Purely client-side wall-clock
 * (`Date.now() - startedAt`, re-rendered every 200ms via its own interval)
 * — an approximation good enough to watch tick up, not meant to match the
 * server's own authoritative `durationMs` (see `LiveBlockState.startedAt`'s
 * own doc comment) to the millisecond. */
function LiveElapsed({ startedAt }: { startedAt: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 200);
    return () => window.clearInterval(id);
  }, []);
  return <>{formatDurationMs(Math.max(0, now - startedAt))}</>;
}

/** The `"skipped"` branch of `LiveRunOutput` — never the block actually
 * requested (only a pulled-in dependency — see SPEC.md's "Runnable code
 * fences"), so there's no fresh output from *this* run: the server sends
 * whatever it printed last time it *actually* ran instead (`step-skipped`'s
 * own `output`/`durationMs`, see `./api.ts`'s `RunEvent`), shown here
 * collapsed by default — it's stale-by-definition (the whole point of the
 * skip is "nothing changed since"), so worth having a click away without
 * cluttering a chain of several skipped steps in a row by default. */
function SkippedRunOutput({ live }: { live: LiveBlockState }) {
  const [expanded, setExpanded] = useState(false);
  const duration =
    live.durationMs !== undefined ? ` · ${formatDurationMs(live.durationMs)}` : undefined;
  return (
    <div className="mesh-code-output" data-exit="skipped">
      <div className="mesh-code-output-head">
        {live.text && (
          <FoldToggle
            folded={!expanded}
            onToggle={() => setExpanded((e) => !e)}
            foldedTitle="Show its output from that earlier run"
            unfoldedTitle="Hide its output from that earlier run"
          />
        )}
        skipped · already ran this session, unchanged{duration}
      </div>
      {expanded && live.text && (
        <pre>
          <code><AnsiText text={live.text} /></code>
        </pre>
      )}
    </div>
  );
}

/** The `"blocked"` branch of `LiveRunOutput` — this block never actually
 * ran (see `LiveBlockState.status`'s doc comment): something earlier in its
 * own chain failed or was killed, so the server gave up on the rest of the
 * chain before ever reaching it. No output/duration to show (there's
 * nothing to expand, unlike `SkippedRunOutput`), just why it's not the
 * "queued…" it was stuck reading before this status existed — the run/chain
 * buttons above this are already re-enabled (this status is neither `busy`
 * in `RunnableCodeBlock`), so retrying is just clicking them again. */
function BlockedRunOutput() {
  return (
    <div className="mesh-code-output" data-exit="blocked">
      <div className="mesh-code-output-head">blocked · a dependency in its chain failed</div>
    </div>
  );
}

/** Renders a `output="markdown"` block's captured stdout as real Markdown
 * (SPEC.md's "Cached output") instead of the `<pre><AnsiText/></pre>` every
 * other block's output gets — same `ReactMarkdown` setup (plugins,
 * `assetBase`-aware link/image resolution) `MeshNodeBody`'s own prose
 * segments use, via `makeMarkdownComponents`, so an `output="markdown"`
 * block's rendered table/etc. looks and behaves exactly like any other
 * Markdown in the node.
 *
 * `stderrText`, if present (`core::output::render_output_block_markdown`
 * captures stderr separately from stdout — see `CachedOutput.stderrText`'s
 * own doc comment), renders first as an ordinary `<pre><AnsiText/></pre>`
 * block, same treatment `RunOutput`'s default text-mode rendering already
 * gives a whole block's output — stderr was never meant to be parsed as
 * this block's Markdown content, so it stays visually distinct from it
 * rather than folded into the same `ReactMarkdown` call.
 *
 * The `.mesh-code-output-markdown` wrapper gives this (both halves
 * together) its own inset padding and dashed top separator (`index.css`),
 * mirroring `.mesh-code-output pre`'s own treatment — `.mesh-code-output`
 * itself has no padding of its own (every other kind of content it holds,
 * `pre` included, supplies its own), and a bare `<table>`'s per-cell
 * borders would otherwise land flush against the box's own border with
 * nothing else here to create that gap. */
function MarkdownOutput({
  text,
  stderrText,
  assetBase,
}: {
  text: string;
  stderrText?: string;
  assetBase?: string;
}) {
  const components = useMemo(() => makeMarkdownComponents(assetBase), [assetBase]);
  return (
    <div className="mesh-code-output-markdown">
      {stderrText && (
        <pre>
          <code><AnsiText text={stderrText} /></code>
        </pre>
      )}
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkImageAttrs, remarkSubSup, remarkGfmAlerts]}
        components={components}
        urlTransform={allowDataImageUrls}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}

/** Live output (queued/running/done/killed, from the current run) — shared
 * by `RunOutput` (a fenced block's own output, falling back to its cached
 * copy when nothing's live — see below) and a runnable `file` node's own
 * body (`NodeBodyContent`), which has no cached copy to fall back to at
 * all. Never shown at all for a read-only run beyond this component's own
 * lifetime (a page reload loses it, same as the transient-output behavior
 * this replaces always had).
 *
 * `outputMarkdown` (default `false`, since a `file` node's own live output
 * has no `output=` attribute to read it from) only switches the *finished*
 * (`"done"`) rendering to Markdown — a still-`"running"` stream is shown
 * raw regardless, the same way a Jupyter cell doesn't try to incrementally
 * render partial rich output either; it only ever renders once a
 * `display_data` payload is complete. */
function LiveRunOutput({
  live,
  outputMarkdown = false,
  assetBase,
}: {
  live: LiveBlockState;
  outputMarkdown?: boolean;
  assetBase?: string;
}) {
  if (live.status === "skipped") {
    return <SkippedRunOutput live={live} />;
  }
  if (live.status === "blocked") {
    return <BlockedRunOutput />;
  }
  const duration =
    live.durationMs !== undefined ? ` · ${formatDurationMs(live.durationMs)}` : undefined;
  const label =
    live.status === "killed" ? (
      <>killed{duration}</>
    ) : live.status === "running" ? (
      <>running… {live.startedAt !== undefined && <LiveElapsed startedAt={live.startedAt} />}</>
    ) : (
      <>
        output · exit {live.exitCode}
        {duration}
      </>
    );
  const exitState =
    live.status === "killed" ? "killed" : live.status === "running" ? "running" : live.exitCode === 0 ? "ok" : "fail";
  // `RunEvent::Output`'s own `stream` tag (see `App.tsx`'s `appendOutputLine`)
  // is what makes this possible at all: `live.stdoutText`/`.stderrText`
  // accumulate separately from `live.text` (the merged view, still used
  // for every non-markdown block) as each line arrives, the same split
  // `ExecOutput.stdout`/`.stderr` gives a `cache`d run's persisted result —
  // so the live "done" view renders correctly split even before `App.tsx`
  // reloads the canvas and swaps in the cached copy.
  const renderMarkdown =
    outputMarkdown && live.status === "done" && (live.stdoutText !== undefined || live.stderrText !== undefined);
  return (
    <div className="mesh-code-output" data-exit={exitState}>
      <div className="mesh-code-output-head">
        {label}
        <span className="mesh-code-output-transient"> · not saved</span>
      </div>
      {renderMarkdown && (
        <MarkdownOutput text={live.stdoutText ?? ""} stderrText={live.stderrText} assetBase={assetBase} />
      )}
      {live.text && !renderMarkdown && (
        <pre>
          <code><AnsiText text={live.text} /></code>
        </pre>
      )}
    </div>
  );
}

/** A fenced code block's own output: live (queued/running/done/killed, from
 * the current run) takes over from the cached copy in the file whenever
 * one's in progress or just finished — cleared back to the cached view once
 * `App.tsx` reloads the canvas after a persisted run. */
function RunOutput({ seg, live, assetBase }: { seg: CodeSegment; live?: LiveBlockState; assetBase?: string }) {
  if (live && live.status !== "queued") {
    return <LiveRunOutput live={live} outputMarkdown={seg.outputMarkdown} assetBase={assetBase} />;
  }
  if (!live && seg.output) {
    return (
      <div className="mesh-code-output" data-exit={seg.output.exitCode === 0 ? "ok" : "fail"} data-stale={seg.output.stale || undefined}>
        <div className="mesh-code-output-head">
          output · exit {seg.output.exitCode}
          {seg.output.durationText && ` · ${seg.output.durationText}`}
          {seg.output.stale && (
            <span className="mesh-code-output-stale" title="The code changed since this output was captured — re-run to refresh it.">
              {" "}
              · stale
            </span>
          )}
        </div>
        {seg.output.text && seg.outputMarkdown && (
          <MarkdownOutput text={seg.output.text} stderrText={seg.output.stderrText} assetBase={assetBase} />
        )}
        {seg.output.text && !seg.outputMarkdown && (
          <pre>
            <code><AnsiText text={seg.output.text} /></code>
          </pre>
        )}
      </div>
    );
  }
  return null;
}

/** `target`'s own extension (lowercase, no dot) as a best-effort Shiki
 * language id — used only when a `file` node has no explicit `lang=` of
 * its own. Not a real filename→language mapping table (Shiki has no
 * public equivalent of CodeMirror's `LanguageDescription.matchFilename`);
 * relies on the extension itself already being a Shiki bundled-language
 * alias (true for most common ones — `rs`, `py`, `ts`, ...) or a matching
 * local/global custom-grammar filename stem (see `shiki.ts`'s
 * `grammarNameMatches`). Anything else just renders unhighlighted, same
 * "still show *something*" fallback the rest of this component already
 * relies on.
 */
function extensionOf(target: string | undefined): string | undefined {
  const dot = target?.lastIndexOf(".");
  return dot && dot >= 0 ? target?.slice(dot + 1).toLowerCase() : undefined;
}

/**
 * A `file` node's `display: "code"` body (see SPEC.md): fetches the
 * target's own content fresh from the server on every mount
 * (`fetchNodeFileContent`, confined server-side to the canvas's own
 * directory) and renders it as a read-only, syntax-highlighted (Shiki, see
 * `./shiki.ts`) view — never runnable, unlike a node's own fenced code
 * blocks. `lang` picks the highlighting grammar when set; otherwise it's
 * guessed from the target's file extension. Falls back to the plain link
 * view on any error (missing file, binary content, a target outside the
 * canvas directory) — a node whose file preview can't load should still
 * show *something* useful, not go blank.
 */
function FileCodePreview({ nodeId, target, lang }: { nodeId: string; target?: string; lang?: string }) {
  const wheelRef = useStopWheelIfScrollable<HTMLDivElement>();
  const [state, setState] = useState<
    | { status: "loading" }
    | { status: "error"; message: string }
    | { status: "ready"; content: string; truncated: boolean; html: string }
  >({ status: "loading" });

  useEffect(() => {
    let cancelled = false;
    setState({ status: "loading" });
    fetchNodeFileContent(nodeId)
      .then(async ({ content, truncated }) => {
        const html = await highlightToHtml(content, lang ?? extensionOf(target) ?? "text");
        if (cancelled) return;
        setState({ status: "ready", content, truncated, html });
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setState({ status: "error", message: err instanceof Error ? err.message : String(err) });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId, target, lang]);

  if (state.status === "error") {
    return (
      <div className="mesh-node-body nopan">
        {target ? (
          <a href={target} target="_blank" rel="noreferrer">
            {target}
          </a>
        ) : (
          <em>no target</em>
        )}
        <p className="mesh-node-hint">code preview unavailable: {state.message}</p>
      </div>
    );
  }
  if (state.status === "loading") {
    return <p className="mesh-node-hint">loading preview…</p>;
  }
  return (
    <div className="mesh-file-code-preview mesh-shiki nodrag nopan" ref={wheelRef}>
      {/* eslint-disable-next-line react/no-danger -- Shiki's own trusted HTML output, not user input */}
      <div dangerouslySetInnerHTML={{ __html: state.html }} />
      {state.truncated && <p className="mesh-node-hint">preview truncated to the first part of the file</p>}
    </div>
  );
}

/** Module-level so remounting the same node (a canvas re-layout, or
 * expanding it into `NodeExpandPanel`) doesn't re-request a preview this
 * tab has already seen — the server itself already caches per-process
 * (see `crates/server/src/link_preview.rs`), this just avoids the
 * redundant round trip. Keyed by URL, not node id, so two link nodes
 * pointing at the same target share one fetch. */
const linkPreviewCache = new Map<string, Promise<LinkPreview | null>>();

function getLinkPreview(url: string): Promise<LinkPreview | null> {
  let pending = linkPreviewCache.get(url);
  if (!pending) {
    pending = fetchLinkPreview(url).catch(() => null);
    linkPreviewCache.set(url, pending);
  }
  return pending;
}

/**
 * A `link` node's `preview: true` card: fetches (or reuses the
 * already-cached, see `getLinkPreview`) OpenGraph title/description/image
 * for `target` and renders it below the plain link. Renders nothing at all
 * once loaded if the server had nothing to show (blocked, unreachable, not
 * HTML, no OG tags) — same "just degrade to the plain link" spirit as
 * `FileCodePreview`'s own error fallback, just without even a hint
 * message, since "no preview" for an arbitrary link is unremarkable.
 */
function LinkPreviewCard({ target }: { target: string }) {
  const [preview, setPreview] = useState<LinkPreview | null | "loading">("loading");

  useEffect(() => {
    let cancelled = false;
    setPreview("loading");
    getLinkPreview(target).then((result) => {
      if (!cancelled) setPreview(result);
    });
    return () => {
      cancelled = true;
    };
  }, [target]);

  if (preview === "loading") {
    return <p className="mesh-node-hint">loading preview…</p>;
  }
  if (!preview || (!preview.title && !preview.description && !preview.image)) {
    return null;
  }
  return (
    <a href={target} target="_blank" rel="noreferrer" className="mesh-link-preview-card nodrag">
      {preview.image && <img src={preview.image} alt="" className="mesh-link-preview-image" />}
      {(preview.title || preview.description) && (
        <div className="mesh-link-preview-text">
          {preview.title && <div className="mesh-link-preview-title">{preview.title}</div>}
          {preview.description && <div className="mesh-link-preview-description">{preview.description}</div>}
        </div>
      )}
    </a>
  );
}

/**
 * Read-only preview of a node's Markdown body — same segment parsing
 * (`parseBody`) and Markdown rendering `MeshNodeBody` uses, but code blocks
 * render plain (language label + code, no Run button, no live/cached
 * output). Used by `NodeTextEditor`'s preview pane, which is previewing
 * draft — possibly invalid, definitely unsaved — text, not something safe
 * to offer execution controls for.
 */
export function NodeBodyPreview({ text }: { text: string }) {
  const segments = parseBody(text, "preview");
  const wheelRef = useStopWheelIfScrollable<HTMLDivElement>();
  return (
    <div className="mesh-node-body nopan" ref={wheelRef}>
      {segments.map((seg, i) => {
        if (seg.type === "markdown") {
          return (
            <ReactMarkdown
              key={i}
              remarkPlugins={[remarkGfm, remarkImageAttrs, remarkSubSup, remarkGfmAlerts]}
              components={markdownComponents}
              urlTransform={allowDataImageUrls}
            >
              {seg.content}
            </ReactMarkdown>
          );
        }
        if (seg.type === "code" && seg.lang === BUTTON_LANG) {
          // Same static, disabled-look rendering `ButtonBlock` would give
          // this fence in the real canvas — no run/kill state to show here
          // (this is an unsaved draft, see this component's own doc
          // comment), just what it would look like.
          return (
            <div className="mesh-button-block" key={`${seg.name}-${i}`}>
              <button type="button" className="mesh-run-button" disabled>
                {seg.code.trim() || seg.name}
              </button>
            </div>
          );
        }
        const highlightLang = seg.type === "constraint" ? "starlark" : seg.lang;
        const lang = seg.type === "constraint" ? `starlark${seg.name ? ` · ${seg.name}` : ""}` : seg.lang;
        return (
          <div className="mesh-code-block" key={`${seg.type === "constraint" ? "constraint" : seg.name}-${i}`}>
            <div className="mesh-code-block-head">
              <span className="mesh-code-lang">
                {lang}
                {seg.type === "code" && seg.interpreter && (
                  <span className="mesh-code-interpreter"> #!{seg.interpreter}</span>
                )}
              </span>
            </div>
            <HighlightedCode code={seg.code} lang={highlightLang} />
          </div>
        );
      })}
    </div>
  );
}

/**
 * Renders a text node's body: the plain stacked list of markdown/code
 * segments. Each block's own dependencies (if any) are spelled out inline
 * via its "after: …" links.
 */

/** One embedded ` ```starlark constraint ` fence within a node's body: its
 * Starlark source, read-only, with a "recheck" button in place of the Run
 * button a runnable fence would get (Starlark isn't a `run`nable language —
 * see `MeshNodeData.onRun`'s counterpart, `onRecheckConstraint`) — and,
 * once evaluated and failing, every `fail(msg)` the script raised, right
 * under the code (the same information the title-bar badge's tooltip has,
 * but readable without hovering, and not truncated to one line).
 *
 * Collapsed by default: the title-bar `ConstraintBadge` already carries the
 * pass/fail signal for the node as a whole, so the raw Starlark itself is
 * rarely what a reader actually came for — except right when it's failing,
 * which is exactly when the detail here (not just the badge's tooltip) is
 * the point, so a failing fence starts expanded instead. This is only the
 * *initial* state on mount; a manual toggle afterward stays put (a later
 * `recheck` doesn't collapse it back out from under whoever just opened
 * it).
 */
function ConstraintFenceBlock({
  seg,
  status,
  onRecheck,
}: {
  seg: ConstraintSegment;
  status: ConstraintStatusDto | undefined;
  onRecheck: () => void;
}) {
  const [expanded, setExpanded] = useState(() => !(status?.ok ?? true));
  return (
    <div className="mesh-code-block">
      <div className="mesh-code-block-head">
        <FoldToggle
          folded={!expanded}
          onToggle={() => setExpanded((e) => !e)}
          foldedTitle="Show the Starlark source"
          unfoldedTitle="Hide the Starlark source"
        />
        <span className="mesh-code-lang">starlark{seg.name ? ` · ${seg.name}` : ""}</span>
        <button onClick={onRecheck} title="Re-fetch the canvas and re-evaluate every constraint">
          ↻ recheck
        </button>
      </div>
      {expanded && (
        <>
          <HighlightedCode code={seg.code} lang="starlark" />
          {status && !status.ok && (
            <div className="mesh-code-output" data-exit="fail">
              <div className="mesh-code-output-head">{status.messages.length} failing</div>
              <pre>
                <code>{status.messages.join("\n")}</code>
              </pre>
            </div>
          )}
        </>
      )}
    </div>
  );
}

function MeshNodeBody({ data, nodeId }: { data: MeshNodeData; nodeId: string }) {
  const segments = parseBody(data.text, nodeId);
  const wheelRef = useStopWheelIfScrollable<HTMLDivElement>();
  const components = useMemo(() => makeMarkdownComponents(data.assetBase), [data.assetBase]);

  let constraintIdx = 0;
  return (
    <div className="mesh-node-body nopan" ref={wheelRef}>
      {segments.map((seg, i) => {
        if (seg.type === "markdown") {
          return (
            <ReactMarkdown
              key={i}
              remarkPlugins={[remarkGfm, remarkImageAttrs, remarkSubSup, remarkGfmAlerts]}
              components={components}
              urlTransform={allowDataImageUrls}
            >
              {seg.content}
            </ReactMarkdown>
          );
        }
        if (seg.type === "constraint") {
          // Matched to `data.constraintResults` purely by position — both
          // are built from the same document order (see
          // `MeshNodeData.constraintResults`'s doc comment).
          const status = data.constraintResults?.[constraintIdx];
          constraintIdx += 1;
          return (
            <ConstraintFenceBlock key={`constraint-${i}`} seg={seg} status={status} onRecheck={data.onRecheckConstraint} />
          );
        }
        return <RunnableCodeBlock key={seg.name} seg={seg} data={data} nodeId={nodeId} />;
      })}
    </div>
  );
}

/**
 * Picks (and renders) a node's own body area by type — the same switch
 * `MeshNode` uses for its inline box, factored out so `NodeExpandPanel`
 * (the "expand into a floating window" view) can render *exactly* the
 * same live, interactive body (run/kill buttons, streaming output, the
 * deps rail) at a larger size, instead of a separate read-only copy that
 * could drift from what the node itself shows.
 */
export function NodeBodyContent({ data, nodeId }: { data: MeshNodeData; nodeId: string }) {
  if (data.nodeType === "group") return null;
  // A runnable file node's live run state is keyed under its own id in
  // `liveBlocks` — same "the block shares its node's own id" convention a
  // `text` node's sole implicit block already uses (see `App.tsx`'s
  // `executeFileRun`). No cached copy to fall back to (unlike a fenced
  // block's `seg.output`): a `file` node has no `cache` concept.
  const fileRunLive = data.interpreter ? data.liveBlocks[nodeId] : undefined;
  if (data.nodeType === "file" && data.display === "code") {
    return (
      <>
        <FileCodePreview nodeId={nodeId} target={data.target} lang={data.lang} />
        {fileRunLive && <LiveRunOutput live={fileRunLive} />}
      </>
    );
  }
  if (data.nodeType === "file" || data.nodeType === "link") {
    return (
      <div className="mesh-node-body nopan">
        {data.target ? (
          <a href={data.target} target="_blank" rel="noreferrer">
            {data.target}
          </a>
        ) : (
          <em>no target</em>
        )}
        {data.nodeType === "link" && data.preview && data.target && (
          <LinkPreviewCard target={data.target} />
        )}
        {fileRunLive && <LiveRunOutput live={fileRunLive} />}
      </div>
    );
  }
  return <MeshNodeBody data={data} nodeId={nodeId} />;
}

export function MeshNode({ id, data, selected }: NodeProps & { data: MeshNodeData }) {
  const [editingText, setEditingText] = useState(false);
  const nodeRootRef = useRef<HTMLDivElement | null>(null);
  useSearchHighlight(nodeRootRef, data, editingText);
  const isTextNode = data.nodeType === "text";
  const isGroup = data.nodeType === "group";
  // See `MeshNodeData.autoOpenTextEditor`'s own doc comment — App.tsx
  // already only ever sets this for a text-type node, but guards it here
  // too rather than trusting that invariant blindly (`plainMarkdownInclude`
  // in particular has no inline editor to open at all, see the ✏ button's
  // own branch below).
  useEffect(() => {
    if (!data.autoOpenTextEditor) return;
    if (isTextNode && !data.plainMarkdownInclude) {
      setEditingText(true);
    }
    data.onAutoOpenTextEditorConsumed?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data.autoOpenTextEditor]);
  const nodeColor = resolveNodeColor(data.effectiveColor ?? data.color);
  // A heading-only node (no body Markdown at all) has nothing to show in
  // its body area — while read-only, that's just dead space under a
  // left-aligned title, so it gets a distinct, centered-title layout
  // instead. Edit mode keeps the normal title bar regardless (still needs
  // its own row for the edit/settings/delete buttons, and staying put
  // avoids the layout jumping under the cursor mid-edit the moment the
  // first character is typed).
  const isTitleOnly = !data.editMode && isTextNode && data.text.trim() === "";

  // The title bar's "▷ run" quick-run button: for a `text` node, its one
  // unambiguous default block (explicit `default` flag or self-named, same
  // rule `core::fence::default_block` uses — see `defaultBlock`); for a
  // runnable `file` node (`interpreter` set), the node's own id, matching
  // the same "the block shares its node's own id" convention its live run
  // state is keyed under (see `App.tsx`'s `executeFileRun`) — a file node
  // is never `tty` (that's only ever a fenced block's own attribute, see
  // `CodeSegment.tty`), so `quickRunIsTty` only checks `quickRunBlock`.
  // `null` when neither applies (nothing to quick-run) — the button just
  // isn't shown.
  const quickRunBlock = isTextNode ? defaultBlock(data.text, id) : null;
  const quickRunBlockName =
    quickRunBlock?.name ?? (data.nodeType === "file" && data.interpreter && data.target ? id : null);
  // A `tty` block needs `onRunTty` (opens a real terminal over its own
  // WebSocket), not `onRun` (the plain captured/streamed-output path) —
  // the server rejects a `tty` block run over `/api/run` outright (see
  // `crates/server/src/lib.rs`'s `run_block`). Mirrors `RunnableCodeBlock`'s
  // own `seg.tty ? onRunTty : onRun` branch just above, which this button
  // had drifted out of sync with (it always called `onRun`, unconditionally,
  // until this).
  const quickRunIsTty = quickRunBlock?.tty ?? false;
  const quickRunAutoclose = quickRunBlock?.autoclose ?? false;
  const quickRunLive = quickRunBlockName ? data.liveBlocks[quickRunBlockName] : undefined;
  const quickRunBusy = quickRunLive?.status === "queued" || quickRunLive?.status === "running";
  const canOpenFile = data.nodeType === "file" && !!data.target;
  const constraintStatus = aggregateConstraintStatus(data.constraintResults);
  // `data.liveBlocks` is already scoped to this node's own blocks (App.tsx
  // keys each node's map by that node alone), so this covers both a `text`
  // node's fenced blocks (keyed by block name) and a runnable `file` node's
  // own run (keyed under its own id) without needing to special-case either.
  const nodeRunning = Object.values(data.liveBlocks).some((lb) => lb.status === "running");
  const nodeFailed = Object.values(data.liveBlocks).some(
    (lb) => lb.status === "killed" || (lb.status === "done" && lb.exitCode !== 0),
  );
  // Clicking the title text toggles fold both ways in read-only mode
  // (alongside `FoldToggle` itself), but the title text is also meant to
  // stay selectable (e.g. to copy it) — a plain `onClick` alone can't
  // tell "clicked" from "dragged to select text then released over the
  // same span", since the browser's `click` event fires either way. So
  // this tracks the `mousedown` position (`titleMouseDownRef`) and only
  // toggles if `mouseup` landed within `TITLE_CLICK_MOVE_THRESHOLD`px of
  // it *and* there's no active text selection left behind — a genuine
  // click, not a selection drag. No `data.hasChildren` gate here, same
  // as before this toggled both ways: a childless `isTitleOnly` node's
  // own row looks identical folded or not (no `FoldToggle` shown for it
  // either — see that render condition below), but this stays its only
  // way back to unfolded for the rare case a document explicitly authors
  // `fold="true"` on one anyway (`App.tsx`'s `resolveDefaultFold` honors
  // that override regardless of `canFold`).
  //
  // In Edit mode this is disabled outright — `FoldToggle`'s own button
  // is the only way to fold there. The title in Edit mode is also the
  // node's drag handle (its own click/drag ambiguity above only guards
  // against a *text-selection* drag, not a node-repositioning one), so a
  // plain click there needs to stay a no-op: a user clicking-and-slightly-
  // releasing while dragging the node into place shouldn't also
  // accidentally fold it out from under them.
  const titleMouseDownRef = useRef<{ x: number; y: number } | null>(null);
  const handleTitleMouseDown = (e: React.MouseEvent) => {
    titleMouseDownRef.current = { x: e.clientX, y: e.clientY };
  };
  const handleTitleClick = (e: React.MouseEvent) => {
    if (data.editMode) return;
    const start = titleMouseDownRef.current;
    const moved = start ? Math.hypot(e.clientX - start.x, e.clientY - start.y) : 0;
    if (moved > TITLE_CLICK_MOVE_THRESHOLD) return;
    if (window.getSelection()?.toString()) return;
    data.onToggleFold();
  };

  // Rendered inline in the title row, right after the title text, in both
  // layouts below — not as a separate row underneath it (TODO.canvas.md:
  // "Тэги в заголовке"). A folded node's box height is a fixed constant
  // (`FOLDED_HEIGHT` in autolayout.ts) that assumes the title row is the
  // *whole* box; a below-title tags row rendered regardless of fold state
  // (there was nothing gating it on `data.folded`, unlike the body) added
  // real height autolayout never accounted for, so a folded, tagged node's
  // actual rendered box silently grew past its allotted slot and
  // overlapped whatever autolayout placed right after it. Living inside
  // the title row instead means a folded node's real height still matches
  // `FOLDED_HEIGHT` exactly, tagged or not.
  const nodeTags = data.tags && data.tags.length > 0 && (
    <span className="mesh-node-tags">
      {data.tags.map((t) => (
        <span className="mesh-tag-chip" key={t}>
          {t}
        </span>
      ))}
    </span>
  );

  return (
    <div
      ref={nodeRootRef}
      className="mesh-node"
      data-type={data.nodeType}
      data-suggested={data.suggested}
      data-folded={data.folded}
      data-focused={data.focused ?? false}
      style={{
        ...(nodeColor ? { borderColor: nodeColor, boxShadow: `inset 4px 0 0 ${nodeColor}` } : undefined),
        ...(data.maxHeight !== undefined ? { maxHeight: data.maxHeight } : undefined),
      }}
    >
      {data.nodeType !== "group" && (
        <NodeResizer
          minWidth={220}
          minHeight={120}
          color="var(--accent)"
          isVisible={data.editMode && selected}
        />
      )}
      {/* Explicit `id`s on every handle, including these original two —
       * once a node has more than one handle of a given type (see the
       * routing-only ones below), React Flow no longer treats an edge's
       * unset `sourceHandle`/`targetHandle` as "the one with no id": it
       * just grabs the first handle of that type in render order,
       * whichever one that happens to be (`getHandle` in
       * `@xyflow/system`). Leaving these two id-less the first time this
       * file grew extra handles silently reassigned every plain
       * parent→child edge's source point to one of the new ones instead —
       * everything below (App.tsx's edge-building effect included) now
       * always passes an explicit id for exactly this reason. */}
      <Handle type="target" id="target-default" position={Position.Left} />
      {/* A `meshfox:edge` extra edge can connect any two nodes anywhere on
       * the canvas, including ones stacked mostly vertically rather than
       * side by side — routed through the plain Left/Right pair above,
       * that case's bezier tangents (which always point horizontally)
       * bow the curve out sideways regardless, often straight through
       * whatever node happens to sit in between. App.tsx's edge-building
       * effect picks these top/bottom handles instead whenever a pair of
       * endpoints is more vertically than horizontally separated, giving
       * the curve a vertical tangent that runs past a between-node's side
       * rather than through its middle. Invisible (`mesh-handle-routing`,
       * see index.css) — an alternate attachment point for that routing
       * decision, not a new connect affordance alongside the visible
       * Left/Right ones. */}
      <Handle type="target" id="target-top" position={Position.Top} className="mesh-handle-routing" />
      <Handle type="target" id="target-bottom" position={Position.Bottom} className="mesh-handle-routing" />
      <Handle type="source" id="source-top" position={Position.Top} className="mesh-handle-routing" />
      <Handle type="source" id="source-bottom" position={Position.Bottom} className="mesh-handle-routing" />
      {isTitleOnly ? (
        <div className="mesh-node-title mesh-node-title-centered nopan" data-level={data.level}>
          {/* `FoldToggle` only if there's a subtree to fold: an
           * empty-bodied text node's own row is this exact compact
           * single-line layout whether folded or not (its body was never
           * shown to begin with), so folding one with no children would be
           * a total no-op with a button to show for it — but one *with*
           * children still has real subtree content the toggle can hide,
           * even though this row itself won't visibly change. See
           * `MeshNodeData.hasChildren`. */}
          {data.hasChildren && <FoldToggle folded={data.folded} onToggle={data.onToggleFold} />}
          {/* The wrapper span below is still always worth keeping: plain,
           * no styling of its own (inherits `nopan`/`user-select: text`/
           * centering/wrapping straight from `.mesh-node-title-centered`
           * above), it's just a stable, icon-free target for a click/drag
           * meant for the title text — same idea as the normal (non-
           * title-only) layout's `.mesh-node-title-text` just below, kept
           * as a distinct class since this one must stay `white-space:
           * normal` (wrap, no ellipsis) for this layout's own "long title
           * wraps across a centered block" behavior. */}
          <span
            className="mesh-node-title-centered-text"
            onMouseDown={handleTitleMouseDown}
            onClick={handleTitleClick}
          >
            <TypeIcon type={data.nodeType} />
            {data.title}
          </span>
          {nodeTags}
          {nodeRunning && <RunningSpinner />}
          {!nodeRunning && nodeFailed && <FailedBadge />}
        </div>
      ) : (
        <div className="mesh-node-title" data-level={data.level}>
          <FoldToggle folded={data.folded} onToggle={data.onToggleFold} />
          <span
            className="mesh-node-title-text nopan"
            onMouseDown={handleTitleMouseDown}
            onClick={handleTitleClick}
          >
            <TypeIcon type={data.nodeType} />
            {data.title}
          </span>
          {nodeTags}
          {nodeRunning && <RunningSpinner />}
          {!nodeRunning && nodeFailed && <FailedBadge />}
          <ConstraintBadge status={constraintStatus} />
          {quickRunBlockName && (
            <button
              type="button"
              className="mesh-node-icon-button mesh-node-quick-run-icon nodrag"
              disabled={quickRunBusy}
              onClick={() =>
                quickRunIsTty
                  ? data.onRunTty(quickRunBlockName, true, quickRunAutoclose)
                  : data.onRun(quickRunBlockName, true)
              }
              title={
                quickRunBusy
                  ? "running…"
                  : quickRunIsTty
                    ? `runs its dependency chain first, then opens an interactive terminal for ${quickRunBlockName}`
                    : `⛓ runs its dependency chain first, then ${quickRunBlockName}`
              }
            >
              ▷
            </button>
          )}
          {canOpenFile && (
            <button
              type="button"
              className="mesh-node-icon-button nodrag"
              onClick={data.onOpenFile}
              title="Open this file in the default application"
            >
              ↗
            </button>
          )}
          {data.editMode && (
            <span className="mesh-node-title-actions">
              {isTextNode &&
                (data.plainMarkdownInclude ? (
                  <button
                    type="button"
                    className="mesh-node-icon-button nodrag"
                    onClick={() => data.onOpenSourceMode(id)}
                    title="This content comes from an included file — edit it in Source mode"
                  >
                    ⇥
                  </button>
                ) : (
                  <button
                    type="button"
                    className="mesh-node-icon-button nodrag"
                    onClick={() => setEditingText(true)}
                    title="Edit this node's Markdown text"
                  >
                    ✏
                  </button>
                ))}
              {isGroup && (
                <button
                  type="button"
                  className="mesh-node-icon-button nodrag"
                  onClick={data.onAddChild}
                  title="Add a child node to this group"
                >
                  +
                </button>
              )}
              {data.onMoveUp && (
                <button
                  type="button"
                  className="mesh-node-icon-button mesh-node-move-up nodrag"
                  onClick={data.onMoveUp}
                  title="Move this node before its previous sibling"
                >
                  ↑
                </button>
              )}
              {data.onMoveDown && (
                <button
                  type="button"
                  className="mesh-node-icon-button mesh-node-move-down nodrag"
                  onClick={data.onMoveDown}
                  title="Move this node after its next sibling"
                >
                  ↓
                </button>
              )}
              {data.onClearLayout && (
                <button
                  type="button"
                  className="mesh-node-icon-button mesh-node-clear-layout nodrag"
                  onClick={data.onClearLayout}
                  title="Reset to auto-layout (clears this node's saved position/size)"
                >
                  ↺
                </button>
              )}
              <button
                type="button"
                className="mesh-node-icon-button nodrag"
                onClick={data.onOpenSettings}
                title="Edit node settings (title, type, color, tags, target, edges)"
              >
                ⚙
              </button>
              {data.canDelete && (
                <button
                  type="button"
                  className="mesh-node-icon-button mesh-node-delete-icon nodrag"
                  onClick={data.onRequestDelete}
                  title="Delete this node"
                >
                  🗑
                </button>
              )}
            </span>
          )}
          {/* Always the rightmost icon, editMode's own action group
           * included — a node's icon set varies (run/open only show up
           * for some node types, editMode adds a whole extra group), and
           * keeping expand pinned last means its position doesn't shift
           * depending on which of the others happen to be present. For a
           * group, this opens a mini sub-canvas of its own members instead
           * of a body panel (a group's own body is always empty) — see
           * NodeExpandPanel.tsx. */}
          <button
            type="button"
            className="mesh-node-icon-button mesh-node-expand-icon nodrag"
            onClick={data.onExpand}
            title={isGroup ? "Open this group's members in their own view" : "Expand this node into a floating window"}
          >
            ⛶
          </button>
        </div>
      )}
      {/* A node's counterpart to the settings gear, but kept as a floating
       * circle just outside the node's own right edge (rather than inline
       * in the title bar, which is reserved for groups — see above) so it
       * reads as "create something new to the right" rather than a node
       * property. Rendered via `NodeToolbar` rather than a plain
       * absolutely-positioned sibling: a plain sibling only wins z-order
       * *within this node's own stacking context*, so a tightly-packed
       * neighbor (common inside a group) could sit visually on top of it
       * and silently swallow the click. `NodeToolbar` portals into React
       * Flow's shared top-level layer instead, painted above every node
       * regardless of DOM order, so the button stays clickable no matter
       * what's nearby. */}
      {!isGroup && (
        <NodeToolbar
          nodeId={id}
          isVisible={data.editMode}
          position={Position.Right}
          align="center"
          offset={7}
        >
          <button
            type="button"
            className="mesh-node-add-child nodrag"
            onClick={data.onAddChild}
            title="Add a child node to the right"
          >
            +
          </button>
        </NodeToolbar>
      )}
      {isTitleOnly || data.folded ? null : <NodeBodyContent data={data} nodeId={id} />}
      {editingText && (
        <NodeTextEditor
          initialText={data.text}
          onChange={(text) => data.onSaveText(text)}
          onClose={() => setEditingText(false)}
        />
      )}
      {/* The root's children sit almost directly below it (just a small
       * nudge right — see layout.rs), so a source handle on the right
       * would force the connector into an ugly backward loop to reach
       * them. Exiting from the left instead gives a near-straight path
       * down. Every other level indents its children a full step to the
       * right, where exiting right is the natural direction. */}
      <Handle
        type="source"
        id="source-default"
        position={data.level === 1 ? Position.Left : Position.Right}
      />
    </div>
  );
}
