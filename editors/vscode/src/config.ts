import * as fs from "fs";
import * as os from "os";
import * as path from "path";

/**
 * `server_socket` from `.meshfox/config.toml` — mirrors
 * `meshfox_core::config::server_socket`'s own local-wins-over-global load
 * (`<canvas_root>/.meshfox/config.toml` overrides `~/.meshfox/config.toml`)
 * exactly, but *not* its general deep-merge machinery: since this is the
 * only key any TypeScript code here reads, "merge" reduces to "prefer the
 * local file's value if it declares one, else the global file's" — there's
 * no second key that could ever need recursive merging. An explicit empty
 * local value disables the global socket for this workspace. `readTopLevelString`
 * below is a narrow, single-key reader for exactly this reason too — adding
 * a real TOML dependency (this extension has none at all today) for one
 * top-level string assignment isn't worth it; if a second config key ever
 * needs reading from here, that's the point to reconsider.
 */
export function resolveServerSocket(canvasPath: string): string | undefined {
  // Match meshfox_core::config::server_socket: an explicit empty override
  // disables the configured daemon for this process (including e2e runs).
  if (process.env.MESHFOX_SERVER_SOCKET !== undefined) {
    return process.env.MESHFOX_SERVER_SOCKET || undefined;
  }
  const canvasRoot = path.dirname(canvasPath);
  const local = readTopLevelString(path.join(canvasRoot, ".meshfox", "config.toml"), "server_socket");
  if (local !== undefined) {
    return local || undefined;
  }
  return readTopLevelString(path.join(os.homedir(), ".meshfox", "config.toml"), "server_socket");
}

/** `key = "value"` or `key = 'value'` at the top level of a TOML file (not
 * inside any `[section]`) — returns `undefined` if the file doesn't exist,
 * can't be read, or never assigns `key` outside a section. Deliberately
 * ignores anything past the first `=` that isn't a plain quoted string
 * (a bare/array/inline-table value) — `server_socket` is only ever a path
 * string, so there's nothing else worth handling here. */
function readTopLevelString(filePath: string, key: string): string | undefined {
  let contents: string;
  try {
    contents = fs.readFileSync(filePath, "utf8");
  } catch {
    return undefined;
  }
  let inTopLevel = true;
  for (const rawLine of contents.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line.length === 0 || line.startsWith("#")) {
      continue;
    }
    if (line.startsWith("[")) {
      inTopLevel = false;
      continue;
    }
    if (!inTopLevel) {
      continue;
    }
    const match = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.+)$/);
    if (!match || match[1] !== key) {
      continue;
    }
    const value = match[2].trim();
    const quote = value[0];
    if (quote !== '"' && quote !== "'") {
      continue; // not a plain quoted string — nothing this reader understands
    }
    const end = value.indexOf(quote, 1);
    if (end < 0) {
      continue;
    }
    return value.slice(1, end);
  }
  return undefined;
}
