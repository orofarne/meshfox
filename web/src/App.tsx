import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import {
  ReactFlow,
  Background,
  Controls,
  MiniMap,
  MarkerType,
  useNodesState,
  useEdgesState,
  type Node,
  type Edge,
  type Connection,
  type ReactFlowInstance,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import {
  fetchCanvas,
  saveCanvas,
  runBlockStream,
  runFileStream,
  openNodeFile,
  killRun,
  fetchVars,
  fetchConfigureVars,
  saveConfigureVars,
  updateOptions,
  createNode,
  updateNode,
  deleteNode,
  reparentNode,
  renameNodeId,
  clearNodeId,
  moveSibling,
  clearNodeLayout,
  watchChanges,
  clearLayout,
  type RunEvent,
  type NodePatch,
} from "./api";
import type { CanvasDoc, CanvasNode, ExtraEdgeDto, VarStatus } from "./types";
import { pathTo, deriveEdges, findRoot, visibleNodeIds, subtreeIds } from "./tree";
import { computeAutoLayout, FOLDED_HEIGHT, type LayoutBox } from "./autolayout";
import { buildBlockGraph, resolveChain, type BlockAddr } from "./deps";
import { parseBody, type CodeSegment } from "./fence";
import { MeshNode, resolveNodeColor, type MeshNodeData, type LiveBlockState } from "./MeshNode";
import { VarsForm } from "./VarsForm";
import { DocumentOptions } from "./DocumentOptions";
import { TtyPanel } from "./TtyPanel";
import { NodeExpandPanel } from "./NodeExpandPanel";
import { NodeSettings } from "./NodeSettings";
import { DeleteNodeDialog } from "./DeleteNodeDialog";
import { AutoLayoutConfirmDialog } from "./AutoLayoutConfirmDialog";
import { ReparentChoiceDialog } from "./ReparentChoiceDialog";
import { DeletableEdge } from "./DeletableEdge";
import { CanvasSourceEditor } from "./CanvasSourceEditor";
import { getThemePreference, setThemePreference, type ThemePreference } from "./theme";

const nodeTypes = { mesh: MeshNode };
// "extra" is a real `meshfox:edge`; "tree" is the structural (nesting)
// edge, only ever registered under this type when it's actually deletable
// (its target has another incoming edge to fall back on — see the
// canvas-load effect below). Both render via the same `DeletableEdge`.
const edgeTypes = { extra: DeletableEdge, tree: DeletableEdge };

/** An extra edge's `strokeDasharray` for its declared `style` — `undefined`
 * (unset) keeps the dashed look every extra edge already had before
 * per-edge styling existed, same as an explicit `"dashed"`. */
function dashArrayFor(style: ExtraEdgeDto["style"]): string | undefined {
  if (style === "solid") return undefined;
  if (style === "dotted") return "1 3";
  return "4 4";
}
/** The zoom a freshly opened canvas starts at — fixed, not fitted to
 * content (`fitView` would otherwise shrink a large document down until
 * its text is unreadable, clamped only by the global `minZoom` floor). `1`
 * renders every node's text at exactly the size its own CSS (`.mesh-node`'s
 * `font-size`, etc.) actually specifies — the same "designed" size a
 * hand-authored `w=`/`h=` node is sized for — so this is the zoom every
 * node was implicitly sized to read well at. */
const INITIAL_ZOOM = 1;

/** How far the root node's own top-left corner sits from the canvas area's
 * top-left on first load — screen pixels, independent of zoom. Anchoring
 * the corner (see the initial-view effect below) rather than centering the
 * root, since a root normally has nothing to its left or above: centering
 * it would just waste the whole left half of the screen as empty canvas. */
const INITIAL_VIEW_PADDING_X = 80;
const INITIAL_VIEW_PADDING_Y = 80;

/** Perpendicular spacing (flow-space px, so it scales naturally with zoom
 * like everything else on the canvas) between two extra edges that share
 * the same unordered node pair — see the edge-building effect's own
 * `parallelOffsets` map, and `DeletableEdge.tsx`'s use of `data.parallelOffset`. */
const PARALLEL_EDGE_OFFSET = 24;

/** Grid step (px) that a persisted `x`/`y`/`width`/`height` is rounded to
 * (see `snapToGrid`, used by `handleSaveLayout`) — React Flow's own
 * drag/resize events carry high-precision floats
 * (e.g. `123.4578921...`), which would otherwise turn every tiny nudge into
 * noisy diff churn in the saved `.canvas.md`. Divides evenly into
 * `autolayout.ts`'s own spacing constants (`H_GAP`, `V_GAP`,
 * `ROOT_CHILD_INDENT`, `GROUP_PADDING`), so auto-placed boxes already land
 * on-grid and never visibly shift the first time they're touched and saved. */
const LAYOUT_GRID = 4;

/** Rounds `value` to the nearest `LAYOUT_GRID` step — applied only when
 * persisting a node's box (`handleSaveLayout`), never to React Flow's own
 * live position/size, so dragging itself stays exactly as smooth as before. */
function snapToGrid(value: number): number {
  return Math.round(value / LAYOUT_GRID) * LAYOUT_GRID;
}

/** Whether `addr`'s own fence opted into `cache` — i.e. whether running it
 * could have changed anything on disk worth reloading for. Used to decide
 * whether an edit-mode run needs to reload the canvas at all afterward;
 * see `executeRun`. */
function isBlockCached(canvas: CanvasDoc, addr: BlockAddr): boolean {
  const node = canvas.nodes.find((n) => n.id === addr.nodeId);
  if (!node) return false;
  const seg = parseBody(node.text, node.id).find(
    (s): s is CodeSegment => s.type === "code" && s.name === addr.blockName,
  );
  return seg?.cache ?? false;
}

/** Whether `n` renders as an already-title-only, empty-bodied row (see
 * MeshNode.tsx's own `isTitleOnly`, computed independently there since it
 * additionally accounts for `editMode` — a purely display-time concern
 * this document-level default has no business depending on). Folding a
 * node like this doesn't change its own row (there was never a body to
 * hide), but it can still have real value if the node has children of its
 * own — see `canFold`, which is what actually decides foldability. */
function isTitleOnlyNode(n: CanvasNode): boolean {
  return (n.type ?? "text") === "text" && n.text.trim() === "";
}

/** Ids that are somebody's structural `parent` — i.e. have at least one
 * child — shared by `canFold` below and the keyboard handler's own h/l/
 * Enter fold logic (App component), and by `MeshNodeData.hasChildren` (the
 * one thing `FoldToggle`'s title-only render branch itself needs to know:
 * MeshNode.tsx has no other way to tell "an empty node with children,
 * worth folding for its subtree" apart from "an empty leaf, nothing to
 * fold at all"). */
function nodesWithChildren(canvas: CanvasDoc): Set<string> {
  return new Set(canvas.nodes.map((n) => n.parent).filter((p): p is string => !!p));
}

/** Whether `n` can be folded at all. Folding a title-only node (see
 * `isTitleOnlyNode`) never changes its own row — the fold toggle there
 * only ever hides *its subtree* — so one with no children of its own has
 * nothing folding could possibly do (no row change, no subtree) and isn't
 * foldable; one with children still is, purely for that subtree's sake.
 * Every other node is always foldable (its own body, if nothing else). */
function canFold(n: CanvasNode, withChildren: ReadonlySet<string>): boolean {
  return !isTitleOnlyNode(n) || withChildren.has(n.id);
}

/** The default folded set for `canvas` on its very first open (nothing
 * saved in localStorage for it yet — see the restore effect below) — the
 * document's own declared preference, not a hardcoded rule: a node's own
 * `fold="true"`/`fold="false"` (see `CanvasNode.fold`) always wins when
 * present. Absent that: root never folds by default; a node `canFold`
 * says isn't foldable at all (a childless title-only node) never folds by
 * default either, for the same reason; a node with a real, authored
 * `width`/`height` doesn't fold by default either — an explicit size is a
 * deliberate "show this much of it" the author already made, which
 * folding it away on open would silently override; every other node
 * folds by default unless the document declares the `unfold` option (see
 * `CanvasDoc.options`, SPEC.md's "Options" section). Matches the TUI's
 * own "collapsed outline, expand what you need" default unless a document
 * opts out. */
function resolveDefaultFold(canvas: CanvasDoc, rootId: string): Set<string> {
  const hasUnfoldOption = canvas.options?.includes("unfold") ?? false;
  const withChildren = nodesWithChildren(canvas);
  const folded = new Set<string>();
  for (const n of canvas.nodes) {
    const hasExplicitSize = n.width !== undefined || n.height !== undefined;
    const resolved =
      n.fold !== undefined
        ? n.fold
        : n.id !== rootId && !hasUnfoldOption && !hasExplicitSize && canFold(n, withChildren);
    if (resolved) folded.add(n.id);
  }
  return folded;
}

/** `n`'s React Flow `position` — relative to its group when it's a direct
 * `group` child (see SPEC.md and `autolayout.ts`'s own `groupOrigin`
 * threading), absolute otherwise. A real x/y is used as-is (it's already
 * in whichever frame it needs to be, per the file format); an auto-placed
 * node instead projects `computeAutoLayout`'s absolute `box` into the
 * parent's frame by subtracting the parent's own absolute box — the one
 * place a conversion is still needed, since `computeAutoLayout`'s internal
 * map stays absolute throughout. Shared by both the canvas-load node-build
 * effect and the measured-height reflow effect below, so a group member's
 * position is computed exactly the same way in either place — the reflow
 * effect used to skip this and write `box.x`/`box.y` straight back
 * (correct for every node *except* a group member, whose `position` needs
 * to stay parent-relative), which is what let `parentId`-composition
 * double-count the group's own offset the moment a member's height was
 * first measured. */
function positionFor(
  n: CanvasNode,
  box: LayoutBox | undefined,
  byId: Map<string, CanvasNode>,
  boxes: Map<string, LayoutBox>,
): { x: number; y: number } {
  const groupParent = n.parent ? byId.get(n.parent) : undefined;
  const isGroupMember = groupParent?.type === "group";
  const parentBox = isGroupMember ? boxes.get(groupParent!.id) : undefined;
  const x =
    n.x !== undefined ? n.x : isGroupMember && box && parentBox ? box.x - parentBox.x : (box?.x ?? 0);
  const y =
    n.y !== undefined ? n.y : isGroupMember && box && parentBox ? box.y - parentBox.y : (box?.y ?? 0);
  return { x, y };
}

export default function App() {
  const [canvas, setCanvas] = useState<CanvasDoc | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A node's subtree folded to a compact title-only row — a view
  // preference (never written to the file, see `handleSaveLayout`), one
  // React state shared by the fold-toggle UI, keyboard nav's visible
  // order, and the layout pass that reflows auto-placed siblings around a
  // folded subtree (see `computeAutoLayout`'s `foldedNodeIds`). Persisted
  // to localStorage per canvas (see the restore/persist effects below) so
  // it survives a reload without polluting the document itself.
  const [foldedNodeIds, setFoldedNodeIds] = useState<Set<string>>(new Set());
  const foldedStorageKeyRef = useRef<string | null>(null);
  // Restores folded state for whichever canvas just loaded, keyed by its
  // root node's id (stable across reloads/renames per SPEC.md — the only
  // thing here that reliably identifies "this document" client-side).
  // Runs once per distinct root id, not on every reload of the *same*
  // canvas (e.g. after a save), so an in-session fold toggle isn't
  // silently reverted by the reload that follows persisting it. The very
  // first time a given canvas is opened (nothing saved for it yet), the
  // document's own declared default applies (see `resolveDefaultFold`) —
  // by default a "collapsed outline, expand what you need" view, same
  // experience the TUI opens to, unless the document opts out via the
  // `unfold` option.
  useEffect(() => {
    if (!canvas) return;
    const rootId = findRoot(canvas)?.id;
    if (!rootId) return;
    const key = `meshfox-folded:${rootId}`;
    if (foldedStorageKeyRef.current === key) return;
    foldedStorageKeyRef.current = key;
    try {
      const raw = localStorage.getItem(key);
      if (raw) {
        setFoldedNodeIds(new Set(JSON.parse(raw)));
        return;
      }
    } catch {
      // fall through to the default below
    }
    setFoldedNodeIds(resolveDefaultFold(canvas, rootId));
  }, [canvas]);
  useEffect(() => {
    const key = foldedStorageKeyRef.current;
    if (!key) return;
    localStorage.setItem(key, JSON.stringify([...foldedNodeIds]));
  }, [foldedNodeIds]);
  // True from the moment a drag/resize changes a node's position/size
  // until the debounced auto-save (below) has actually persisted it —
  // drives the toolbar's "saving layout…" indicator.
  const [dirty, setDirty] = useState(false);
  // Opening a canvas shouldn't be one click away from running its bash
  // blocks — read-only until the user explicitly opts in.
  const [editMode, setEditMode] = useState(false);
  // True while the toolbar's "Source" toggle has swapped the graph view for
  // CanvasSourceEditor's raw-Markdown editor — edit-mode-only, see the
  // effect below that clears it if editMode itself turns off.
  const [sourceMode, setSourceMode] = useState(false);
  // Whether CanvasSourceEditor has unsaved edits — disables the "done"
  // button so leaving Edit mode can't silently discard them.
  const [sourceDirty, setSourceDirty] = useState(false);
  // Which file CanvasSourceEditor should open on — an include's own
  // `nodeId` (see `IncludeManifestEntry`), or `undefined` for the
  // document itself (the toolbar's plain "Source" toggle). Set by
  // `handleOpenSourceMode` (a `plainMarkdownInclude` node's title-bar
  // button, since that content can only ever be edited via its own real
  // file — see `MeshNode.tsx`'s `plainMarkdownInclude`), consumed once by
  // `CanvasSourceEditor`'s own initial-selection prop and not touched
  // again while Source mode stays open (switching files from there on is
  // that component's own picker's job).
  const [sourceInitialInclude, setSourceInitialInclude] = useState<string | undefined>(undefined);
  // Toolbar's light/dark toggle (see theme.ts) — "system" (the default)
  // follows the OS; initialized from localStorage rather than always
  // "system" so a stored override survives a reload without a flash (see
  // main.tsx's own `applyThemePreference` call, which sets the DOM
  // attribute before this ever renders).
  const [themePreference, setThemePreferenceState] = useState<ThemePreference>(getThemePreference);
  const cycleTheme = useCallback(() => {
    setThemePreferenceState((prev) => {
      const next: ThemePreference = prev === "system" ? "light" : prev === "light" ? "dark" : "system";
      setThemePreference(next);
      return next;
    });
  }, []);
  const [nodes, setNodes, onNodesChange] = useNodesState<Node<MeshNodeData>>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  // Set via `onInit` below — needed to call `setCenter` imperatively once
  // the canvas has actually loaded (see the initial-view effect further
  // down). Predates main.tsx's app-wide `<ReactFlowProvider>` (added for
  // NodeExpandPanel's `useReactFlow()` — see its own doc comment), which
  // would make `useReactFlow()` reachable here too now; kept as `onInit`
  // regardless, since this only ever needs the instance once, on load.
  const [flowInstance, setFlowInstance] = useState<ReactFlowInstance<Node<MeshNodeData>> | null>(null);
  // Only the very first load should recenter the view — not every
  // subsequent canvas reload (after a run, a save, a live-reload push),
  // which would otherwise yank the viewport out from under whatever the
  // user is currently looking at.
  const hasSetInitialView = useRef(false);
  // Snapshot, not live-tracked: the 60%/40% viewport-relative widths
  // `autolayout.ts` computes are read once per app load, not recomputed on
  // window resize (deliberate — see autolayout.ts's module doc comment).
  const viewportWidthRef = useRef(window.innerWidth);
  // Every auto-placed node's last known *real, rendered* height (React
  // Flow's own `measured.height`), keyed by node id — persists across a
  // canvas reload (unlike `nodes` state, which the canvas-load effect below
  // fully rebuilds), so a reload doesn't visibly snap every unpositioned
  // node back to the placeholder height for a frame before re-measuring.
  // Kept in sync by the measured-height effect further down.
  const measuredHeightsRef = useRef<Map<string, number>>(new Map());
  const toggleFold = useCallback(
    (id: string) => {
      const isUnfolding = foldedNodeIds.has(id);
      setFoldedNodeIds((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      });
      if (!isUnfolding || !canvas || !flowInstance) return;
      // Unfolding can reveal a subtree wide enough that it no longer fits
      // the current viewport at the current zoom — shift the camera so
      // the just-unfolded node's own left edge lands near the viewport's
      // own left edge (same left padding the very first view uses, see
      // `INITIAL_VIEW_PADDING_X`), maximizing how much of the newly
      // revealed content to its right actually ends up on screen.
      // Computed directly via `computeAutoLayout` against the *resulting*
      // folded set, rather than read back from `nodes`/React Flow's own
      // store afterward — those only settle a render or two later, once
      // the reflow effect further down reacts to this same
      // `foldedNodeIds` change, which would mean either racing that
      // effect or reading a stale, still-folded layout.
      const nextFolded = new Set(foldedNodeIds);
      nextFolded.delete(id);
      const boxes = computeAutoLayout({
        canvas,
        viewportWidth: viewportWidthRef.current,
        measuredHeight: (nodeId) => measuredHeightsRef.current.get(nodeId),
        foldedNodeIds: nextFolded,
      });
      const nodeBox = boxes.get(id);
      if (!nodeBox) return;
      // The revealed subtree's own rightmost extent — every descendant
      // that's now actually visible has an entry in `boxes`; one still
      // hidden behind its own, separately folded subtree doesn't, and is
      // rightly left out of this.
      let maxX = nodeBox.x + nodeBox.width;
      for (const descendantId of subtreeIds(canvas, id)) {
        const box = boxes.get(descendantId);
        if (box) maxX = Math.max(maxX, box.x + box.width);
      }
      // Whether it "fits" has to be judged against where the content
      // actually sits on screen *right now*, not just whether it's
      // narrower than the viewport in the abstract — content well within
      // the viewport's own width can still be scrolled off past its
      // right edge entirely (exactly what a group tucked far out to the
      // right, itself unfolded for the first time, routinely is), and a
      // width-only check would wrongly call that "already fits".
      const viewport = flowInstance.getViewport();
      const screenLeft = nodeBox.x * viewport.zoom + viewport.x;
      const screenRight = maxX * viewport.zoom + viewport.x;
      if (screenLeft >= 0 && screenRight <= viewportWidthRef.current) return;
      flowInstance.setViewport(
        { x: INITIAL_VIEW_PADDING_X - nodeBox.x * viewport.zoom, y: viewport.y, zoom: viewport.zoom },
        { duration: 400 },
      );
    },
    [foldedNodeIds, canvas, flowInstance],
  );
  // Set only while the pre-run "configure variables" modal is open (see
  // VarsForm) — remembers which run to actually start once it's answered.
  // Independent of editMode: asking for run configuration isn't editing
  // the document, so it works the same read-only or not.
  const [varsModal, setVarsModal] = useState<{
    nodeId: string;
    blockName: string;
    withDeps: boolean;
    missing: VarStatus[];
    /** Set for a `tty` block's run — `handleVarsSubmit` opens `TtyPanel`
     * (via `ttySession`) with the answers instead of calling `executeRun`. */
    tty?: boolean;
  } | null>(null);
  // Whether the toolbar's "configure" button should even appear — set from
  // `load()`'s own `fetchConfigureVars` call, so it's known without a
  // click. `null` while unknown yet (first load in flight) is treated the
  // same as `false` below; a canvas that declares only `secret` variables
  // (or none at all) reports empty here, same as the TUI's own
  // `has_configurable_vars`.
  const [hasConfigurableVars, setHasConfigurableVars] = useState(false);
  // The open "configure every declared variable" modal (see VarsForm,
  // `handleConfigure`) — the browser counterpart to `meshfox configure`/
  // the TUI's `c` key. Independent of `varsModal`: this isn't gating a
  // run, it can be opened any time the toolbar button is visible.
  const [configureVars, setConfigureVars] = useState<VarStatus[] | null>(null);
  // Whether the toolbar's "options" modal (see DocumentOptions) is open —
  // unlike `configureVars`, needs no fetch first: `canvas.options` (from
  // `GET /api/canvas`'s own `declared_options` call) is already live in
  // `canvas` state, so the button just flips this straight to `true`.
  const [documentOptionsOpen, setDocumentOptionsOpen] = useState(false);
  // Set while a `tty` block's interactive terminal panel is open — see
  // `handleRunTty`/`TtyPanel`. Only one at a time (opening another closes
  // whatever's already open, same as `NodeTextEditor`'s single-editor
  // convention) — `TtyPanel` owns its own WebSocket for its whole
  // lifetime, so this is just the address it was opened for.
  const [ttySession, setTtySession] = useState<{
    path: string[];
    blockName: string;
    withDeps: boolean;
    vars?: Record<string, string>;
  } | null>(null);
  // Which node's settings modal (title/type/color/target/edges) is open,
  // if any — see NodeSettings.tsx. Set right after a successful "add
  // child" too, so the new node's title is immediately editable.
  const [settingsNodeId, setSettingsNodeId] = useState<string | null>(null);
  // Which node's body is expanded into a floating window (see
  // NodeExpandPanel) — available read-only, unlike `settingsNodeId`. Looked
  // up from `nodes` (the live React Flow state, not `canvas.nodes`) so the
  // panel renders the exact same `MeshNodeData` — live run state, callbacks
  // and all — the node's own inline box does.
  const [expandedNodeId, setExpandedNodeId] = useState<string | null>(null);
  // Which node's delete-confirm dialog (see DeleteNodeDialog) is open, if
  // any — same "id, not the node itself" shape as `settingsNodeId`, for the
  // same reason: re-look-up from `canvas` on every render rather than risk
  // holding a stale snapshot across an unrelated canvas reload.
  const [deleteConfirmNodeId, setDeleteConfirmNodeId] = useState<string | null>(null);
  // Which node's "choose a new parent" dialog (see ReparentChoiceDialog) is
  // open, if any — only used when deleting a node's structural parent edge
  // and it has *more than one* other incoming edge to choose from; with
  // exactly one, `requestReparentEdge` promotes it directly, no dialog.
  const [reparentPromptNodeId, setReparentPromptNodeId] = useState<string | null>(null);
  // Whether the edit-mode toolbar's "Auto-layout" confirm dialog (see
  // AutoLayoutConfirmDialog) is open — destructive (clears every node's
  // stored position/size in the file), so this is the one gate before
  // `handleAutoLayout` actually runs.
  const [autoLayoutConfirmOpen, setAutoLayoutConfirmOpen] = useState(false);

  // Best-effort client-side mirror of crates/core/src/deps.rs — used only
  // to preview a run's dependency chain (so "running" indicators can light
  // up on every block about to run, not just the clicked one). The server
  // remains the source of truth for actually resolving and executing the
  // chain.
  const blockGraph = useMemo(() => (canvas ? buildBlockGraph(canvas) : new Map()), [canvas]);

  // Every node id that's a structural parent of at least one other node —
  // shared by the fold-marker's `hasChildren` prop (a title-only node
  // needs one to have anything worth folding — see MeshNode.tsx's
  // `FoldToggle`) and keyboard nav's own h/Enter fold logic (feature 5),
  // so both agree on what "has a subtree" means.
  const parentIdSet = useMemo(
    () => (canvas ? nodesWithChildren(canvas) : new Set<string>()),
    [canvas],
  );

  const load = useCallback(async () => {
    try {
      const doc = await fetchCanvas();
      setCanvas(doc);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
    // Best-effort and separate from the try/catch above: a failure here
    // (or the document simply declaring no configurable variable) should
    // just hide the toolbar button, never surface as the page's own error.
    try {
      const vars = await fetchConfigureVars();
      setHasConfigurableVars(vars.length > 0);
    } catch {
      setHasConfigurableVars(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // True once `/api/watch`'s connection has ended for any reason — which,
  // for a purely local server with no other reason to drop a live
  // connection, means the server process itself has stopped (see
  // ./api.ts's watchChanges). Nothing here can be trusted to still work
  // once this is set, so the whole app is replaced with a plain message
  // instead (see the render below) rather than risk more failed requests.
  const [serverGone, setServerGone] = useState(false);

  // Mirrors `sourceMode` for the watch effect below, which shouldn't itself
  // re-subscribe every time Source mode toggles — only read at the moment
  // a `"changed"` event actually arrives.
  const sourceModeRef = useRef(sourceMode);
  useEffect(() => {
    sourceModeRef.current = sourceMode;
  }, [sourceMode]);

  // An external change (edited on disk, outside this tab) that arrived
  // while Source mode was open — CanvasSourceEditor keeps its own separate
  // copy of the raw text (fetched once on mount), so reloading `canvas`
  // here wouldn't even reach it; instead this just remembers to reload once
  // the user actually leaves Source mode.
  const pendingExternalChange = useRef(false);

  useEffect(() => {
    const stop = watchChanges(
      () => {
        if (sourceModeRef.current) {
          pendingExternalChange.current = true;
          return;
        }
        load();
      },
      () => setServerGone(true),
    );
    return stop;
  }, [load]);

  useEffect(() => {
    if (!sourceMode && pendingExternalChange.current) {
      pendingExternalChange.current = false;
      load();
    }
  }, [sourceMode, load]);

  // Best-effort: most browsers refuse to let a script close a tab it didn't
  // itself open via `window.open` (this one was opened by the OS's default-
  // browser handler, via the CLI's `open::that`), so this silently no-ops
  // there — the message below is what actually tells the user either way.
  useEffect(() => {
    if (serverGone) window.close();
  }, [serverGone]);

  // Leaving Edit mode (the "done" button) always leaves Source mode too —
  // there's no read-only source view, so the two can't be out of sync.
  useEffect(() => {
    if (!editMode) setSourceMode(false);
  }, [editMode]);

  useEffect(() => {
    if (!sourceMode) {
      setSourceDirty(false);
      // Whichever file a `plainMarkdownInclude` redirect (see
      // `handleOpenSourceMode`) pointed CanvasSourceEditor at shouldn't
      // linger for next time — covers every way out of Source mode
      // (the editor's own Cancel/Save, and the toolbar's own toggle,
      // which don't each need their own copy of this reset).
      setSourceInitialInclude(undefined);
    }
  }, [sourceMode]);

  // CanvasSourceEditor's Save succeeded — the file changed from underneath
  // every other bit of state here (positions, node ids, everything), so
  // reload from scratch same as any other server-driven canvas change.
  const handleSourceSaved = useCallback(async () => {
    setSourceMode(false);
    await load();
  }, [load]);

  // Patches one block's live state on its owning node, merging into
  // whatever was there (so an `output` event's text-append doesn't clobber
  // a status set by an earlier event, and vice versa).
  const patchLiveBlock = useCallback(
    (targetNodeId: string, blockName: string, patch: Partial<LiveBlockState>) => {
      setNodes((nds) =>
        nds.map((n) => {
          if (n.id !== targetNodeId) return n;
          const prev: LiveBlockState = n.data.liveBlocks[blockName] ?? { status: "queued", text: "" };
          return {
            ...n,
            data: {
              ...n.data,
              liveBlocks: { ...n.data.liveBlocks, [blockName]: { ...prev, ...patch } },
            },
          };
        }),
      );
    },
    [setNodes],
  );

  // Actually starts a run — split out from `handleRun` (below) so the
  // vars-modal's "run" button can call straight back into this once
  // answered, without re-checking `fetchVars` a second time.
  const executeRun = useCallback(
    async (nodeId: string, blockName: string, withDeps: boolean, vars?: Record<string, string>) => {
      if (!canvas) return;
      const path = pathTo(canvas, nodeId);

      // Best-effort preview of the full chain this run will trigger
      // server-side, purely so every block about to run (not just the one
      // clicked) shows a "queued" state right away instead of waiting on
      // the network round-trip — falls back to just the clicked block if
      // the graph can't be resolved locally (e.g. it references something
      // that genuinely doesn't exist; the server reports that
      // authoritatively via an `"error"` event once the request lands).
      // Skipped entirely for a no-deps run: only the clicked block itself
      // is about to run, so there's nothing else to preview.
      let previewChain: BlockAddr[];
      if (!withDeps) {
        previewChain = [{ nodeId, blockName }];
      } else {
        try {
          previewChain = resolveChain(blockGraph, { nodeId, blockName });
        } catch {
          previewChain = [{ nodeId, blockName }];
        }
      }
      const queuedByNode = new Map<string, string[]>();
      for (const addr of previewChain) {
        queuedByNode.set(addr.nodeId, [...(queuedByNode.get(addr.nodeId) ?? []), addr.blockName]);
      }
      setNodes((nds) =>
        nds.map((n) => {
          const names = queuedByNode.get(n.id);
          if (!names) return n;
          const liveBlocks = { ...n.data.liveBlocks };
          for (const name of names) {
            // Only the block whose button was actually clicked (not the
            // rest of its chain, pulled in automatically) records which
            // button that was — see LiveBlockState.viaChain in MeshNode.tsx.
            const viaChain = withDeps && n.id === nodeId && name === blockName;
            liveBlocks[name] = { status: "queued", text: "", viaChain };
          }
          return { ...n, data: { ...n.data, liveBlocks } };
        }),
      );

      // Attached to whichever block is the currently-executing step, so
      // its Kill button knows what to cancel — the same `runId` covers the
      // whole chain (the server only ever runs one step of it at a time).
      let runId: string | undefined;

      try {
        // Running is always allowed; only Edit mode persists a cache'd
        // block's output to the file. When `withDeps`, the server
        // automatically expands this into the block's full `deps=` chain
        // (see SPEC.md); otherwise only `blockName` itself runs. Streams
        // one event per line as it happens (see ./api.ts).
        await runBlockStream(path, blockName, editMode, withDeps, (event: RunEvent) => {
          switch (event.type) {
            case "started":
              runId = event.runId;
              break;
            case "step-start":
              patchLiveBlock(event.nodeId, event.block, { status: "running", text: "", exitCode: undefined, runId });
              break;
            case "output":
              setNodes((nds) =>
                nds.map((n) => {
                  if (n.id !== event.nodeId) return n;
                  const prev = n.data.liveBlocks[event.block] ?? { status: "running", text: "" };
                  const text = prev.text ? `${prev.text}\n${event.text}` : event.text;
                  return {
                    ...n,
                    data: { ...n.data, liveBlocks: { ...n.data.liveBlocks, [event.block]: { ...prev, text } } },
                  };
                }),
              );
              break;
            case "step-end":
              patchLiveBlock(event.nodeId, event.block, { status: "done", exitCode: event.exitCode, runId: undefined });
              break;
            case "killed":
              patchLiveBlock(event.nodeId, event.block, { status: "killed", runId: undefined });
              break;
            case "error":
              setError(event.message);
              break;
            case "done":
              break;
          }
        }, vars);
        // Reloading clears every node's `liveBlocks` (see the canvas-load
        // effect below) — worth it when it picks up a `cache`d block's
        // freshly-persisted output, but pure loss for a chain that has no
        // `cache`d step at all: nothing changed on disk, so there's
        // nothing to pick up, and the reload would just wipe the live
        // output this very run produced right as it finishes (it'd
        // otherwise stay visible, same as it does in read-only mode).
        if (editMode && previewChain.some((addr) => isBlockCached(canvas, addr))) {
          await load();
        }
      } catch (e) {
        setError(String(e));
      }
    },
    [canvas, editMode, load, blockGraph, setNodes, patchLiveBlock],
  );

  // Runs a runnable `file` node's `interpreter target` (see
  // `api.ts`'s `runFileStream`) — the file-node counterpart to
  // `executeRun`. A `file` node has no `deps=`/`env=`/`cache` concept, so
  // there's no vars-modal gate (`handleRun` routes straight here for one,
  // skipping `fetchVars`/`executeRun` entirely) and no reload afterward
  // (nothing on disk changes). `blockName` is always `nodeId` itself, same
  // "the block shares its node's own id" convention a `text` node's sole
  // implicit block already uses — see `RunEvent`'s `nodeId`/`block`.
  const executeFileRun = useCallback(
    async (nodeId: string) => {
      patchLiveBlock(nodeId, nodeId, { status: "queued", text: "" });
      let runId: string | undefined;
      try {
        await runFileStream(nodeId, (event: RunEvent) => {
          switch (event.type) {
            case "started":
              runId = event.runId;
              break;
            case "step-start":
              patchLiveBlock(event.nodeId, event.block, { status: "running", text: "", exitCode: undefined, runId });
              break;
            case "output":
              setNodes((nds) =>
                nds.map((n) => {
                  if (n.id !== event.nodeId) return n;
                  const prev = n.data.liveBlocks[event.block] ?? { status: "running", text: "" };
                  const text = prev.text ? `${prev.text}\n${event.text}` : event.text;
                  return {
                    ...n,
                    data: { ...n.data, liveBlocks: { ...n.data.liveBlocks, [event.block]: { ...prev, text } } },
                  };
                }),
              );
              break;
            case "step-end":
              patchLiveBlock(event.nodeId, event.block, { status: "done", exitCode: event.exitCode, runId: undefined });
              break;
            case "killed":
              patchLiveBlock(event.nodeId, event.block, { status: "killed", runId: undefined });
              break;
            case "error":
              setError(event.message);
              break;
            case "done":
              break;
          }
        });
      } catch (e) {
        setError(String(e));
      }
    },
    [patchLiveBlock, setNodes],
  );

  // Gate in front of `executeRun`: checks whether any declared
  // `meshfox:var` (see SPEC.md's "Variables") is still unresolved before
  // actually starting the run, and opens the modal to ask instead when
  // one is — the browser counterpart to the CLI's terminal prompt.
  // Fetches fresh every click rather than caching canvas-load-time
  // status, since an earlier run (this tab or another) may have just
  // resolved something. A runnable `file` node (see `CanvasNode.interpreter`)
  // routes straight to `executeFileRun` instead — it has no fenced block,
  // `deps=` chain, or `env=` to check `GET /api/vars` for.
  const handleRun = useCallback(
    async (nodeId: string, blockName: string, withDeps: boolean) => {
      if (!canvas) return;
      const node = canvas.nodes.find((n) => n.id === nodeId);
      if (node?.type === "file" && node.interpreter) {
        await executeFileRun(nodeId);
        return;
      }
      const path = pathTo(canvas, nodeId);
      let statuses: VarStatus[];
      try {
        statuses = await fetchVars(path, blockName, withDeps);
      } catch (e) {
        setError(String(e));
        return;
      }
      const missing = statuses.filter((v) => !v.resolved);
      if (missing.length > 0) {
        setVarsModal({ nodeId, blockName, withDeps, missing });
        return;
      }
      await executeRun(nodeId, blockName, withDeps);
    },
    [canvas, executeRun, executeFileRun],
  );

  // `tty` counterpart to `handleRun`: same "check `GET /api/vars` first,
  // ask via the same modal if anything's missing" gate, but the actual run
  // opens `TtyPanel` (a real terminal) instead of calling `executeRun` —
  // `varsModal.tty` is how `handleVarsSubmit` (below) tells the two apart
  // once the modal comes back.
  const handleRunTty = useCallback(
    async (nodeId: string, blockName: string, withDeps: boolean) => {
      if (!canvas) return;
      const path = pathTo(canvas, nodeId);
      let statuses: VarStatus[];
      try {
        statuses = await fetchVars(path, blockName, withDeps);
      } catch (e) {
        setError(String(e));
        return;
      }
      const missing = statuses.filter((v) => !v.resolved);
      if (missing.length > 0) {
        setVarsModal({ nodeId, blockName, withDeps, missing, tty: true });
        return;
      }
      setTtySession({ path, blockName, withDeps });
    },
    [canvas],
  );

  const handleVarsSubmit = useCallback(
    async (answers: Record<string, string>) => {
      if (!varsModal) return;
      const { nodeId, blockName, withDeps, tty } = varsModal;
      setVarsModal(null);
      if (tty) {
        if (!canvas) return;
        setTtySession({ path: pathTo(canvas, nodeId), blockName, withDeps, vars: answers });
        return;
      }
      await executeRun(nodeId, blockName, withDeps, answers);
    },
    [varsModal, executeRun, canvas],
  );

  const handleVarsCancel = useCallback(() => setVarsModal(null), []);

  // Toolbar "configure" button — the browser counterpart to `meshfox
  // configure`/the TUI's `c` key: opens a form for *every* declared
  // non-secret variable in the document (not just what one block's chain
  // still needs), pre-filled with each one's currently-resolved value.
  // Always fetches fresh right before opening, same as `handleRun` does
  // for its own gate, rather than reusing whatever `load()` last saw.
  const handleConfigure = useCallback(async () => {
    try {
      setConfigureVars(await fetchConfigureVars());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const handleConfigureSubmit = useCallback(async (answers: Record<string, string>) => {
    setConfigureVars(null);
    try {
      await saveConfigureVars(answers);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const handleConfigureCancel = useCallback(() => setConfigureVars(null), []);

  // Toolbar "options" button's submit — replaces the document's whole
  // declared `meshfox:option` set in one PUT (see `updateOptions`,
  // SPEC.md's "Options"). Doesn't touch `foldedNodeIds` itself — the
  // fold-restore effect above only ever applies `resolveDefaultFold` the
  // very first time a given root id is seen with nothing in localStorage
  // yet (see that effect's own comment), so toggling `unfold` here
  // changes what a *fresh* browser/session opens to, not this session's
  // already-resolved fold state.
  const handleDocumentOptionsSubmit = useCallback(async (options: string[]) => {
    setDocumentOptionsOpen(false);
    try {
      setCanvas(await updateOptions(options));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const handleDocumentOptionsCancel = useCallback(() => setDocumentOptionsOpen(false), []);

  // Reads current state through `setNodes`'s updater rather than closing
  // over the `nodes` variable directly: `data.onKill` is bound once (when
  // the canvas-load effect below builds each node, which deliberately
  // doesn't re-run on every `nodes` change — see that effect's comment),
  // so a plain closure over `nodes` here would always see whatever it was
  // at that one moment, never the `runId` a real run later attaches.
  // `setNodes`'s updater always receives the latest state instead,
  // regardless of when this closure was created.
  const handleKill = useCallback(
    (nodeId: string, blockName: string) => {
      setNodes((nds) => {
        const node = nds.find((n) => n.id === nodeId);
        const runId = node?.data.liveBlocks[blockName]?.runId;
        if (runId) killRun(runId).catch((e) => setError(String(e)));
        return nds; // read-only — the eventual "killed" event updates state
      });
    },
    [setNodes],
  );

  // Opens a `file` node's target in the OS's default application (the
  // title bar's "↗ open" button) — fire-and-forget from the UI's point of
  // view, just surfaced to the same error banner every other action here
  // uses if the server couldn't spawn the opener.
  const handleOpenFile = useCallback(async (nodeId: string) => {
    try {
      await openNodeFile(nodeId);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Creates a new child node under `parentId` (empty body, no position —
  // the server's layout suggestion places it to the parent's right, see
  // `mdcanvas::insert_child_node`'s doc comment) and immediately opens its
  // settings modal, pre-focused on the title field, so the freshly-created
  // "New Node" placeholder title gets renamed right away.
  const handleAddChild = useCallback(
    async (parentId: string) => {
      if (!canvas) return;
      const previousIds = new Set(canvas.nodes.map((n) => n.id));
      try {
        const updated = await createNode(parentId, "New Node");
        setCanvas(updated);
        const added = updated.nodes.find((n) => !previousIds.has(n.id));
        if (added) setSettingsNodeId(added.id);
      } catch (e) {
        setError(String(e));
      }
    },
    [canvas],
  );

  // NodeSettings' "ok" commit for every field except the id (see
  // `handleNodeIdChange` for that one) — passed straight through as its
  // `onChange` prop, `id` supplied by `NodeSettings` itself rather than a
  // closure captured here (see that prop's own doc comment for why: a
  // same-click id rename makes any id captured at render time go stale
  // before this fires). Never closes the modal itself; errors (e.g. the
  // server rejecting an invalid type/target combination) are just
  // surfaced, since the user is likely still mid-edit.
  const handleNodeSettingsChange = useCallback(async (id: string, patch: NodePatch) => {
    try {
      const updated = await updateNode(id, patch);
      setCanvas(updated);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // MeshNode's ↑/↓ sibling-reorder buttons (auto-placed nodes only — see
  // `MeshNodeData.onMoveUp`/`onMoveDown`) — same fire-and-surface-globally
  // error handling as `handleNodeSettingsChange` above, nothing local to
  // revert since the buttons themselves aren't a form.
  const handleMoveSibling = useCallback((id: string, target: { before: string } | { after: string }) => {
    moveSibling(id, target)
      .then(setCanvas)
      .catch((e) => setError(String(e)));
  }, []);

  // MeshNode's ↺ "reset to auto-layout" button (positioned nodes only —
  // see `MeshNodeData.onClearLayout`) — same fire-and-surface-globally
  // error handling as the two callbacks above.
  const handleClearNodeLayout = useCallback((id: string) => {
    clearNodeLayout(id)
      .then(setCanvas)
      .catch((e) => setError(String(e)));
  }, []);

  // NodeSettings' ID field commit — unlike `handleNodeSettingsChange`, this
  // rejects (rethrows) on failure so the field itself can show the error
  // and revert, rather than just surfacing the global error banner. On
  // success, `settingsNodeId` (and the `touchedNodeIds` bookkeeping below)
  // both need to follow the id, or they'd keep pointing at an id that no
  // longer exists in the next canvas.
  const handleNodeIdChange = useCallback(async (id: string, newId: string) => {
    const updated = await renameNodeId(id, newId);
    setCanvas(updated);
    setSettingsNodeId((cur) => (cur === id ? newId : cur));
    if (touchedNodeIds.current.has(id)) {
      touchedNodeIds.current.delete(id);
      touchedNodeIds.current.add(newId);
    }
  }, []);

  // NodeSettings' "leave the ID field empty" commit — same "id might have
  // just changed out from under this component" bookkeeping as
  // `handleNodeIdChange` above, except the resulting id isn't known up
  // front (it's whatever the server derived from the title), so it comes
  // back from `clearNodeId` itself rather than being passed in.
  const handleNodeIdClear = useCallback(async (id: string) => {
    const { id: newId, doc } = await clearNodeId(id);
    setCanvas(doc);
    setSettingsNodeId((cur) => (cur === id ? newId : cur));
    if (touchedNodeIds.current.has(id)) {
      touchedNodeIds.current.delete(id);
      touchedNodeIds.current.add(newId);
    }
    return newId;
  }, []);

  // Deletes a node — `mode` is the delete-confirm dialog's choice of what
  // happens to its direct children (see DeleteNodeDialog): `"subtree"`
  // drops them too, `"reparent"` promotes them to this node's own parent.
  // Also closes the settings modal afterward if it happened to be open on
  // the very node just deleted — sitting in a settings panel for a node
  // that no longer exists isn't useful. The delete-confirm dialog itself is
  // always closed by its own caller before this even runs (see below).
  const handleDeleteNode = useCallback(async (id: string, mode: "subtree" | "reparent") => {
    try {
      const updated = await deleteNode(id, mode);
      setCanvas(updated);
    } catch (e) {
      setError(String(e));
    }
    setSettingsNodeId((cur) => (cur === id ? null : cur));
  }, []);

  // Reverts every node to auto-placed (see `AutoLayoutConfirmDialog`, the
  // toolbar button's actual caller). Also drops `touchedNodeIds` — those ids
  // named nodes whose *previous* on-screen box was authored/dragged; keeping
  // them would make `handleSaveLayout` immediately persist each one's fresh
  // auto-computed box back into the file the moment anything next
  // autosaves, defeating the whole point of resetting.
  const handleAutoLayout = useCallback(async () => {
    try {
      const updated = await clearLayout();
      touchedNodeIds.current.clear();
      setCanvas(updated);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Persists a full replacement of a node's raw Markdown body — the inline
  // NodeTextEditor's auto-save.
  const handleSaveText = useCallback(async (id: string, text: string) => {
    try {
      const updated = await updateNode(id, { text });
      setCanvas(updated);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // A `plainMarkdownInclude` node's title-bar button (see `MeshNode.tsx`):
  // that content has no per-node write path of its own (see
  // `Node.plain_markdown_include`'s own doc comment), so this opens
  // Source mode pre-scoped to the include's real file instead of trying
  // (and failing) to save an inline edit.
  const handleOpenSourceMode = useCallback((includeNodeId: string) => {
    setSourceInitialInclude(includeNodeId);
    setSourceMode(true);
  }, []);

  // Removes one extra incoming edge (`meshfox:edge from="sourceNodeId"`)
  // from `targetNodeId` — shared by DeletableEdge's own "×" button (on a
  // real extra edge) and `onEdgesChangeAndPersist`'s keyboard-delete
  // handling below.
  const removeExtraEdge = useCallback(
    (targetNodeId: string, sourceNodeId: string) => {
      if (!canvas) return;
      const target = canvas.nodes.find((n) => n.id === targetNodeId);
      if (!target) return;
      const next = (target.extraParents ?? []).filter((e) => e.from !== sourceNodeId);
      updateNode(targetNodeId, { extraParents: next })
        .then(setCanvas)
        .catch((e) => setError(String(e)));
    },
    [canvas],
  );

  // Patches one extra edge's own styling (label/color/style/arrow ends) —
  // the on-canvas edge editor's auto-save (see DeletableEdge), fired with
  // just the changed field(s). Full-array replace, same as every other
  // `extraParents` write here — re-sending the untouched entries alongside
  // is a harmless no-op.
  const updateExtraEdgeStyle = useCallback(
    (targetNodeId: string, sourceNodeId: string, patch: Partial<Omit<ExtraEdgeDto, "from">>) => {
      if (!canvas) return;
      const target = canvas.nodes.find((n) => n.id === targetNodeId);
      if (!target) return;
      const next = (target.extraParents ?? []).map((e) =>
        e.from === sourceNodeId ? { ...e, ...patch } : e,
      );
      updateNode(targetNodeId, { extraParents: next })
        .then(setCanvas)
        .catch((e) => setError(String(e)));
    },
    [canvas],
  );

  // Deletes `nodeId`'s structural (nesting) parent edge, promoting one of
  // its *other* declared incoming edges to take its place (see
  // `reparentNode` and `mdcanvas::reparent_node`) — shared by
  // DeletableEdge's own "×" button (on an eligible structural edge) and
  // `onEdgesChangeAndPersist`'s keyboard-delete handling below. Only
  // reachable when `nodeId` actually has at least one extra parent (the
  // edge-building effect never marks a structural edge deletable
  // otherwise): exactly one promotes it directly; more than one opens
  // ReparentChoiceDialog to ask which.
  const requestReparentEdge = useCallback(
    (nodeId: string) => {
      if (!canvas) return;
      const node = canvas.nodes.find((n) => n.id === nodeId);
      const candidates = (node?.extraParents ?? []).map((e) => e.from);
      if (candidates.length === 0) return;
      if (candidates.length === 1) {
        reparentNode(nodeId, candidates[0])
          .then(setCanvas)
          .catch((e) => setError(String(e)));
        return;
      }
      setReparentPromptNodeId(nodeId);
    },
    [canvas],
  );

  // Sets (or, given `""`, clears) a structural edge's own label —
  // `edgeLabel=` on the *child* node's `meshfox:node`, the only per-edge
  // attribute a plain parent→child edge has (see CanvasNode.edgeLabel's
  // own doc comment for why it lives there rather than on a dedicated
  // edge object the way `ExtraEdgeDto`'s does) — the new structural-edge
  // properties panel's "ok".
  const updateStructuralEdgeLabel = useCallback((nodeId: string, label: string) => {
    updateNode(nodeId, { edgeLabel: label })
      .then(setCanvas)
      .catch((e) => setError(String(e)));
  }, []);

  // Dragging a new connection between two node handles (edit mode only,
  // see `nodesConnectable` on <ReactFlow>) adds an extra `meshfox:edge` —
  // the canvas-native way to create one, alongside NodeSettings' "add
  // edge" picker.
  const handleConnect = useCallback(
    (connection: Connection) => {
      if (!canvas) return;
      const { source, target } = connection;
      if (!source || !target || source === target) return;
      const targetNode = canvas.nodes.find((n) => n.id === target);
      if (!targetNode) return;
      const current = targetNode.extraParents ?? [];
      if (current.some((e) => e.from === source)) return;
      updateNode(target, { extraParents: [...current, { from: source }] })
        .then(setCanvas)
        .catch((e) => setError(String(e)));
    },
    [canvas],
  );

  // Rebuild React Flow nodes/edges whenever the canvas doc (re)loads. Node
  // position edits happen locally via onNodesChange and are only written
  // back to `canvas` by the debounced layout auto-save below, so this must
  // not run on every drag.
  useEffect(() => {
    if (!canvas) return;
    // Every distinct tag already used anywhere in the document — offered
    // as suggestions by `TagEditor` (node settings computes this itself
    // from its own `allNodes` prop; an extra edge's own tags editor has no
    // such prop, so it gets this instead — see `DeletableEdgeData.existingTags`).
    const documentTags = Array.from(new Set(canvas.nodes.flatMap((n) => n.tags ?? [])));
    // The web client computes its own tree-aware default (see
    // `autolayout.ts`) for any node missing a real position/size — always,
    // for `group`, whose box is never stored. Dragging a node in the
    // browser is the only way to give it a real position/size; see
    // README's "Auto-layout" section. Not the server anymore (no more
    // `suggested*` over the API): only the browser actually knows its own
    // viewport width and each node's real rendered content height, neither
    // of which the server has.
    const boxes = computeAutoLayout({
      canvas,
      viewportWidth: viewportWidthRef.current,
      measuredHeight: (id) => measuredHeightsRef.current.get(id),
      foldedNodeIds,
    });
    const byId = new Map(canvas.nodes.map((n) => [n.id, n]));
    setNodes(
      canvas.nodes.map((n) => {
        const isGroup = n.type === "group";
        const isFolded = foldedNodeIds.has(n.id);
        const suggested = n.x === undefined || n.y === undefined;
        const box: LayoutBox | undefined = boxes.get(n.id);
        // A direct child of a `group` stores x/y relative to that group's
        // own anchor, not absolute (see SPEC.md) — React Flow's own
        // `parentId` already means exactly that for a node's `position`,
        // so wiring it up here is what lets the framework move a dragged
        // group's members along with it for free (no more manual delta
        // rewrite, see the deleted `groupMoves` this replaced). See
        // `positionFor` for how `x`/`y` themselves get computed.
        const rawParent = n.parent ? byId.get(n.parent) : undefined;
        const groupParent = rawParent?.type === "group" ? rawParent : undefined;
        // Document-order neighbors among the same structural parent's
        // children — an auto-placed node's own heading order (see
        // `suggested` above) is its *only* sibling order, and moving it
        // past its immediate neighbor either way is exactly what
        // `mdcanvas::move_sibling` does. `undefined` at either end (already
        // first/last) hides the corresponding button entirely rather than
        // showing it disabled.
        const siblings = canvas.nodes.filter((s) => s.parent === n.parent);
        const siblingIdx = siblings.findIndex((s) => s.id === n.id);
        const prevSibling = siblingIdx > 0 ? siblings[siblingIdx - 1] : undefined;
        const nextSibling =
          siblingIdx >= 0 && siblingIdx < siblings.length - 1 ? siblings[siblingIdx + 1] : undefined;
        const { x, y } = positionFor(n, box, byId, boxes);
        const width = n.width ?? box?.width ?? 280;
        // A group's box is a computed wrapper around its members, not
        // something its own (essentially empty, `pointerEvents: none`) DOM
        // content could ever size itself from — always explicit. Every
        // other node either has a real, authored height (explicit, fixed —
        // never second-guessed) or doesn't, in which case `height` is left
        // undefined entirely so React Flow measures it from its actual
        // rendered content instead of this component dictating a number;
        // `box.maxHeight` (set only for an auto depth-≥2 node — see
        // autolayout.ts) is passed through `data.maxHeight` instead of a
        // `style` override here: MeshNode.tsx applies it directly to its own
        // root element, since a `max-height` on *this* React Flow node
        // wrapper doesn't give that element's own `height: 100%` anything
        // definite to resolve against (confirmed directly — see the
        // `maxHeight` doc comment on `MeshNodeData` in MeshNode.tsx).
        const height = isGroup ? box?.height : isFolded ? FOLDED_HEIGHT : n.height;
        const maxHeight = !isGroup && !isFolded && n.height === undefined ? box?.maxHeight : undefined;
        const style: CSSProperties | undefined = isGroup ? { pointerEvents: "none" } : undefined;
        return {
          id: n.id,
          type: "mesh",
          // `parentId` is React Flow's own native parent/child nesting —
          // once set, `position` above is relative to the parent (exactly
          // what a group member's own x/y already means, see above) and
          // dragging the parent moves every descendant with it automatically,
          // no manual delta rewrite needed (unlike the old per-drag
          // `groupMoves` synthesis this replaced). Deliberately no
          // `extent: "parent"` — that would clamp a member inside the
          // group's *current* (possibly stale) bounds, fighting the "box
          // grows to fit whatever members do" model `layoutGroups` gives it.
          ...(groupParent ? { parentId: groupParent.id } : {}),
          position: { x, y },
          width,
          height,
          // Groups are draggable like everything else (deferring to the
          // global `nodesDraggable` prop, which the Edit toggle controls),
          // but a group's own position is only persisted once it's actually
          // been dragged this session (see `handleSaveLayout`'s own
          // `touchedNodeIds` check) — until then it stays whatever
          // `layoutGroups` derives from its members. Resizing stays off (no
          // `NodeResizer` for groups, below) since the box's *size* is
          // always derived, never authored, even once a group has a real
          // anchor of its own.
          //
          // React Flow gives every node wrapper `pointer-events: all` by
          // default (see its own NodeWrapper) — fine for an opaque node,
          // but a group's box is a big, mostly-empty rectangle with a
          // transparent background that visually wraps its members: you can
          // *see* whatever's underneath (an edge, another node's control)
          // but without this override the wrapper would still silently eat
          // clicks meant for it. `n.style` here is spread after React
          // Flow's own default, so this wins. `.mesh-node-title` (in
          // index.css) opts back into `pointer-events: auto` so the group
          // stays selectable/draggable by grabbing its title bar.
          style,
          data: {
            id: n.id,
            title: n.title,
            level: n.level,
            nodeType: n.type ?? "text",
            suggested,
            maxHeight,
            editMode,
            liveBlocks: {},
            folded: isFolded,
            hasChildren: parentIdSet.has(n.id),
            onToggleFold: () => toggleFold(n.id),
            target: n.target,
            constraintResults: n.constraintResults,
            display: n.display,
            lang: n.lang,
            interpreter: n.interpreter,
            preview: n.preview,
            text: n.text,
            color: n.color,
            effectiveColor: n.effectiveColor,
            tags: n.tags,
            assetBase: n.assetBase,
            plainMarkdownInclude: n.plainMarkdownInclude,
            onRun: (blockName: string, withDeps: boolean) => handleRun(n.id, blockName, withDeps),
            onKill: (blockName: string) => handleKill(n.id, blockName),
            onRunTty: (blockName: string, withDeps: boolean) => handleRunTty(n.id, blockName, withDeps),
            onRecheckConstraint: () => load(),
            onOpenFile: () => handleOpenFile(n.id),
            onAddChild: () => handleAddChild(n.id),
            onMoveUp:
              suggested && prevSibling
                ? () => handleMoveSibling(n.id, { before: prevSibling.id })
                : undefined,
            onMoveDown:
              suggested && nextSibling
                ? () => handleMoveSibling(n.id, { after: nextSibling.id })
                : undefined,
            onClearLayout: suggested ? undefined : () => handleClearNodeLayout(n.id),
            onOpenSettings: () => setSettingsNodeId(n.id),
            onExpand: () => setExpandedNodeId(n.id),
            onSaveText: (text: string) => handleSaveText(n.id, text),
            onOpenSourceMode: handleOpenSourceMode,
            canDelete: !!n.parent,
            onRequestDelete: () => setDeleteConfirmNodeId(n.id),
          },
        };
      }),
    );
    const derivedEdges = deriveEdges(canvas);
    // Two extra edges sharing the same *unordered* node pair — in
    // practice always exactly a mutual link, `A->B` declared alongside
    // `B->A` (an `extraParents` entry can't repeat the same `from` twice
    // under one node, so that's the only way to actually get two here) —
    // get identical `sourceX/Y`/`targetX/Y` regardless of which one is
    // which (both resolve to the exact same pair of handle points, see
    // the routing comment below), so a plain bezier between them draws
    // the exact same curve twice, on top of itself: indistinguishable
    // from one edge, and unclickable for anything but whichever rendered
    // last. `DeletableEdge.tsx`'s `getParallelBezierPath` offsets a
    // curve's own control point perpendicular to *its own* source→target
    // line — which already flips sign between `A->B` and `B->A`
    // (reversing direction negates the perpendicular), so giving every
    // edge in the group the exact same signed offset value below (not
    // alternating it per edge) is what lands `A->B`'s control point and
    // `B->A`'s on opposite sides of the line they share. A structural
    // edge never needs this: each node has exactly one `parent`, so two
    // structural edges can never share an unordered pair to begin with.
    const parallelOffsets = new Map<string, number>();
    {
      const groups = new Map<string, string[]>();
      for (const e of derivedEdges) {
        if (!e.extra) continue;
        const key = [e.source, e.target].sort().join(" ");
        const group = groups.get(key);
        if (group) group.push(e.id);
        else groups.set(key, [e.id]);
      }
      for (const ids of groups.values()) {
        if (ids.length < 2) continue;
        // Same signed value for every edge in the group, deliberately not
        // alternated per edge — see this block's own doc comment above
        // for why an earlier, alternating-by-index version of this
        // canceled itself back out to zero separation instead.
        for (const id of ids) parallelOffsets.set(id, PARALLEL_EDGE_OFFSET);
      }
    }
    setEdges(
      derivedEdges.map((e) => {
        if (!e.extra) {
          // Deleting a structural (nesting) edge means reparenting the
          // node (moving its heading block) — only offered when there's
          // somewhere for it to go: another declared incoming edge
          // (`extraParents`) to promote in its place; see this edge's own
          // properties panel (`canDelete` below), reached the same way an
          // extra edge's is (click the edge, in edit mode). With no
          // candidate, `canDelete` is false and that panel's own "Delete"
          // button doesn't render — same accessibility rule as before this
          // had a panel at all, just relocated (see DeletableEdge.tsx's
          // own doc comment for why an on-canvas midpoint button doesn't
          // work well here).
          const targetNode = canvas.nodes.find((n) => n.id === e.target);
          const candidates = (targetNode?.extraParents ?? []).map((p) => p.from);
          const title =
            candidates.length === 0
              ? ""
              : candidates.length === 1
                ? `Delete this link — "${canvas.nodes.find((n) => n.id === candidates[0])?.title ?? candidates[0]}" becomes the new parent`
                : `Delete this link — choose which of its ${candidates.length} other edges becomes the new parent`;
          return {
            id: e.id,
            source: e.source,
            target: e.target,
            // Every node has more than one handle of each type now (see
            // MeshNode.tsx's routing-only top/bottom pair, added for
            // extra edges) — an explicit id here, matching the plain
            // Left/Right ones every node still has, is what keeps this
            // structural edge on them rather than React Flow's own
            // "no id given, just use whichever handle of that type it
            // finds first" fallback silently picking one of the new
            // ones instead.
            sourceHandle: "source-default",
            targetHandle: "target-default",
            // Right-angle "elbow" routing to match the indented-tree-view
            // layout (see layout.rs) — nodes grow rightward with depth,
            // so a classic step/elbow connector reads better here than a
            // bezier. `"tree"` (not the built-in `"smoothstep"`) even for
            // one with nothing to delete — every structural edge is
            // clickable now (see `edgeTypes` in App.tsx's own
            // `<ReactFlow>`, both map to `DeletableEdge`), since its label
            // is editable regardless of whether it's also deletable.
            type: "tree",
            markerEnd: { type: MarkerType.ArrowClosed },
            data: {
              editMode,
              canDelete: candidates.length > 0,
              title,
              label: e.label,
              onDelete: () => requestReparentEdge(e.target),
              onUpdateLabel: (label: string) => updateStructuralEdgeLabel(e.target, label),
            },
          };
        }
        // Per-edge styling (label/color/line-style/arrow ends) — see
        // `ExtraEdgeDto`. Every field is optional; unset means "keep the
        // look this always had before these existed" (dashed, arrow only
        // at the end, no stroke override — see `dashArrayFor` below and
        // DeletableEdge's own bezier routing, which is what actually makes
        // this "non-rectangular" unlike a structural `tree` edge).
        const strokeColor = e.color ? resolveNodeColor(e.color) : undefined;
        const dash = dashArrayFor(e.style);
        // `color` is only ever included on the marker object below when
        // there's an actual value to put there — never sent as an
        // explicit `color: undefined`, unlike an earlier version of this
        // that rendered every uncolored edge's arrowhead invisible.
        // React Flow's own `createMarkerIds` (`@xyflow/system`) builds
        // each marker definition as `{ color: marker.color || defaultColor,
        // ...marker }` — spreading `marker` *after* that computed
        // fallback, so a `color` key present on our own object at all
        // (even set to `undefined`) clobbers the fallback right back to
        // `undefined` regardless of its value, which its own
        // `ArrowClosedSymbol` component then turns into a literal inline
        // `style="stroke:none;fill:none"` (its own default parameter,
        // `color = 'none'`, only kicks in for a JS `undefined` *value*,
        // not for the key being altogether missing — a distinct thing
        // from what got clobbered here) — invisible, and an inline style
        // beats any CSS fallback rule regardless of specificity. Omitting
        // the key outright instead lets `defaultColor` (`--xy-edge-
        // stroke-default`, the same accent-colored default every other
        // edge already uses) through as intended.
        const markerEnd =
          e.arrowEnd === "none"
            ? undefined
            : { type: MarkerType.ArrowClosed, ...(strokeColor ? { color: strokeColor } : {}) };
        const markerStart =
          e.arrowStart === "arrow"
            ? { type: MarkerType.ArrowClosed, ...(strokeColor ? { color: strokeColor } : {}) }
            : undefined;
        // An extra edge can connect any two nodes anywhere on the canvas —
        // routed through the plain Left/Right handle pair every node also
        // has (the "default" case below), a bezier's horizontal tangents
        // bow the curve sideways regardless of where the other endpoint
        // actually is, which routinely cuts straight across a node's own
        // box on the way there. Picking the vertical Top/Bottom pair
        // instead (see MeshNode.tsx) avoids that whenever the two boxes'
        // *y-ranges* don't overlap at all — a real gap the curve can stay
        // inside of for its whole vertical travel, unlike a horizontal
        // chord between two wide boxes with lots of shared x-range, which
        // has nowhere to go but through one or both of them. Comparing
        // actual box extents rather than center-to-center distance
        // matters here: two wide boxes stacked with a real vertical gap
        // between them can still have their *centers* mostly offset
        // sideways (their width dwarfing the gap), which centroid
        // comparison alone would misread as "these are side by side" and
        // route horizontally anyway — straight through both. Symmetric
        // check for the x-ranges not overlapping (classic side-by-side,
        // already the common case the plain default handled correctly on
        // its own) rounds this out; where both ranges overlap (one box's
        // footprint genuinely overlaps the other's) there's no gap on
        // either axis to route through, so this just keeps the plain
        // default rather than attempting real obstacle-avoiding pathfinding.
        // Absolute box extents (`boxes`, already computed above for
        // `positionFor`) rather than each node's own possibly-group-
        // relative `position`, so this compares like with like regardless
        // of nesting. Always an explicit id either way — same "no id
        // given, just grab the first handle of that type" pitfall as the
        // structural edges above, now that every node has more than one
        // handle of each type.
        const sourceBox = boxes.get(e.source);
        const targetBox = boxes.get(e.target);
        const handles: { sourceHandle: string; targetHandle: string } = (() => {
          if (sourceBox && targetBox) {
            const sourceBottom = sourceBox.y + sourceBox.height;
            const targetBottom = targetBox.y + targetBox.height;
            const yOverlaps = sourceBox.y < targetBottom && targetBox.y < sourceBottom;
            if (!yOverlaps) {
              return sourceBottom <= targetBox.y
                ? { sourceHandle: "source-bottom", targetHandle: "target-top" }
                : { sourceHandle: "source-top", targetHandle: "target-bottom" };
            }
          }
          return { sourceHandle: "source-default", targetHandle: "target-default" };
        })();
        return {
          id: e.id,
          source: e.source,
          target: e.target,
          ...handles,
          type: "extra",
          markerEnd,
          markerStart,
          style: { stroke: strokeColor, strokeDasharray: dash },
          data: {
            editMode,
            editable: true,
            canDelete: true,
            title: "Remove this edge",
            onDelete: () => removeExtraEdge(e.target, e.source),
            onUpdate: (patch: Partial<Omit<ExtraEdgeDto, "from">>) =>
              updateExtraEdgeStyle(e.target, e.source, patch),
            label: e.label,
            color: e.color,
            style: e.style,
            arrowStart: e.arrowStart,
            arrowEnd: e.arrowEnd,
            tags: e.tags,
            existingTags: documentTags,
            parallelOffset: parallelOffsets.get(e.id) ?? 0,
          },
        };
      }),
    );
    setDirty(false);
    // Deliberately excludes handleRun/handleKill/setNodes/setEdges: this effect should
    // only re-run when a fresh canvas doc arrives.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canvas]);

  // Keeps `measuredHeightsRef` current — cheap (a ref write, no state) so
  // this can safely depend on `nodes` directly, unlike the canvas-load
  // effect above.
  useEffect(() => {
    for (const n of nodes) {
      if (n.measured?.height !== undefined) {
        measuredHeightsRef.current.set(n.id, n.measured.height);
      }
    }
  }, [nodes]);

  // Re-runs `autolayout.ts` and re-applies the result to every still
  // auto-placed node whenever one of *their* measured heights actually
  // changes — a node's first real render (correcting the placeholder
  // height the canvas-load effect above had to guess at), or later, its
  // content genuinely growing/shrinking (e.g. streamed run output). Only
  // touches position/width/the max-height style override (plus `height`
  // itself, but *only* for a `group` — its box is always derived from its
  // members, so it has to track a member's corrected height too, unlike an
  // ordinary node's own `height`, deliberately left undefined so it keeps
  // auto-measuring from content) — never rebuilds `data` (that would reset
  // each node's live run state for nothing), except for the `folded` flag
  // itself, which this is also responsible for keeping in sync (see below).
  // `measuredSignature`/`foldedNodeIds` — not `nodes` itself — are the
  // dependencies so this doesn't re-run on every drag/selection change,
  // only when it'd actually produce a different layout.
  const measuredSignature = useMemo(
    () =>
      nodes
        .filter((n) => n.data.suggested)
        .map((n) => `${n.id}:${n.measured?.height ?? ""}`)
        .join("|"),
    [nodes],
  );
  useEffect(() => {
    if (!canvas) return;
    const boxes = computeAutoLayout({
      canvas,
      viewportWidth: viewportWidthRef.current,
      measuredHeight: (id) => measuredHeightsRef.current.get(id),
      foldedNodeIds,
    });
    const byId = new Map(canvas.nodes.map((cn) => [cn.id, cn]));
    setNodes((prev) =>
      prev.map((n) => {
        const isFolded = foldedNodeIds.has(n.id);
        if (n.data.suggested) {
          const box = boxes.get(n.id);
          const canvasNode = byId.get(n.id);
          if (!box || !canvasNode) return n;
          // `positionFor` handles a group member's box the same way the
          // canvas-load effect above does — this branch used to write
          // `box.x`/`box.y` straight back regardless, which was correct
          // for every node *except* a group member (whose `position` has
          // to stay relative to its parent once `parentId` is wired) —
          // silently double-counting the group's own offset the moment a
          // member's height was first measured (this effect's own
          // trigger) undid the canvas-load effect's correct relative
          // value.
          const { x, y } = positionFor(canvasNode, box, byId, boxes);
          const isGroup = n.data.nodeType === "group";
          // A group's own height always tracks its members' derived box.
          // A plain suggested node's is deliberately `undefined` while
          // unfolded (auto-measured from content — see the canvas-load
          // effect above) and `FOLDED_HEIGHT` while folded, computed fresh
          // from `isFolded` rather than carried over from `n.height`: that
          // used to just pass `n.height` through unfolded too, which is
          // fine *unless* it was last set to an explicit `FOLDED_HEIGHT` by
          // the canvas-load effect (any full canvas reload — e.g. creating
          // an unrelated node elsewhere — while this one happened to be
          // folded writes exactly that number). Unfolding afterward flipped
          // `data.folded` back to `false` here (so its body starts
          // rendering again) but left that stale explicit height in place,
          // clamping the box to its folded size regardless — the node's
          // fold toggle visibly changed, the body was back in the DOM, but
          // the box itself never grew to show it (TODO.canvas.md:
          // "Проблема с разворачиванием ноды при редактировании").
          const nextHeight = isGroup ? box.height : isFolded ? FOLDED_HEIGHT : undefined;
          const heightChanged = n.height !== nextHeight;
          if (
            n.position.x === x &&
            n.position.y === y &&
            n.width === box.width &&
            n.data.maxHeight === box.maxHeight &&
            n.data.folded === isFolded &&
            !heightChanged
          ) {
            return n;
          }
          return {
            ...n,
            position: { x, y },
            width: box.width,
            height: nextHeight,
            data: isGroup ? { ...n.data, folded: isFolded } : { ...n.data, maxHeight: box.maxHeight, folded: isFolded },
          };
        }
        // A real (authored/dragged) node never moves here — but its own
        // rendered height still needs to collapse to a compact header when
        // folded (and restore its authored height when unfolded), same
        // compact size `computeAutoLayout`'s own `sizeFor` already reserves
        // for this node's box, so a later auto-placed sibling reflows
        // consistently either way.
        if (n.data.folded === isFolded) return n;
        const canvasHeight = canvas.nodes.find((cn) => cn.id === n.id)?.height;
        return {
          ...n,
          height: isFolded ? FOLDED_HEIGHT : canvasHeight,
          data: { ...n.data, folded: isFolded },
        };
      }),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canvas, measuredSignature, foldedNodeIds]);

  // Anchors the very first view on the root node's own top-left corner, a
  // fixed padding in from the canvas area's, at a fixed readable zoom (see
  // INITIAL_ZOOM) — instead of `fitView`'s content-fitted one (shrinks a
  // large document until its text is unreadable) or centering root (root
  // normally has nothing to its left/above, so centering it just wastes
  // the left half of the screen as empty canvas). Waits for both the flow
  // instance (`onInit`, below) and the root's own measured node (built
  // from `canvas` above) — whichever arrives second triggers this, so it
  // doesn't matter which one that is.
  useEffect(() => {
    if (hasSetInitialView.current || !flowInstance || !canvas) return;
    const root = canvas.nodes.find((n) => !n.parent);
    const rootNode = root && nodes.find((n) => n.id === root.id);
    if (!rootNode) return;
    hasSetInitialView.current = true;
    flowInstance.setViewport(
      {
        x: INITIAL_VIEW_PADDING_X - rootNode.position.x * INITIAL_ZOOM,
        y: INITIAL_VIEW_PADDING_Y - rootNode.position.y * INITIAL_ZOOM,
        zoom: INITIAL_ZOOM,
      },
      { duration: 0 },
    );
  }, [flowInstance, canvas, nodes]);

  // Toggling Edit mode shouldn't rebuild the whole graph (that would reset
  // any in-progress drag/selection) — just patch the flags every node
  // reads.
  //
  // Also re-wires `onRun`/`onKill`/`onToggleFold`: those close over
  // `handleRun`/`handleKill`/`toggleFold`, which in turn close over
  // `editMode`/`foldedNodeIds`/`canvas`/`flowInstance` (see `executeRun`
  // and `toggleFold`'s own doc comment). The *other* effect that builds
  // these closures only runs when `canvas` itself changes, not on an
  // Edit-mode toggle or a fold/unfold, so without refreshing them here
  // too, every already-rendered node's run button (or fold toggle) would
  // keep calling the stale pre-toggle closure — a run button quietly
  // running with the *old* `editMode` (never persisting, never reloading)
  // even though the toolbar now says "editing"; a fold toggle's own
  // closure permanently stuck thinking `foldedNodeIds` is whatever it was
  // on the very first canvas load (empty, before `resolveDefaultFold`
  // even ran) — harmless for the toggle itself (its `setFoldedNodeIds`
  // call uses the functional-updater form, immune to this), but exactly
  // the trap its own "did this just reveal a subtree wider than the
  // viewport" check fell into otherwise.
  useEffect(() => {
    setNodes((nds) =>
      nds.map((n) => ({
        ...n,
        data: {
          ...n.data,
          editMode,
          onRun: (blockName: string, withDeps: boolean) => handleRun(n.id, blockName, withDeps),
          onKill: (blockName: string) => handleKill(n.id, blockName),
          onRunTty: (blockName: string, withDeps: boolean) => handleRunTty(n.id, blockName, withDeps),
          onToggleFold: () => toggleFold(n.id),
        },
      })),
    );
    // Both deletable-edge kinds' "×" (see DeletableEdge.tsx) is gated on
    // the same flag.
    setEdges((eds) =>
      eds.map((e) => (e.type === "extra" || e.type === "tree" ? { ...e, data: { ...e.data, editMode } } : e)),
    );
  }, [editMode, setNodes, setEdges, handleRun, handleKill, handleRunTty, toggleFold, load]);

  // Every node reachable without descending into a folded subtree, in
  // document (depth-first) order — the same order keyboard nav's j/k walks
  // (feature 5). The set derived from it is what's actually handed to React
  // Flow, filtered from `nodes`/`edges` rather than baked into that state
  // itself, so a fold toggle never has to rebuild (and thus reset the live
  // run state of) anything that stays visible.
  const visibleOrder = useMemo(
    () => (canvas ? visibleNodeIds(canvas, foldedNodeIds) : []),
    [canvas, foldedNodeIds],
  );
  const visibleNodeIdSet = useMemo(() => new Set(visibleOrder), [visibleOrder]);
  const visibleNodes = useMemo(() => nodes.filter((n) => visibleNodeIdSet.has(n.id)), [nodes, visibleNodeIdSet]);
  const visibleEdges = useMemo(
    () => edges.filter((e) => visibleNodeIdSet.has(e.source) && visibleNodeIdSet.has(e.target)),
    [edges, visibleNodeIdSet],
  );

  // Keyboard-driven focus (j/k/h/l/Enter, see the keydown effect below) —
  // deliberately separate from React Flow's own mouse-driven `selected`
  // (NodeResizer visibility, multi-select), so a keypress never silently
  // changes what's selected for resize/multi-op purposes. `nodeDepth`
  // mirrors the TUI's own per-row depth (`tree::TreeRow.depth`) — how many
  // `parent` hops from root, used by `h`'s "jump to the nearest preceding
  // row at depth-1" (no stored parent-pointer needed, same linear scan the
  // TUI's `collapse_or_to_parent` does over its own flat row list).
  const [focusedNodeId, setFocusedNodeId] = useState<string | null>(null);
  const nodeDepth = useMemo(() => {
    if (!canvas) return new Map<string, number>();
    return new Map(visibleOrder.map((id) => [id, pathTo(canvas, id).length]));
  }, [canvas, visibleOrder]);
  const focusNode = useCallback(
    (id: string) => {
      setFocusedNodeId(id);
      const rfNode = flowInstance?.getNode(id);
      if (flowInstance && rfNode) {
        const w = rfNode.measured?.width ?? rfNode.width ?? 280;
        const h = rfNode.measured?.height ?? rfNode.height ?? 160;
        flowInstance.setCenter(rfNode.position.x + w / 2, rfNode.position.y + h / 2, {
          zoom: flowInstance.getZoom(),
          duration: 400,
        });
      }
    },
    [flowInstance],
  );
  useEffect(() => {
    function isEditableTarget(el: Element | null): boolean {
      if (!(el instanceof HTMLElement)) return false;
      return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable;
    }
    function onKeyDown(e: KeyboardEvent) {
      if (isEditableTarget(document.activeElement)) return;
      // Any modal/dialog open — keys here are for whatever it's showing,
      // not canvas navigation. Belt-and-suspenders alongside the
      // activeElement check above, since not every dialog necessarily
      // focuses an input (e.g. a plain confirm dialog).
      if (
        sourceMode ||
        settingsNodeId ||
        ttySession ||
        varsModal ||
        configureVars ||
        deleteConfirmNodeId ||
        reparentPromptNodeId ||
        expandedNodeId ||
        autoLayoutConfirmOpen
      ) {
        return;
      }
      if (visibleOrder.length === 0) return;

      // `e.code` (the physical key position, e.g. "KeyJ" for whatever key
      // sits where a QWERTY "j" would) rather than `e.key` (the character
      // that key actually produces under the *active* layout) — on a
      // Cyrillic layout, the physical j/k/h/l keys produce "о"/"л"/"р"/"д",
      // not "j"/"k"/"h"/"l", so checking `e.key` here silently never
      // matched for anyone not on a Latin layout. Arrow keys and Enter
      // have no such ambiguity (`e.code`'s "ArrowDown"/"Enter" already
      // match `e.key`'s), so those still read `e.key` for clarity.
      const isDown = e.code === "KeyJ" || e.key === "ArrowDown";
      const isUp = e.code === "KeyK" || e.key === "ArrowUp";
      const isRight = e.code === "KeyL" || e.key === "ArrowRight";
      const isLeft = e.code === "KeyH" || e.key === "ArrowLeft";

      if (isDown || isUp) {
        e.preventDefault();
        const delta = isDown ? 1 : -1;
        const currentIndex = focusedNodeId ? visibleOrder.indexOf(focusedNodeId) : -1;
        const nextIndex = Math.min(Math.max(currentIndex + delta, 0), visibleOrder.length - 1);
        focusNode(visibleOrder[nextIndex]);
        return;
      }
      if (!focusedNodeId) return;
      // Whether the focused node supports folding at all (see `canFold`)
      // — every node except an already-title-only one with no children,
      // where folding could neither change its own row nor hide any
      // subtree, so there's nothing for h/Enter to actually do.
      const focusedNode = canvas?.nodes.find((n) => n.id === focusedNodeId);
      const foldable = focusedNode ? canFold(focusedNode, parentIdSet) : false;
      if (isRight) {
        if (foldedNodeIds.has(focusedNodeId)) {
          e.preventDefault();
          toggleFold(focusedNodeId);
        }
        return;
      }
      if (isLeft) {
        // Any foldable node can be folded, not just ones with children
        // (see MeshNode.tsx's `FoldToggle`) — `h` folds the focused node
        // first; pressed again once it's already folded (nothing left to
        // collapse here), or on a node that was never foldable to begin
        // with, it falls through to jumping up to the parent instead, same
        // as the TUI's own `collapse_or_to_parent`.
        if (foldable && !foldedNodeIds.has(focusedNodeId)) {
          e.preventDefault();
          toggleFold(focusedNodeId);
          return;
        }
        const depth = nodeDepth.get(focusedNodeId);
        if (depth === undefined || depth === 0) return;
        const currentIndex = visibleOrder.indexOf(focusedNodeId);
        for (let i = currentIndex - 1; i >= 0; i--) {
          if (nodeDepth.get(visibleOrder[i]) === depth - 1) {
            e.preventDefault();
            focusNode(visibleOrder[i]);
            break;
          }
        }
        return;
      }
      if (e.key === "Enter" && foldable) {
        e.preventDefault();
        toggleFold(focusedNodeId);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    focusedNodeId,
    visibleOrder,
    nodeDepth,
    foldedNodeIds,
    focusNode,
    toggleFold,
    canvas,
    parentIdSet,
    sourceMode,
    settingsNodeId,
    ttySession,
    varsModal,
    configureVars,
    deleteConfirmNodeId,
    reparentPromptNodeId,
    expandedNodeId,
    autoLayoutConfirmOpen,
  ]);
  const focusedRenderNodes = useMemo(
    () =>
      visibleNodes.map((n) =>
        (n.data.focused ?? false) === (n.id === focusedNodeId) ? n : { ...n, data: { ...n.data, focused: n.id === focusedNodeId } },
      ),
    [visibleNodes, focusedNodeId],
  );

  const settingsNode = useMemo(
    () => canvas?.nodes.find((n) => n.id === settingsNodeId),
    [canvas, settingsNodeId],
  );

  const expandedNode = useMemo(() => nodes.find((n) => n.id === expandedNodeId), [nodes, expandedNodeId]);

  const deleteConfirmNode = useMemo(
    () => canvas?.nodes.find((n) => n.id === deleteConfirmNodeId),
    [canvas, deleteConfirmNodeId],
  );
  const deleteConfirmChildCount = useMemo(
    () => (deleteConfirmNode ? (canvas?.nodes.filter((n) => n.parent === deleteConfirmNode.id).length ?? 0) : 0),
    [canvas, deleteConfirmNode],
  );

  const reparentPromptNode = useMemo(
    () => canvas?.nodes.find((n) => n.id === reparentPromptNodeId),
    [canvas, reparentPromptNodeId],
  );

  // Ids of nodes the user has actually dragged/resized (this session) —
  // `handleSaveLayout` only ever persists a position/size for a node that's
  // either already real (not `suggested`) or in this set, so dragging one
  // node doesn't silently bake every *other*, still-auto-placed node's
  // suggested position into the file as if it were authored data.
  const touchedNodeIds = useRef<Set<string>>(new Set());

  // Set (by `onNodesChangeAndMark`, below) the instant a drag/resize
  // *finishes* — checked by the effect right after `handleSaveLayout`,
  // which actually persists. A ref, not state: setting it doesn't need to
  // trigger a render, only to be seen by that effect once `nodes` itself
  // updates (see its own comment for why the save can't just happen
  // synchronously here).
  const layoutGestureEnded = useRef(false);

  const onNodesChangeAndMark: typeof onNodesChange = useCallback(
    (changes) => {
      // Dragging a group used to have nothing of its own to persist (its
      // box was always re-derived from its members) and needed a manual
      // delta rewrite of every descendant to move along with it. Neither
      // is true anymore: group members are wired with React Flow's own
      // `parentId` (see the node-building effect above), which moves them
      // with their parent natively, and a group's own dragged position is
      // now real, persistable data in its own right (see
      // `handleSaveLayout`) — so `changes` needs no synthesis here at all,
      // just the same touched/dirty/gesture-end bookkeeping every other
      // node's drag already gets.
      onNodesChange(changes);
      // React Flow also emits a `dimensions` change for every node purely
      // from its own passive remeasurement (e.g. right after `setNodes`
      // replaces the array with fresh object references — precisely what
      // the layout auto-save's own canvas-reload does) — those never carry
      // a `resizing` field, unlike one actually driven by dragging
      // `NodeResizer`'s handle. Same for `position` changes: only a real
      // drag carries `dragging`.
      const isUserDriven = (c: (typeof changes)[number]) =>
        (c.type === "position" && c.dragging !== undefined) ||
        (c.type === "dimensions" && c.resizing !== undefined);
      // The *last* event of a drag/resize carries `dragging: false` /
      // `resizing: false` specifically — every event before that (while
      // the gesture is still in progress) has it `true`. Saving on every
      // intermediate one would be pointless (the position/size is about to
      // change again immediately) — only the final value, once the user
      // actually lets go, is worth persisting.
      const isGestureEnd = (c: (typeof changes)[number]) =>
        (c.type === "position" && c.dragging === false) ||
        (c.type === "dimensions" && c.resizing === false);
      let touched = false;
      for (const c of changes) {
        if (isUserDriven(c) && "id" in c) {
          touchedNodeIds.current.add(c.id);
          touched = true;
        }
        if (isGestureEnd(c)) layoutGestureEnded.current = true;
      }
      if (touched) setDirty(true);
    },
    [onNodesChange],
  );

  // Intercepts a "remove" change (Backspace/Delete with an edge selected)
  // for either deletable-edge kind, persisting server-side instead of
  // forwarding it to React Flow's own local state — the subsequent canvas
  // reload is what actually drops it from `edges`, keeping the file the
  // single source of truth. Every structural edge is selectable now (its
  // label is editable regardless of whether it's also deletable — see its
  // own `canDelete`), so Backspace on one with nothing to promote in its
  // place *does* reach here — `requestReparentEdge` itself is what no-ops
  // on an empty candidate list, not this filter.
  const onEdgesChangeAndPersist: typeof onEdgesChange = useCallback(
    (changes) => {
      const toForward = changes.filter((change) => {
        if (change.type !== "remove") return true;
        const edge = edges.find((e) => e.id === change.id);
        if (edge?.type === "extra") {
          removeExtraEdge(edge.target, edge.source);
          return false;
        }
        if (edge?.type === "tree") {
          requestReparentEdge(edge.target);
          return false;
        }
        return true;
      });
      if (toForward.length > 0) onEdgesChange(toForward);
    },
    [edges, onEdgesChange, removeExtraEdge, requestReparentEdge],
  );

  const handleSaveLayout = useCallback(async () => {
    if (!canvas) return;
    // A group's *size* is always derived (see `layoutGroups`) — never
    // treat it as authored data to persist. Its own *position*, though, is
    // a real anchor now (see SPEC.md) — persisted exactly like any other
    // node's, but only once it's actually been dragged this session
    // (`touchedNodeIds`; a group is never `suggested`, so it'd otherwise
    // always look "touched enough" to save). Every non-group node keeps
    // the same rule as before: skip one that's still showing its
    // server-suggested position/size and that the user hasn't actually
    // touched — dragging one node shouldn't silently pin every other,
    // still-auto-placed node's suggested box into the file as if it were
    // real authored data. A group *member*'s own `n.position` is already
    // group-relative here (React Flow's own `parentId` semantics — see the
    // node-building effect above), so it's persisted verbatim, no delta
    // math needed.
    const layout = new Map(
      nodes
        .filter((n) =>
          n.data.nodeType === "group"
            ? touchedNodeIds.current.has(n.id)
            : !n.data.suggested || touchedNodeIds.current.has(n.id),
        )
        .map((n) => [
          n.id,
          {
            x: snapToGrid(n.position.x),
            y: snapToGrid(n.position.y),
            width: n.data.nodeType === "group" || n.width === undefined ? undefined : snapToGrid(n.width),
            height: n.data.nodeType === "group" || n.height === undefined ? undefined : snapToGrid(n.height),
          },
        ]),
    );
    const updated: CanvasDoc = {
      ...canvas,
      nodes: canvas.nodes.map((n) => {
        const box = layout.get(n.id);
        if (!box) return n;
        return {
          ...n,
          x: box.x,
          y: box.y,
          width: box.width ?? n.width,
          height: box.height ?? n.height,
        };
      }),
    };
    try {
      await saveCanvas(updated);
      // Re-fetch rather than `setCanvas(updated)`: `updated` only carries
      // the positions/sizes this client already knew about, but saving can
      // shift server-computed values it didn't — most importantly every
      // group's box, always re-derived from its members' *new* positions
      // (see layout.rs) and never itself part of `updated`'s patched
      // fields. Skipping this was why a group's box used to sit stale until
      // a full page reload, however much its members grew or shrank.
      const fresh = await fetchCanvas();
      setCanvas(fresh);
      setDirty(false);
    } catch (e) {
      setError(String(e));
    }
  }, [canvas, nodes]);

  // Auto-saves layout (position/size), but only once a drag/resize
  // actually *finishes* — not, e.g., 800ms after the last intermediate
  // frame, which would still fire mid-gesture for anything but a very
  // quick flick. `onNodesChangeAndMark` sets `layoutGestureEnded` the
  // instant it sees that final event; this effect can't just save
  // synchronously right there, though, since `onNodesChange` updates
  // `nodes` asynchronously — reading `nodes` in the same call would still
  // see the *previous* position. Depending on `nodes` here means this only
  // ever runs after that update has actually committed, so the save
  // reads the true final value.
  useEffect(() => {
    if (!layoutGestureEnded.current) return;
    layoutGestureEnded.current = false;
    handleSaveLayout();
  }, [nodes, handleSaveLayout]);

  // Aggregate pass/fail across every embedded constraint fence's most
  // recently evaluated status (see `ConstraintStatusDto`), across every
  // node — `null` when the document has none at all, so the toolbar badge
  // below can render nothing rather than a vacuous "0/0".
  const constraintStats = useMemo(() => {
    if (!canvas) return null;
    const results = canvas.nodes.flatMap((n) => n.constraintResults ?? []);
    if (results.length === 0) return null;
    const failed = results.filter((r) => !r.ok).length;
    return { total: results.length, failed };
  }, [canvas]);

  const toolbar = useMemo(
    () => (
      <div className="toolbar">
        <strong>meshfox</strong>
        {constraintStats && (
          <span
            className={
              constraintStats.failed > 0
                ? "constraint-stats constraint-stats-fail"
                : "constraint-stats constraint-stats-ok"
            }
            title={
              constraintStats.failed > 0
                ? `${constraintStats.failed} of ${constraintStats.total} constraints failing`
                : `All ${constraintStats.total} constraint${constraintStats.total === 1 ? "" : "s"} pass`
            }
          >
            🛡 {constraintStats.total - constraintStats.failed}/{constraintStats.total}
          </span>
        )}
        {editMode ? (
          <>
            <span className="mode-badge mode-badge-edit">editing</span>
            {dirty && <span className="saving-indicator">saving layout…</span>}
            <button
              className={sourceMode ? "deps-toggle deps-toggle-active" : "deps-toggle"}
              onClick={() => setSourceMode((s) => !s)}
              title="Edit the document's raw Markdown source directly"
            >
              {sourceMode ? "Canvas" : "Source"}
            </button>
            <button
              className="deps-toggle"
              onClick={() => setAutoLayoutConfirmOpen(true)}
              title="Clear every node's stored position and size, reverting to auto-placed"
            >
              Auto-layout
            </button>
            <button
              className="deps-toggle"
              onClick={() => setDocumentOptionsOpen(true)}
              title="Document-wide settings (see SPEC.md's Options) — e.g. whether the canvas opens with everything expanded by default"
            >
              ⚙ options
            </button>
            <button
              onClick={() => setEditMode(false)}
              disabled={sourceMode && sourceDirty}
              title={sourceMode && sourceDirty ? "Save or discard source changes first" : undefined}
            >
              done
            </button>
          </>
        ) : (
          <>
            <span className="mode-badge">read-only</span>
            <button onClick={() => setEditMode(true)}>Edit</button>
          </>
        )}
        {hasConfigurableVars && (
          <button
            className="deps-toggle"
            onClick={handleConfigure}
            title="Interactively resolve every declared meshfox:var and save the answers (see SPEC.md's Variables) — same as `meshfox configure`"
          >
            ⚙ configure
          </button>
        )}
        {error && <span className="error">{error}</span>}
        <button
          className="deps-toggle theme-toggle"
          onClick={cycleTheme}
          title="Cycle the color theme: follow the OS, or pin light/dark regardless of it"
        >
          {themePreference === "system" ? "Theme: Auto" : themePreference === "light" ? "Theme: Light" : "Theme: Dark"}
        </button>
      </div>
    ),
    [
      editMode,
      sourceMode,
      sourceDirty,
      dirty,
      error,
      constraintStats,
      hasConfigurableVars,
      handleConfigure,
      themePreference,
      cycleTheme,
    ],
  );

  if (serverGone) {
    return (
      <div className="server-gone">
        <p>meshfox: the server has stopped.</p>
        <p>You can close this tab.</p>
      </div>
    );
  }

  return (
    <div className="app">
      {toolbar}
      <div className="canvas-area">
        {sourceMode ? (
          <CanvasSourceEditor
            initialInclude={sourceInitialInclude}
            onSaved={handleSourceSaved}
            onClose={() => setSourceMode(false)}
            onDirtyChange={setSourceDirty}
          />
        ) : (
          <ReactFlow
            nodes={focusedRenderNodes}
            edges={visibleEdges}
            onNodesChange={onNodesChangeAndMark}
            onEdgesChange={onEdgesChangeAndPersist}
            onConnect={handleConnect}
            onInit={setFlowInstance}
            nodeTypes={nodeTypes}
            edgeTypes={edgeTypes}
            // Without this, the Controls/MiniMap panels stay on React
            // Flow's light-mode colors (near-white button background,
            // `color: inherit`) even when the OS is in dark mode, which
            // resolves `inherit` to this app's light `--fg` text color —
            // near-invisible light-gray icons on a near-white background.
            // `themePreference`'s value ("system"/"light"/"dark") is
            // exactly React Flow's own `colorMode` type, so the toolbar's
            // manual override (see theme.ts) reaches these panels too,
            // not just this app's own `--fg`/`--bg`-driven CSS.
            colorMode={themePreference}
            // React Flow's own default (a flat gray, `#b1b1b7`) for an
            // arrowhead with no explicit per-edge `color` — the common
            // case, most edges never set one. `var(--accent)` matches
            // this app's own edge-stroke default (see index.css's
            // `--xy-edge-stroke-default` override) instead, so an
            // uncolored edge's arrowhead doesn't clash with its own line.
            // A literal CSS custom property reference works fine as an
            // inline style value (which is what this ultimately becomes —
            // see `createMarkerIds`/`ArrowClosedSymbol` in
            // `@xyflow/system`/`@xyflow/react`), resolving through the
            // cascade like any other `var()`, light/dark theme included.
            defaultMarkerColor="var(--accent)"
            nodesDraggable={editMode}
            // Dragging a new connection between handles creates an extra
            // `meshfox:edge` (see handleConnect) — only worth allowing once
            // there's somewhere for the result to be saved.
            nodesConnectable={editMode}
            // Wheel/two-finger scroll pans instead of zooming; zoom stays on
            // the Controls buttons and pinch (trackpad/touchscreen), which
            // React Flow keeps separate from plain scroll via zoomOnPinch.
            panOnScroll
            zoomOnScroll={false}
            zoomOnPinch
            // Matches the common case (root at canvas-space (0, 0), same as
            // a fresh `meshfox create`d file) so there's nothing to correct
            // visibly once the initial-view effect above runs for a root
            // positioned anywhere else.
            defaultViewport={{ x: INITIAL_VIEW_PADDING_X, y: INITIAL_VIEW_PADDING_Y, zoom: INITIAL_ZOOM }}
          >
            <Background />
            <Controls />
            <MiniMap />
          </ReactFlow>
        )}
      </div>
      {varsModal && (
        <VarsForm vars={varsModal.missing} onSubmit={handleVarsSubmit} onCancel={handleVarsCancel} />
      )}
      {configureVars && (
        <VarsForm
          vars={configureVars}
          onSubmit={handleConfigureSubmit}
          onCancel={handleConfigureCancel}
          title="Configure declared variables"
          hint="Every declared variable, whether or not any block currently needs it — answered here (even left unchanged) is saved right away, same as `meshfox configure`."
          submitLabel="save"
        />
      )}
      {documentOptionsOpen && canvas && (
        <DocumentOptions
          options={canvas.options ?? []}
          onSubmit={handleDocumentOptionsSubmit}
          onCancel={handleDocumentOptionsCancel}
        />
      )}
      {ttySession && (
        <TtyPanel
          key={`${ttySession.path.join("/")}/${ttySession.blockName}`}
          path={ttySession.path}
          blockName={ttySession.blockName}
          withDeps={ttySession.withDeps}
          persist={editMode}
          vars={ttySession.vars}
          onClose={() => setTtySession(null)}
        />
      )}
      {expandedNode && (
        <NodeExpandPanel
          node={expandedNode}
          onClose={() => setExpandedNodeId(null)}
          canvas={canvas}
          nodes={nodes}
          edges={edges}
          onNodesChange={onNodesChangeAndMark}
          onEdgesChange={onEdgesChangeAndPersist}
          editMode={editMode}
          onAddChild={handleAddChild}
          themePreference={themePreference}
        />
      )}
      {settingsNode && (
        <NodeSettings
          // Keyed on id so a successful id rename remounts this modal fresh
          // (new `node.id`/`node.title` as the starting point for every
          // field, including NodeSettings' own id-suggestion tracking)
          // rather than reusing stale local state under the old id.
          key={settingsNode.id}
          node={settingsNode}
          allNodes={canvas?.nodes ?? []}
          onChange={handleNodeSettingsChange}
          onRenameId={handleNodeIdChange}
          onClearId={handleNodeIdClear}
          onClose={() => setSettingsNodeId(null)}
        />
      )}
      {deleteConfirmNode && (
        <DeleteNodeDialog
          title={deleteConfirmNode.title}
          childCount={deleteConfirmChildCount}
          parentTitle={canvas?.nodes.find((n) => n.id === deleteConfirmNode.parent)?.title}
          onDelete={(mode) => {
            setDeleteConfirmNodeId(null);
            handleDeleteNode(deleteConfirmNode.id, mode);
          }}
          onCancel={() => setDeleteConfirmNodeId(null)}
        />
      )}
      {autoLayoutConfirmOpen && (
        <AutoLayoutConfirmDialog
          onConfirm={() => {
            setAutoLayoutConfirmOpen(false);
            handleAutoLayout();
          }}
          onCancel={() => setAutoLayoutConfirmOpen(false)}
        />
      )}
      {reparentPromptNode && (
        <ReparentChoiceDialog
          nodeTitle={reparentPromptNode.title}
          candidates={(reparentPromptNode.extraParents ?? []).map(({ from: id }) => ({
            id,
            title: canvas?.nodes.find((n) => n.id === id)?.title ?? id,
          }))}
          onChoose={(newParentId) => {
            setReparentPromptNodeId(null);
            reparentNode(reparentPromptNode.id, newParentId)
              .then(setCanvas)
              .catch((e) => setError(String(e)));
          }}
          onCancel={() => setReparentPromptNodeId(null)}
        />
      )}
    </div>
  );
}
