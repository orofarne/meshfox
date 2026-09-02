import * as vscode from "vscode";
import * as net from "net";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { ChildProcess, spawn } from "child_process";
import { parseWorkerMessage } from "./protocol";
import { VIEW_TYPE } from "./constants";

const READY_TIMEOUT_MS = 15000;

interface WorkerEntry {
  proc: ChildProcess;
  port?: number;
  waiters: Array<{ resolve: (port: number) => void; reject: (err: Error) => void }>;
}

/** Resolves symlinks the same way `std::fs::canonicalize` does on the Rust
 * side, so a path reported back over the socket (see `protocol.ts`'s own
 * doc comment) matches the key this map was spawned under. Falls back to a
 * plain absolute path if the file doesn't exist yet (e.g. a canonicalize
 * race right as a worker is starting) rather than throwing. */
function canonical(p: string): string {
  try {
    return fs.realpathSync.native(p);
  } catch {
    return path.resolve(p);
  }
}

/**
 * Acts as the coordinator role `crates/cli/src/watcher.rs` documents —
 * reimplemented here in TypeScript per `meshfox_server::watcher_protocol`'s
 * own stated design goal (a plain, language-agnostic wire protocol so
 * *something else* can stand in for meshfox's own watcher). One instance
 * per VS Code window: binds a private Unix socket, spawns one `meshfox
 * view --watcher-socket <that socket>` worker per open canvas, and tracks
 * each worker's port once its own `Ready` message arrives.
 *
 * Unix-socket only — same known gap as meshfox's own watcher (see
 * TODO.canvas.md's "Полноценная поддержка Windows"), so `start()` refuses
 * outright on `win32` rather than failing obscurely later.
 */
export class Coordinator implements vscode.Disposable {
  private readonly socketDir: string;
  private readonly socketPath: string;
  private server?: net.Server;
  private readonly workers = new Map<string, WorkerEntry>();
  private readonly pendingFragments = new Map<string, string | undefined>();

  constructor(private readonly output: vscode.OutputChannel) {
    this.socketDir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-vscode-"));
    this.socketPath = path.join(this.socketDir, "coordinator.sock");
  }

  async start(): Promise<void> {
    if (process.platform === "win32") {
      throw new Error(
        "meshfox: Windows isn't supported yet — the watcher-socket protocol this " +
          "extension speaks to meshfox is Unix-socket only for now (tracked in the " +
          "meshfox repo's TODO as \"Полноценная поддержка Windows\")."
      );
    }
    await new Promise<void>((resolve, reject) => {
      const server = net.createServer((socket) => this.handleConnection(socket));
      server.once("error", reject);
      server.listen(this.socketPath, () => {
        server.removeListener("error", reject);
        server.on("error", (err) => this.output.appendLine(`meshfox: coordinator socket error: ${err.message}`));
        resolve();
      });
      this.server = server;
    });
  }

  private handleConnection(socket: net.Socket): void {
    let buf = "";
    socket.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      let idx: number;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx);
        buf = buf.slice(idx + 1);
        if (line.trim().length > 0) {
          this.handleLine(line);
        }
      }
    });
    socket.on("error", () => {
      // A worker's own `send` is fire-and-forget (see
      // `watcher_protocol.rs`'s doc comment) — nothing to do here.
    });
  }

  private handleLine(line: string): void {
    const msg = parseWorkerMessage(line);
    if (!msg) {
      this.output.appendLine(`meshfox: coordinator got an unparseable message: ${line}`);
      return;
    }
    const key = canonical(msg.canvas_path);
    if (msg.op === "ready") {
      const entry = this.workers.get(key);
      if (!entry) {
        return;
      }
      entry.port = msg.port;
      const waiters = entry.waiters.splice(0);
      waiters.forEach((w) => w.resolve(msg.port));
    } else if (msg.op === "open") {
      this.pendingFragments.set(key, msg.fragment ?? undefined);
      vscode.commands.executeCommand("vscode.openWith", vscode.Uri.file(msg.canvas_path), VIEW_TYPE);
    }
  }

  /** Consumed once by the editor provider right after a worker for
   * `fsPath` becomes ready — carries a cross-canvas `Open`'s own deep-link
   * fragment (if any) to whichever `resolveCustomEditor` call ends up
   * serving it, mirroring `Entry.pending_open` in `watcher.rs`. */
  consumePendingFragment(fsPath: string): string | undefined {
    const key = canonical(fsPath);
    const fragment = this.pendingFragments.get(key);
    this.pendingFragments.delete(key);
    return fragment;
  }

  /** Returns the port an already-running (or freshly spawned) worker for
   * `fsPath` is serving on, waiting for its `Ready` message if needed. */
  async getOrSpawnWorker(fsPath: string): Promise<number> {
    const key = canonical(fsPath);
    let entry = this.workers.get(key);
    if (entry?.port !== undefined) {
      return entry.port;
    }
    if (!entry) {
      const exe = vscode.workspace.getConfiguration("meshfox").get<string>("executablePath", "meshfox");
      const proc = spawn(exe, ["view", fsPath, "--port", "0", "--watcher-socket", this.socketPath], {
        stdio: ["ignore", "pipe", "pipe"],
      });
      entry = { proc, waiters: [] };
      this.workers.set(key, entry);
      proc.stderr?.on("data", (chunk) => this.output.append(chunk.toString("utf8")));
      proc.on("error", (err) => {
        this.output.appendLine(`meshfox: failed to launch "${exe} view": ${err.message}`);
        const waiters = entry!.waiters.splice(0);
        waiters.forEach((w) => w.reject(err));
        this.workers.delete(key);
      });
      proc.on("exit", (code, signal) => {
        if (entry!.port === undefined) {
          const err = new Error(
            `meshfox view exited before reporting readiness (code=${code}, signal=${signal}) — ` +
              "check the \"meshfox\" output channel."
          );
          const waiters = entry!.waiters.splice(0);
          waiters.forEach((w) => w.reject(err));
        }
        this.workers.delete(key);
      });
    }
    return new Promise<number>((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(new Error(`meshfox view did not report readiness within ${READY_TIMEOUT_MS / 1000}s`));
      }, READY_TIMEOUT_MS);
      entry!.waiters.push({
        resolve: (port) => {
          clearTimeout(timer);
          resolve(port);
        },
        reject: (err) => {
          clearTimeout(timer);
          reject(err);
        },
      });
    });
  }

  /** Called when the webview tab for `fsPath` is closed — kills its
   * worker right away instead of waiting on the server's own auto-exit
   * grace period (`TabGuard` in `crates/server/src/lib.rs`), which still
   * applies as a fallback if this never runs (e.g. the extension host
   * itself is killed). */
  killWorker(fsPath: string): void {
    const key = canonical(fsPath);
    const entry = this.workers.get(key);
    if (entry) {
      entry.proc.kill();
      this.workers.delete(key);
    }
  }

  dispose(): void {
    for (const entry of this.workers.values()) {
      entry.proc.kill();
    }
    this.workers.clear();
    this.server?.close();
    try {
      fs.rmSync(this.socketDir, { recursive: true, force: true });
    } catch {
      // Best-effort — a leftover temp dir under `os.tmpdir()` isn't worth
      // failing deactivation over.
    }
  }
}
