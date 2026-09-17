// Mirrors `meshfox_server::watcher_protocol::Message` (crates/server/src/
// watcher_protocol.rs) exactly — a `meshfox view --watcher-socket <path>`
// worker speaks this to whoever is listening on `<path>`, as one
// newline-delimited JSON object per message. That module's own doc comment
// is explicit that the listener doesn't have to be meshfox's own Rust
// watcher; this extension is the "something else entirely" it anticipates.
//
// `GetPort` is the odd one out: sent not by a worker but by this extension
// itself, to an *external* coordinator (see `config.ts`'s `resolveServerSocket`),
// asking it to get-or-spawn a worker without opening anything and just
// report the port back — unlike the three above, it gets a reply on the
// same connection. See `requestPort`, below.

import * as net from "net";

export interface ReadyMessage {
  op: "ready";
  /** Canonicalized absolute path, as `PathBuf` serializes it. */
  canvas_path: string;
  port: number;
}

export interface OpenMessage {
  op: "open";
  canvas_path: string;
  /** A deep link's own `#node-id`, or absent/null for the target's root. */
  fragment?: string | null;
}

/** A "↗ open" on a plain (non-canvas) file node's target — see
 * `watcher_protocol::Message::OpenFile`'s own doc comment for why this is
 * a separate variant from `OpenMessage` rather than a reused field on it. */
export interface OpenFileMessage {
  op: "open_file";
  path: string;
}

export type WorkerMessage = ReadyMessage | OpenMessage | OpenFileMessage;

export function parseWorkerMessage(line: string): WorkerMessage | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    return undefined;
  }
  if (typeof parsed !== "object" || parsed === null || !("op" in parsed)) {
    return undefined;
  }
  const msg = parsed as { op: unknown };
  if (
    msg.op === "ready" &&
    typeof (parsed as ReadyMessage).canvas_path === "string" &&
    typeof (parsed as ReadyMessage).port === "number"
  ) {
    return parsed as ReadyMessage;
  }
  if (msg.op === "open" && typeof (parsed as OpenMessage).canvas_path === "string") {
    return parsed as OpenMessage;
  }
  if (msg.op === "open_file" && typeof (parsed as OpenFileMessage).path === "string") {
    return parsed as OpenFileMessage;
  }
  return undefined;
}

const GET_PORT_TIMEOUT_MS = 15000;

/** [`Message::GetPort`] — get-or-spawn a worker for `canvasPath` (already
 * canonicalized by the caller — an external coordinator resolves a
 * relative path against *its own* cwd, not this extension's, so sending
 * one uncanonicalized would silently ask it to look in the wrong place)
 * at `socketPath`, without opening anything, and read back the one JSON
 * reply line a `get_port` request gets (unlike every other message here) —
 * `{"port": number}` on success, `{"error": string}` on failure. Mirrors
 * `meshfox_server::watcher_protocol::request_port` exactly, including its
 * "no response payload" failure modes: a connection error, a timeout, or a
 * coordinator-reported `error` field all reject the same way. */
export function requestPort(socketPath: string, canvasPath: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(socketPath);
    let buf = "";
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      socket.destroy();
      reject(new Error(`coordinator at ${socketPath} did not answer get_port within ${GET_PORT_TIMEOUT_MS / 1000}s`));
    }, GET_PORT_TIMEOUT_MS);
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      fn();
    };
    socket.on("connect", () => {
      socket.write(JSON.stringify({ op: "get_port", canvas_path: canvasPath }) + "\n");
    });
    socket.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      const idx = buf.indexOf("\n");
      if (idx < 0) return;
      const line = buf.slice(0, idx);
      let parsed: unknown;
      try {
        parsed = JSON.parse(line);
      } catch {
        finish(() => reject(new Error(`coordinator sent an unparseable get_port reply: ${line}`)));
        return;
      }
      const reply = parsed as { port?: unknown; error?: unknown };
      if (typeof reply.port === "number") {
        finish(() => resolve(reply.port as number));
      } else if (typeof reply.error === "string") {
        finish(() => reject(new Error(reply.error as string)));
      } else {
        finish(() => reject(new Error(`coordinator sent a malformed get_port reply: ${line}`)));
      }
    });
    socket.on("error", (err) => finish(() => reject(err)));
    socket.on("close", () => finish(() => reject(new Error("coordinator closed the connection without answering get_port"))));
  });
}
