// Mirrors `meshfox_server::watcher_protocol::Message` (crates/server/src/
// watcher_protocol.rs) exactly — a `meshfox view --watcher-socket <path>`
// worker speaks this to whoever is listening on `<path>`, as one
// newline-delimited JSON object per message. That module's own doc comment
// is explicit that the listener doesn't have to be meshfox's own Rust
// watcher; this extension is the "something else entirely" it anticipates.

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
