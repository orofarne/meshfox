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
  type NodePositionChange,
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
  createNode,
  updateNode,
  deleteNode,
  reparentNode,
  renameNodeId,
  watchChanges,
  clearLayout,
  type RunEvent,
  type NodePatch,
} from "./api";
import type { CanvasDoc, ExtraEdgeDto, VarStatus } from "./types";
import { pathTo, deriveEdges, subtreeIds } from "./tree";
import { computeAutoLayout, type LayoutBox } from "./autolayout";
import { buildBlockGraph, resolveChain, crossNodeDepEdges, type BlockAddr } from "./deps";
import { parseBody, type CodeSegment } from "./fence";
import { MeshNode, resolveNodeColor, type MeshNodeData, type LiveBlockState } from "./MeshNode";
import { VarsForm } from "./VarsForm";
import { TtyPanel } from "./TtyPanel";
import { NodeExpandPanel } from "./NodeExpandPanel";
import { NodeSettings } from "./NodeSettings";
import { DeleteNodeDialog } from "./DeleteNodeDialog";
import { AutoLayoutConfirmDialog } from "./AutoLayoutConfirmDialog";
import { ReparentChoiceDialog } from "./ReparentChoiceDialog";
import { DeletableEdge } from "./DeletableEdge";
import { CanvasSourceEditor } from "./CanvasSourceEditor";

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
 * hand-authored `w=`/`h=` node or `meshfox fmt`'s auto-layout sizes for —
 * so this is the zoom every node was implicitly sized to read well at. */
const INITIAL_ZOOM = 1;

/** How far the root node's own top-left corner sits from the canvas area's
 * top-left on first load — screen pixels, independent of zoom. Anchoring
 * the corner (see the initial-view effect below) rather than centering the
 * root, since a root normally has nothing to its left or above: centering
 * it would just waste the whole left half of the screen as empty canvas. */
const INITIAL_VIEW_PADDING_X = 80;
const INITIAL_VIEW_PADDING_Y = 80;

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

export default function App() {
  const [canvas, setCanvas] = useState<CanvasDoc | null>(null);
  const [error, setError] = useState<string | null>(null);
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
  // Dependency arrows (see ./deps.ts) are drawn in a distinct style from
  // the tree/`meshfox:edge` connectors — off by default so a canvas with no
  // `deps=` blocks looks exactly as it did before this existed.
  const [showDeps, setShowDeps] = useState(false);
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
  // up on every block about to run, not just the clicked one) and to draw
  // dependency arrows. The server remains the source of truth for actually
  // resolving and executing the chain.
  const blockGraph = useMemo(() => (canvas ? buildBlockGraph(canvas) : new Map()), [canvas]);

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
    if (!sourceMode) setSourceDirty(false);
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

  // NodeSettings' auto-save — fired (debounced) as fields change, and once
  // more when the modal closes. Never closes the modal itself; errors
  // (e.g. the server rejecting an invalid type/target combination) are
  // just surfaced, since the user is likely still mid-edit.
  const handleNodeSettingsChange = useCallback(async (id: string, patch: NodePatch) => {
    try {
      const updated = await updateNode(id, patch);
      setCanvas(updated);
    } catch (e) {
      setError(String(e));
    }
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
    // The web client computes its own tree-aware default (see
    // `autolayout.ts`) for any node missing a real position/size — always,
    // for `group`, whose box is never stored. `meshfox fmt` can write these
    // into the file for anything but groups; see README's "Auto-layout"
    // section. Not the server anymore (no more `suggested*` over the API):
    // only the browser actually knows its own viewport width and each
    // node's real rendered content height, neither of which the server has.
    const boxes = computeAutoLayout({
      canvas,
      viewportWidth: viewportWidthRef.current,
      measuredHeight: (id) => measuredHeightsRef.current.get(id),
    });
    setNodes(
      canvas.nodes.map((n) => {
        const isGroup = n.type === "group";
        const suggested = n.x === undefined || n.y === undefined;
        const box: LayoutBox | undefined = boxes.get(n.id);
        const x = n.x ?? box?.x ?? 0;
        const y = n.y ?? box?.y ?? 0;
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
        const height = isGroup ? box?.height : n.height;
        const maxHeight = !isGroup && n.height === undefined ? box?.maxHeight : undefined;
        const style: CSSProperties | undefined = isGroup ? { pointerEvents: "none" } : undefined;
        return {
          id: n.id,
          type: "mesh",
          position: { x, y },
          width,
          height,
          // Groups are draggable like everything else (deferring to the
          // global `nodesDraggable` prop, which the Edit toggle controls),
          // but a group's own position is never itself persisted — dragging
          // one instead moves its whole subtree along with it (see
          // `onNodesChangeAndMark`), and the group's box is then re-derived
          // from its members' new positions on the next layout save (see
          // `autolayout.ts`/`layout.rs`). Resizing stays off (no
          // `NodeResizer` for groups, below) since the box is never
          // authored directly either way.
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
            showDeps,
            target: n.target,
            constraintResults: n.constraintResults,
            display: n.display,
            lang: n.lang,
            interpreter: n.interpreter,
            text: n.text,
            color: n.color,
            tags: n.tags,
            onRun: (blockName: string, withDeps: boolean) => handleRun(n.id, blockName, withDeps),
            onKill: (blockName: string) => handleKill(n.id, blockName),
            onRunTty: (blockName: string, withDeps: boolean) => handleRunTty(n.id, blockName, withDeps),
            onRecheckConstraint: () => load(),
            onOpenFile: () => handleOpenFile(n.id),
            onAddChild: () => handleAddChild(n.id),
            onOpenSettings: () => setSettingsNodeId(n.id),
            onExpand: () => setExpandedNodeId(n.id),
            onSaveText: (text: string) => handleSaveText(n.id, text),
            canDelete: !!n.parent,
            onRequestDelete: () => setDeleteConfirmNodeId(n.id),
          },
        };
      }),
    );
    setEdges(
      deriveEdges(canvas).map((e) => {
        if (!e.extra) {
          // Deleting a structural (nesting) edge means reparenting the
          // node (moving its heading block) — only offered when there's
          // somewhere for it to go: another declared incoming edge
          // (`extraParents`) to promote in its place. With none, this stays
          // a plain, non-deletable edge exactly as before this existed.
          const targetNode = canvas.nodes.find((n) => n.id === e.target);
          const candidates = (targetNode?.extraParents ?? []).map((p) => p.from);
          if (candidates.length === 0) {
            return {
              id: e.id,
              source: e.source,
              target: e.target,
              // Right-angle "elbow" routing to match the indented-tree-view
              // layout (see layout.rs) — nodes grow rightward with depth,
              // so a classic step/elbow connector reads better here than a
              // bezier.
              type: "smoothstep",
              markerEnd: { type: MarkerType.ArrowClosed },
              deletable: false,
              selectable: false,
            };
          }
          const title =
            candidates.length === 1
              ? `Delete this link — "${canvas.nodes.find((n) => n.id === candidates[0])?.title ?? candidates[0]}" becomes the new parent`
              : `Delete this link — choose which of its ${candidates.length} other edges becomes the new parent`;
          return {
            id: e.id,
            source: e.source,
            target: e.target,
            type: "tree",
            markerEnd: { type: MarkerType.ArrowClosed },
            data: { editMode, title, onDelete: () => requestReparentEdge(e.target) },
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
        const markerEnd =
          e.arrowEnd === "none" ? undefined : { type: MarkerType.ArrowClosed, color: strokeColor };
        const markerStart =
          e.arrowStart === "arrow" ? { type: MarkerType.ArrowClosed, color: strokeColor } : undefined;
        return {
          id: e.id,
          source: e.source,
          target: e.target,
          type: "extra",
          markerEnd,
          markerStart,
          style: { stroke: strokeColor, strokeDasharray: dash },
          data: {
            editMode,
            editable: true,
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
  // each node's live run state for nothing). `measuredSignature` — not
  // `nodes` itself — is the dependency so this doesn't re-run on every
  // drag/selection change, only when it'd actually produce a different
  // layout.
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
    });
    setNodes((prev) =>
      prev.map((n) => {
        if (!n.data.suggested) return n;
        const box = boxes.get(n.id);
        if (!box) return n;
        const isGroup = n.data.nodeType === "group";
        const heightChanged = isGroup && n.height !== box.height;
        if (
          n.position.x === box.x &&
          n.position.y === box.y &&
          n.width === box.width &&
          n.data.maxHeight === box.maxHeight &&
          !heightChanged
        ) {
          return n;
        }
        return {
          ...n,
          position: { x: box.x, y: box.y },
          width: box.width,
          height: isGroup ? box.height : n.height,
          data: isGroup ? n.data : { ...n.data, maxHeight: box.maxHeight },
        };
      }),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canvas, measuredSignature]);

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

  // Toggling Edit mode or "show deps" shouldn't rebuild the whole graph
  // (that would reset any in-progress drag/selection) — just patch the
  // flags every node reads. `showDeps` here is what gates each node's
  // in-body dependency rail (see MeshNode); the cross-node arrows below
  // are gated the same way, independently, since they live in `App`'s own
  // edge list rather than per-node data.
  //
  // Also re-wires `onRun`/`onKill`: those close over `handleRun`/
  // `handleKill`, which in turn close over `editMode` (whether a `cache`d
  // block's output actually gets persisted, and whether a run reloads the
  // canvas afterward) — see `executeRun`. The *other* effect that builds
  // these closures only runs when `canvas` itself changes, not on an
  // Edit-mode toggle, so without refreshing them here too, every already
  // rendered node's run button would keep calling the stale pre-toggle
  // closure — quietly running with the *old* `editMode` (never persisting,
  // never reloading) even though the toolbar now says "editing".
  useEffect(() => {
    setNodes((nds) =>
      nds.map((n) => ({
        ...n,
        data: {
          ...n.data,
          editMode,
          showDeps,
          onRun: (blockName: string, withDeps: boolean) => handleRun(n.id, blockName, withDeps),
          onKill: (blockName: string) => handleKill(n.id, blockName),
          onRunTty: (blockName: string, withDeps: boolean) => handleRunTty(n.id, blockName, withDeps),
        },
      })),
    );
    // Both deletable-edge kinds' "×" (see DeletableEdge.tsx) is gated on
    // the same flag.
    setEdges((eds) =>
      eds.map((e) => (e.type === "extra" || e.type === "tree" ? { ...e, data: { ...e.data, editMode } } : e)),
    );
  }, [editMode, showDeps, setNodes, setEdges, handleRun, handleKill, handleRunTty, load]);

  // Dependency arrows (`deps=` on a fence, cross-node only — same-node deps
  // are shown inline on the block instead, see MeshNode) — computed
  // separately from `edges` state so toggling the button never disturbs the
  // tree/`meshfox:edge` connectors or their positions. Styled distinctly
  // (color + dash pattern) so a chain-of-computation arrow never reads as
  // just another nesting line.
  const depEdges: Edge[] = useMemo(() => {
    if (!showDeps) return [];
    return crossNodeDepEdges(blockGraph).map((e) => ({
      id: e.id,
      source: e.fromNodeId,
      target: e.toNodeId,
      label: `${e.fromBlock} → ${e.toBlock}`,
      type: "smoothstep",
      className: "mesh-dep-edge",
      markerEnd: { type: MarkerType.ArrowClosed, color: "var(--dep)" },
      selectable: false,
      deletable: false,
      zIndex: 1000,
    }));
  }, [blockGraph, showDeps]);

  const displayEdges = useMemo(() => [...edges, ...depEdges], [edges, depEdges]);

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
      // Dragging a group has nothing of its own to persist (its box is
      // always re-derived from its members, see layout.rs) — so instead,
      // moving it translates every descendant in its subtree by the same
      // delta, turning "drag the group" into "drag all its members
      // together". Synthesized as ordinary position changes (same shape
      // React Flow itself emits) so they ride along through the exact same
      // touched/dirty/gesture-end bookkeeping below as a real drag would.
      const groupMoves = changes.filter(
        (c): c is NodePositionChange =>
          c.type === "position" &&
          c.position !== undefined &&
          nodes.find((n) => n.id === c.id)?.data.nodeType === "group",
      );
      let allChanges = changes;
      if (groupMoves.length > 0 && canvas) {
        const extra: NodePositionChange[] = [];
        for (const move of groupMoves) {
          const group = nodes.find((n) => n.id === move.id);
          if (!group || !move.position) continue;
          const dx = move.position.x - group.position.x;
          const dy = move.position.y - group.position.y;
          if (dx === 0 && dy === 0) continue;
          for (const memberId of subtreeIds(canvas, move.id)) {
            const member = nodes.find((n) => n.id === memberId);
            if (!member) continue;
            extra.push({
              type: "position",
              id: memberId,
              position: { x: member.position.x + dx, y: member.position.y + dy },
              dragging: move.dragging,
            });
          }
        }
        allChanges = [...changes, ...extra];
      }
      onNodesChange(allChanges);
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
      for (const c of allChanges) {
        if (isUserDriven(c) && "id" in c) {
          touchedNodeIds.current.add(c.id);
          touched = true;
        }
        if (isGestureEnd(c)) layoutGestureEnded.current = true;
      }
      if (touched) setDirty(true);
    },
    [onNodesChange, nodes, canvas],
  );

  // Intercepts a "remove" change (Backspace/Delete with an edge selected)
  // for either deletable-edge kind, persisting server-side instead of
  // forwarding it to React Flow's own local state — the subsequent canvas
  // reload is what actually drops it from `edges`, keeping the file the
  // single source of truth. A structural edge with nothing to promote in
  // its place never generates a "remove" change at all (`deletable: false`,
  // set above), so nothing needs filtering for those.
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
    // Group boxes are always derived (see layoutGroups) — never treat them
    // as authored data to persist. Nor a node that's still showing its
    // server-suggested position/size and that the user hasn't actually
    // touched — dragging one node shouldn't silently pin every other,
    // still-auto-placed node's suggested box into the file as if it were
    // real authored data (see `touchedNodeIds`).
    const layout = new Map(
      nodes
        .filter(
          (n) => n.data.nodeType !== "group" && (!n.data.suggested || touchedNodeIds.current.has(n.id)),
        )
        .map((n) => [n.id, { x: n.position.x, y: n.position.y, width: n.width, height: n.height }]),
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
        <button
          className={showDeps ? "deps-toggle deps-toggle-active" : "deps-toggle"}
          onClick={() => setShowDeps((s) => !s)}
          title="Show/hide code-block dependencies (deps=): cross-node arrows and each node's in-body rail"
        >
          ⛓ {showDeps ? "hide deps" : "show deps"}
        </button>
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
      </div>
    ),
    [editMode, sourceMode, sourceDirty, dirty, error, showDeps, constraintStats, hasConfigurableVars, handleConfigure],
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
            onSaved={handleSourceSaved}
            onClose={() => setSourceMode(false)}
            onDirtyChange={setSourceDirty}
          />
        ) : (
          <ReactFlow
            nodes={nodes}
            edges={displayEdges}
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
            colorMode="system"
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
      {expandedNode && <NodeExpandPanel node={expandedNode} onClose={() => setExpandedNodeId(null)} />}
      {settingsNode && (
        <NodeSettings
          // Keyed on id so a successful id rename remounts this modal fresh
          // (new `node.id`/`node.title` as the starting point for every
          // field, including NodeSettings' own id-suggestion tracking)
          // rather than reusing stale local state under the old id.
          key={settingsNode.id}
          node={settingsNode}
          allNodes={canvas?.nodes ?? []}
          onChange={(patch) => handleNodeSettingsChange(settingsNode.id, patch)}
          onRenameId={handleNodeIdChange}
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
