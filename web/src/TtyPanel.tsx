import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { usePrefersDark } from "./NodeTextEditor";
import { killRun } from "./api";

/** Mirrors `crates/server/src/lib.rs`'s `RunEvent` (JSON shape, camelCase)
 * as seen over `/api/run/tty`'s WebSocket — a superset of `./api.ts`'s
 * plain `RunEvent`: only this channel ever emits `tty-start`, right after
 * a `tty` step's own `step-start`, marking the point from which every
 * further WebSocket frame is raw pty I/O rather than a `RunEvent` (binary
 * frames both ways, and a text frame is a resize control message, not a
 * `RunEvent`) — until that step's own `step-end`. See SPEC.md's
 * "Interactive (`tty`) blocks". */
type TtyRunEvent =
  | { type: "started"; runId: string }
  | { type: "step-start"; nodeId: string; block: string }
  | { type: "output"; nodeId: string; block: string; text: string }
  | { type: "tty-start"; nodeId: string; block: string }
  | { type: "step-end"; nodeId: string; block: string; exitCode: number }
  | { type: "killed"; nodeId: string; block: string }
  | { type: "error"; message: string }
  | { type: "done"; exitCode: number };

type Status = "connecting" | "running" | "tty" | "exited" | "killed" | "error" | "closed";

interface TtyPanelProps {
  /** Node-id path from the root down to the node owning `blockName` — same
   * shape `pathTo` produces, joined with commas for the query string. */
  path: string[];
  blockName: string;
  withDeps: boolean;
  /** Whether a `cache`d step earlier in the chain (before the `tty` step
   * itself, which can never be `cache`d — see SPEC.md) should actually
   * persist its output — the Edit-mode flag, same role `RunRequest.persist`
   * plays for `/api/run`. */
  persist: boolean;
  vars?: Record<string, string>;
  onClose: () => void;
}

function statusLabel(status: Status, exitCode: number | undefined, errorMsg: string | undefined): string {
  switch (status) {
    case "connecting":
      return "connecting…";
    case "running":
      return "starting…";
    case "tty":
      return "interactive";
    case "exited":
      return `exited (${exitCode})`;
    case "killed":
      return "killed";
    case "error":
      return errorMsg ?? "error";
    case "closed":
      return "disconnected";
  }
}

/**
 * A `tty` block's run panel: a real interactive terminal (`xterm.js`)
 * wired to `/api/run/tty`'s WebSocket, rendered via a portal — the same
 * fixed-overlay-over-everything approach `NodeTextEditor` uses, and for the
 * same reason (a node's own box is nowhere near big enough, and is at the
 * mercy of the canvas's current pan/zoom). Unlike `NodeTextEditor`, this
 * also supports collapsing into a small corner pill without ending the
 * session — minimizing only ever changes CSS (`visibility`, not `display`,
 * and never a `fit()`/resize), so a full-screen program running inside
 * (`vim`, `htop`, an interactive shell) never sees its terminal size change
 * just because the panel was tucked out of the way; only actually resizing
 * the *expanded* panel (or the window) does that.
 *
 * One WebSocket per mount, for the panel's whole lifetime — closing it
 * (however: the × button, or just unmounting) closes the socket, which the
 * server reads as "client gone" and kills whatever's still running (see
 * `pty_exec::PtyProcess::kill`), same as closing a real terminal window.
 */
export function TtyPanel({ path, blockName, withDeps, persist, vars, onClose }: TtyPanelProps) {
  const dark = usePrefersDark();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const runIdRef = useRef<string | undefined>(undefined);
  const ttyActiveRef = useRef(false);

  const [status, setStatus] = useState<Status>("connecting");
  const [exitCode, setExitCode] = useState<number | undefined>(undefined);
  const [errorMsg, setErrorMsg] = useState<string | undefined>(undefined);
  const [activeBlock, setActiveBlock] = useState(blockName);
  const [collapsed, setCollapsed] = useState(false);
  const [canKill, setCanKill] = useState(false);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      convertEol: true,
      fontFamily: '"Fira Code", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
      fontSize: 13,
      cursorBlink: true,
      theme: dark
        ? { background: "#16171a", foreground: "#e8e8e8" }
        : { background: "#1a1a1a", foreground: "#e8e8e8" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    fit.fit();
    termRef.current = term;

    const params = new URLSearchParams({
      path: path.join(","),
      block: blockName,
      noDeps: String(!withDeps),
      persist: String(persist),
      vars: JSON.stringify(vars ?? {}),
      cols: String(term.cols),
      rows: String(term.rows),
    });
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(`${proto}//${location.host}/api/run/tty?${params}`);
    ws.binaryType = "arraybuffer";
    wsRef.current = ws;

    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        const event = JSON.parse(ev.data) as TtyRunEvent;
        switch (event.type) {
          case "started":
            runIdRef.current = event.runId;
            setCanKill(true);
            break;
          case "step-start":
            setActiveBlock(event.block);
            setStatus("running");
            term.write(`\x1b[2m── ${event.block} ──\x1b[0m\r\n`);
            break;
          case "tty-start":
            ttyActiveRef.current = true;
            setStatus("tty");
            break;
          case "output":
            // A captured (non-`tty`) step earlier in the chain — printed
            // into the same terminal rather than a separate log widget, so
            // the whole chain reads as one continuous session.
            term.write(event.text.replace(/\n/g, "\r\n") + "\r\n");
            break;
          case "step-end":
            if (ttyActiveRef.current) {
              ttyActiveRef.current = false;
              setCanKill(false);
              setStatus("exited");
              setExitCode(event.exitCode);
            } else {
              term.write(`\x1b[2m(exit ${event.exitCode})\x1b[0m\r\n`);
            }
            break;
          case "killed":
            ttyActiveRef.current = false;
            setCanKill(false);
            setStatus("killed");
            break;
          case "error":
            ttyActiveRef.current = false;
            setCanKill(false);
            setStatus("error");
            setErrorMsg(event.message);
            break;
          case "done":
            break;
        }
      } else {
        term.write(new Uint8Array(ev.data as ArrayBuffer));
      }
    };
    ws.onerror = () => {
      setCanKill(false);
      setStatus((s) => (s === "exited" || s === "killed" ? s : "error"));
    };
    ws.onclose = () => {
      ttyActiveRef.current = false;
      setCanKill(false);
      setStatus((s) => (s === "exited" || s === "killed" || s === "error" ? s : "closed"));
    };

    const dataSub = term.onData((data) => {
      if (ttyActiveRef.current && ws.readyState === WebSocket.OPEN) {
        ws.send(new TextEncoder().encode(data));
      }
    });

    // Only actually resizes the pty while expanded — `.fit()` is never
    // called while collapsed (see the component doc comment), since the
    // collapsed pill doesn't change this container's real layout size at
    // all, only its visibility.
    const resizeObserver = new ResizeObserver(() => {
      fit.fit();
      if (ttyActiveRef.current && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ cols: term.cols, rows: term.rows }));
      }
    });
    resizeObserver.observe(container);

    return () => {
      resizeObserver.disconnect();
      dataSub.dispose();
      ws.close();
      term.dispose();
    };
    // Mount-once: `path`/`blockName`/`withDeps`/`persist`/`vars` address
    // exactly the one run this panel was opened for — a real change to any
    // of them means a different run, which means a fresh `TtyPanel` (a new
    // `key` from the caller), not a live-reconnect of this one.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleKill = () => {
    if (runIdRef.current) killRun(runIdRef.current).catch(() => {});
  };
  const handleClose = () => {
    wsRef.current?.close();
    onClose();
  };

  return createPortal(
    // Unlike NodeTextEditor's backdrop, a click here never closes the
    // panel — this session may still be running something interactive, and
    // closing it kills the process (see `handleClose`'s doc comment on
    // `TtyPanel`'s own JSDoc above); only the explicit × button does that.
    <div className={`mesh-tty-backdrop${collapsed ? " collapsed" : ""}`}>
      <div className="mesh-tty-panel">
        <div className="mesh-tty-head">
          <span className="mesh-tty-head-title">▶ {activeBlock}</span>
          <span className="mesh-tty-head-status" data-status={status}>
            {statusLabel(status, exitCode, errorMsg)}
          </span>
          <span className="mesh-tty-head-actions">
            {canKill && (
              <button type="button" onClick={handleKill} title="Kill this session">
                ⏹ kill
              </button>
            )}
            <button type="button" onClick={() => setCollapsed(true)} title="Minimize — session keeps running">
              _
            </button>
            <button type="button" onClick={handleClose} title="Close (ends the session if still running)">
              ✕
            </button>
          </span>
        </div>
        <div className="mesh-tty-body" ref={containerRef} />
      </div>
      {collapsed && (
        <button type="button" className="mesh-tty-pill" onClick={() => setCollapsed(false)}>
          ▶ {activeBlock} · {statusLabel(status, exitCode, errorMsg)}
        </button>
      )}
    </div>,
    document.body,
  );
}
