import { Fragment, useCallback, useEffect, useState } from "react";
import { Handle, NodeResizer, NodeToolbar, Position, useReactFlow, type NodeProps } from "@xyflow/react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import CodeMirror from "@uiw/react-codemirror";
import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import type { Extension } from "@codemirror/state";
import { parseBody, type BodySegment, type CodeSegment } from "./fence";
import { parseBlockRef, blockDomId } from "./deps";
import { AnsiText } from "./AnsiText";
import { NodeTextEditor, usePrefersDark } from "./NodeTextEditor";
import { fetchNodeFileContent } from "./api";
import type { ConstraintStatusDto, NodeType } from "./types";

/**
 * Shared by every `<ReactMarkdown>` in this file: a link in a node's
 * rendered body opens in a new tab rather than navigating the canvas
 * itself away from the app — `rel="noopener noreferrer"` is the standard
 * `target="_blank"` companion (the opened page otherwise gets a live
 * `window.opener` back to this one). `node` is react-markdown's own mdast
 * node for this element, not a real DOM attribute — destructured out so it
 * never reaches the native `<a>`.
 */
const markdownComponents: Components = {
  a: ({ node: _node, ...props }) => <a {...props} target="_blank" rel="noopener noreferrer" />,
};

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
  status: "queued" | "running" | "done" | "killed";
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
  /** Set once `status` becomes `"running"` — what `onKill` needs to
   * identify *this* run to the server. Cleared once the block is no
   * longer the actively-streaming step. */
  runId?: string;
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
  /** Mirrors the toolbar's "show deps" toggle (see App.tsx) — same flag
   * that gates the cross-node dependency arrows also gates this node's
   * in-body rail (dots + connecting line for same-node `deps=` blocks),
   * so both dependency visualizations turn on/off together. */
  showDeps: boolean;
  target?: string;
  /** constraint-node only — its most recently evaluated pass/fail result
   * (see `ConstraintStatusDto`). Absent means the server hasn't evaluated
   * it (never the case for a loaded canvas, since `GET /api/canvas`
   * always does), not "passed" — the badge below only renders once this
   * is actually present. */
  constraintStatus?: ConstraintStatusDto;
  /** file-node display mode — `"code"` shows a read-only, syntax-highlighted
   * preview of the target file's own content instead of a plain link. */
  display?: "link" | "code";
  /** file-node syntax-highlighting language hint for `display: "code"`;
   * absent means auto-detect from the target's file extension. */
  lang?: string;
  text: string;
  /** JSON Canvas color — either a hex string or a preset `"1"`-`"6"` (see
   * `resolveNodeColor`) — `undefined`/empty means no color was set. */
  color?: string;
  /** Free-form labels, shown as small chips under the title — purely
   * descriptive, no structural meaning. */
  tags?: string[];
  /** `withDeps` picks which button was clicked: the plain "run" (false,
   * skips `deps=`) or "⛓ run chain" (true, runs the chain first). */
  onRun: (blockName: string, withDeps: boolean) => void;
  onKill: (blockName: string) => void;
  /** `tty` blocks only — opens a `TtyPanel` (a real interactive terminal)
   * instead of streaming captured output the way `onRun` does. Same
   * `withDeps` meaning: plain "run" (false) vs "⛓ run chain" (true). */
  onRunTty: (blockName: string, withDeps: boolean) => void;
  /** constraint-node only — its Starlark isn't a `run`nable fence (only
   * bash/sh are), so its body gets this instead of a Run button: re-fetches
   * the whole canvas, which re-evaluates every constraint server-side (see
   * App.tsx's `load`) and refreshes this node's badge/messages. */
  onRecheckConstraint: () => void;
  /** Creates a new child node under this one — the "+" button (inline in
   * the title bar for a group, floating at the right edge otherwise). */
  onAddChild: () => void;
  /** Opens this node's settings modal (title/type/color/target/edges). */
  onOpenSettings: () => void;
  /** Opens this node's body in a floating window (`NodeExpandPanel`) — same
   * live content (run/kill buttons, streaming output) as the inline box,
   * just bigger and not at the mercy of the canvas's current pan/zoom.
   * Available read-only, unlike the edit-mode-only actions above. */
  onExpand: () => void;
  /** Persists a full replacement of this node's raw Markdown body — the
   * inline text editor's auto-save. */
  onSaveText: (text: string) => void;
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

/** Names of code blocks that are either the source or the target of a
 * same-node `deps=` edge (a raw entry with no `node-id/` prefix) — used to
 * pick which rail dots get the "in a chain" treatment. Cross-node deps
 * aren't representable as a line inside this node (the other end isn't
 * rendered here), so they don't count. */
function sameNodeChainNames(segments: BodySegment[]): Set<string> {
  const codeSegs = segments.filter((s): s is CodeSegment => s.type === "code");
  const names = new Set(codeSegs.map((s) => s.name));
  const chain = new Set<string>();
  for (const seg of codeSegs) {
    for (const raw of seg.deps) {
      if (raw.includes("/") || !names.has(raw)) continue;
      chain.add(seg.name);
      chain.add(raw);
    }
  }
  return chain;
}

const TYPE_ICON: Partial<Record<NodeType, string>> = {
  file: "📎",
  link: "🔗",
  constraint: "🛡️",
};

/** Small pass/fail pill for a `constraint` node's title bar — green check
 * when its script raised no violations, red cross (with every `fail(msg)`
 * as a tooltip) otherwise. Renders nothing until the server has actually
 * evaluated it (see `MeshNodeData.constraintStatus`'s doc comment). */
function ConstraintBadge({ status }: { status: ConstraintStatusDto | undefined }) {
  if (!status) return null;
  return (
    <span
      className={
        status.ok
          ? "mesh-node-constraint-badge mesh-node-constraint-badge-ok"
          : "mesh-node-constraint-badge mesh-node-constraint-badge-fail"
      }
      title={status.ok ? "Constraint passes" : status.messages.join("\n")}
    >
      {status.ok ? "✓" : "✗"}
    </span>
  );
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

function RunnableCodeBlock({ seg, data, nodeId }: { seg: CodeSegment; data: MeshNodeData; nodeId: string }) {
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
  const runHandler = seg.tty ? () => data.onRunTty(seg.name, false) : () => data.onRun(seg.name, false);
  const chainHandler = seg.tty ? () => data.onRunTty(seg.name, true) : () => data.onRun(seg.name, true);

  return (
    <div className="mesh-code-block" id={blockDomId({ nodeId, blockName: seg.name })}>
      <div className="mesh-code-block-head">
        <span className="mesh-code-lang">{seg.lang}</span>
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
      <pre>
        <code>{seg.code}</code>
      </pre>
      {!seg.tty && <RunOutput seg={seg} live={live} />}
    </div>
  );
}

/** Live output (queued/running/done/killed, from the current run) takes
 * over from the cached copy in the file whenever one's in progress or just
 * finished — cleared back to the cached view once `App.tsx` reloads the
 * canvas after a persisted run, or never shown at all for a read-only run
 * beyond this component's own lifetime (a page reload loses it, same as
 * the transient-output behavior this replaces always had). */
function RunOutput({ seg, live }: { seg: CodeSegment; live?: LiveBlockState }) {
  if (live && live.status !== "queued") {
    const label =
      live.status === "killed"
        ? "killed"
        : live.status === "running"
          ? "running…"
          : `output · exit ${live.exitCode}`;
    const exitState =
      live.status === "killed" ? "killed" : live.status === "running" ? "running" : live.exitCode === 0 ? "ok" : "fail";
    return (
      <div className="mesh-code-output" data-exit={exitState}>
        <div className="mesh-code-output-head">
          {label}
          <span className="mesh-code-output-transient"> · not saved</span>
        </div>
        {live.text && (
          <pre>
            <code><AnsiText text={live.text} /></code>
          </pre>
        )}
      </div>
    );
  }
  if (!live && seg.output) {
    return (
      <div className="mesh-code-output" data-exit={seg.output.exitCode === 0 ? "ok" : "fail"}>
        <div className="mesh-code-output-head">output · exit {seg.output.exitCode}</div>
        {seg.output.text && (
          <pre>
            <code><AnsiText text={seg.output.text} /></code>
          </pre>
        )}
      </div>
    );
  }
  return null;
}

/** `lang` (if it names a known language) wins; otherwise falls back to
 * guessing from `target`'s file extension. Returns `null` when neither
 * resolves to anything CodeMirror knows how to highlight — the caller then
 * just renders plain, unhighlighted text rather than erroring. */
function pickLanguage(lang: string | undefined, target: string | undefined): LanguageDescription | null {
  const byName = lang ? LanguageDescription.matchLanguageName(languages, lang, true) : null;
  if (byName) return byName;
  return target ? LanguageDescription.matchFilename(languages, target) : null;
}

/**
 * A `file` node's `display: "code"` body (see SPEC.md): fetches the
 * target's own content fresh from the server on every mount
 * (`fetchNodeFileContent`, confined server-side to the canvas's own
 * directory) and renders it as a read-only, syntax-highlighted CodeMirror
 * view — never runnable, unlike a node's own fenced code blocks. `lang`
 * picks the highlighting grammar when set; otherwise it's guessed from the
 * target's file extension. Falls back to the plain link view on any error
 * (missing file, binary content, a target outside the canvas directory) —
 * a node whose file preview can't load should still show *something*
 * useful, not go blank.
 */
function FileCodePreview({ nodeId, target, lang }: { nodeId: string; target?: string; lang?: string }) {
  const dark = usePrefersDark();
  const wheelRef = useStopWheelIfScrollable<HTMLDivElement>();
  const [state, setState] = useState<
    | { status: "loading" }
    | { status: "error"; message: string }
    | { status: "ready"; content: string; truncated: boolean; extensions: Extension[] }
  >({ status: "loading" });

  useEffect(() => {
    let cancelled = false;
    setState({ status: "loading" });
    fetchNodeFileContent(nodeId)
      .then(async ({ content, truncated }) => {
        const desc = pickLanguage(lang, target);
        const support = desc ? await desc.load().catch(() => null) : null;
        if (cancelled) return;
        setState({ status: "ready", content, truncated, extensions: support ? [support] : [] });
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
    <div className="mesh-file-code-preview nodrag nopan" ref={wheelRef}>
      <CodeMirror
        value={state.content}
        theme={dark ? "dark" : "light"}
        extensions={state.extensions}
        editable={false}
        basicSetup={{ highlightActiveLine: false, foldGutter: false }}
      />
      {state.truncated && <p className="mesh-node-hint">preview truncated to the first part of the file</p>}
    </div>
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
      {segments.map((seg, i) =>
        seg.type === "markdown" ? (
          <ReactMarkdown key={i} remarkPlugins={[remarkGfm]} components={markdownComponents}>
            {seg.content}
          </ReactMarkdown>
        ) : (
          <div className="mesh-code-block" key={`${seg.name}-${i}`}>
            <div className="mesh-code-block-head">
              <span className="mesh-code-lang">{seg.lang}</span>
            </div>
            <pre>
              <code>{seg.code}</code>
            </pre>
          </div>
        ),
      )}
    </div>
  );
}

/**
 * Renders a text node's body. With the toolbar's "show deps" toggle off,
 * this is just the plain stacked list of markdown/code segments (matches a
 * canvas with no `deps=` blocks exactly as before the rail existed).
 * Toggled on, it switches to a two-column CSS grid: a narrow left "rail"
 * with a dot per code block (plus a connecting line spanning from the
 * first to the last one), and the actual markdown/code content on the
 * right. Every segment — markdown included — gets an explicit `gridRow`
 * matching its position in document order, so the rail's dots line up with
 * their code block without any JS measurement: the grid's own auto row
 * sizing keeps both columns in lockstep as content (e.g. streaming output)
 * changes height. The line is purely a "these run in sequence" visual
 * thread, not a precise per-edge dependency graph — dots on blocks that
 * are part of an actual same-node `deps=` edge get the accent color (see
 * `sameNodeChainNames`); the exact edges are still spelled out in each
 * block's own "after: …" links.
 */

/** Pulls the Starlark source out of a `constraint` node's body — always
 * exactly one ` ```starlark ` fence and nothing else (`mdcanvas` rejects
 * any other body shape for this type, see SPEC.md's "Constraint nodes"),
 * so this never needs to handle anything else gracefully. Falls back to
 * the raw text on the off chance a not-yet-reloaded node's body doesn't
 * match (e.g. mid-edit), rather than rendering nothing. */
function constraintScript(text: string): string {
  const match = /^```starlark\n([\s\S]*?)\n?```$/.exec(text.trim());
  return match ? match[1] : text.trim();
}

/** A `constraint` node's body: its Starlark source, read-only, with a
 * "recheck" button in place of the Run button a runnable fence would get
 * (Starlark isn't a `run`nable language — see `MeshNodeData.onRun`'s
 * counterpart, `onRecheckConstraint`) — and, once evaluated and failing,
 * every `fail(msg)` the script raised, right under the code (the same
 * information the title-bar badge's tooltip has, but readable without
 * hovering, and not truncated to one line).
 */
function ConstraintBody({ data }: { data: MeshNodeData }) {
  const status = data.constraintStatus;
  const wheelRef = useStopWheelIfScrollable<HTMLDivElement>();
  return (
    <div className="mesh-node-body nopan" ref={wheelRef}>
      <div className="mesh-code-block">
        <div className="mesh-code-block-head">
          <span className="mesh-code-lang">starlark</span>
          <button onClick={data.onRecheckConstraint} title="Re-fetch the canvas and re-evaluate every constraint">
            ↻ recheck
          </button>
        </div>
        <pre>
          <code>{constraintScript(data.text)}</code>
        </pre>
        {status && !status.ok && (
          <div className="mesh-code-output" data-exit="fail">
            <div className="mesh-code-output-head">{status.messages.length} failing</div>
            <pre>
              <code>{status.messages.join("\n")}</code>
            </pre>
          </div>
        )}
      </div>
    </div>
  );
}

function MeshNodeBody({ data, nodeId }: { data: MeshNodeData; nodeId: string }) {
  const segments = parseBody(data.text, nodeId);
  const wheelRef = useStopWheelIfScrollable<HTMLDivElement>();

  if (!data.showDeps) {
    return (
      <div className="mesh-node-body nopan" ref={wheelRef}>
        {segments.map((seg, i) =>
          seg.type === "markdown" ? (
            <ReactMarkdown key={i} remarkPlugins={[remarkGfm]} components={markdownComponents}>
              {seg.content}
            </ReactMarkdown>
          ) : (
            <RunnableCodeBlock key={seg.name} seg={seg} data={data} nodeId={nodeId} />
          ),
        )}
      </div>
    );
  }

  const chainNames = sameNodeChainNames(segments);
  const codeRows = segments.map((seg, i) => (seg.type === "code" ? i : -1)).filter((i) => i !== -1);

  return (
    <div className="mesh-node-body mesh-rail nopan" ref={wheelRef}>
      {codeRows.length > 1 && (
        <div
          className="mesh-rail-line"
          style={{ gridRow: `${codeRows[0] + 1} / ${codeRows[codeRows.length - 1] + 2}` }}
        />
      )}
      {segments.map((seg, i) =>
        seg.type === "markdown" ? (
          <div key={`md-${i}`} className="mesh-rail-content" style={{ gridRow: i + 1, gridColumn: 2 }}>
            <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownComponents}>{seg.content}</ReactMarkdown>
          </div>
        ) : (
          <Fragment key={seg.name}>
            <span
              className="mesh-rail-dot"
              data-in-chain={chainNames.has(seg.name)}
              style={{ gridRow: i + 1, gridColumn: 1 }}
              title={seg.deps.length ? `after: ${seg.deps.join(", ")}` : seg.name}
            />
            <div className="mesh-rail-content" style={{ gridRow: i + 1, gridColumn: 2 }}>
              <RunnableCodeBlock seg={seg} data={data} nodeId={nodeId} />
            </div>
          </Fragment>
        ),
      )}
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
  if (data.nodeType === "file" && data.display === "code") {
    return <FileCodePreview nodeId={nodeId} target={data.target} lang={data.lang} />;
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
      </div>
    );
  }
  if (data.nodeType === "constraint") return <ConstraintBody data={data} />;
  return <MeshNodeBody data={data} nodeId={nodeId} />;
}

export function MeshNode({ id, data, selected }: NodeProps & { data: MeshNodeData }) {
  const [editingText, setEditingText] = useState(false);
  const isTextNode = data.nodeType === "text";
  const isGroup = data.nodeType === "group";
  const nodeColor = resolveNodeColor(data.color);
  // A heading-only node (no body Markdown at all) has nothing to show in
  // its body area — while read-only, that's just dead space under a
  // left-aligned title, so it gets a distinct, centered-title layout
  // instead. Edit mode keeps the normal title bar regardless (still needs
  // its own row for the edit/settings/delete buttons, and staying put
  // avoids the layout jumping under the cursor mid-edit the moment the
  // first character is typed).
  const isTitleOnly = !data.editMode && isTextNode && data.text.trim() === "";

  return (
    <div
      className="mesh-node"
      data-type={data.nodeType}
      data-suggested={data.suggested}
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
      <Handle type="target" position={Position.Left} />
      {isTitleOnly ? (
        <div className="mesh-node-title mesh-node-title-centered nopan" data-level={data.level}>
          {TYPE_ICON[data.nodeType] ? `${TYPE_ICON[data.nodeType]} ` : ""}
          {data.title}
        </div>
      ) : (
        <div className="mesh-node-title" data-level={data.level}>
          <span className="mesh-node-title-text nopan">
            {TYPE_ICON[data.nodeType] ? `${TYPE_ICON[data.nodeType]} ` : ""}
            {data.title}
          </span>
          {data.nodeType === "constraint" && <ConstraintBadge status={data.constraintStatus} />}
          {!isGroup && (
            <button
              type="button"
              className="mesh-node-icon-button mesh-node-expand-icon nodrag"
              onClick={data.onExpand}
              title="Expand this node into a floating window"
            >
              ⛶
            </button>
          )}
          {data.editMode && (
            <span className="mesh-node-title-actions">
              {isTextNode && (
                <button
                  type="button"
                  className="mesh-node-icon-button nodrag"
                  onClick={() => setEditingText(true)}
                  title="Edit this node's Markdown text"
                >
                  ✏️
                </button>
              )}
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
        </div>
      )}
      {data.tags && data.tags.length > 0 && (
        <div className="mesh-node-tags">
          {data.tags.map((t) => (
            <span className="mesh-tag-chip" key={t}>
              {t}
            </span>
          ))}
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
      {isTitleOnly ? null : <NodeBodyContent data={data} nodeId={id} />}
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
        position={data.level === 1 ? Position.Left : Position.Right}
      />
    </div>
  );
}
