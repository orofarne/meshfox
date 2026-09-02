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

export type WorkerMessage = ReadyMessage | OpenMessage;

export function parseWorkerMessage(line: string): WorkerMessage | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    return undefined;
  }
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    !("op" in parsed) ||
    !("canvas_path" in parsed)
  ) {
    return undefined;
  }
  const msg = parsed as { op: unknown; canvas_path: unknown };
  if (typeof msg.canvas_path !== "string") {
    return undefined;
  }
  if (msg.op === "ready" && typeof (parsed as ReadyMessage).port === "number") {
    return parsed as ReadyMessage;
  }
  if (msg.op === "open") {
    return parsed as OpenMessage;
  }
  return undefined;
}
