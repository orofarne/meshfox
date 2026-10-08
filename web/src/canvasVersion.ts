import type { CanvasDoc } from "./types";

/** One gate shared by HTTP reads and mutations. Retired worker sessions
 * cannot replace the current worker after a delayed response arrives. */
export function canvasVersionGate() {
  let session: string | undefined;
  let version = -1;
  const retired = new Set<string>();
  return (snapshot: CanvasDoc): boolean => {
    if (snapshot.serverSession === undefined || snapshot.canvasVersion === undefined) return session === undefined;
    if (retired.has(snapshot.serverSession)) return false;
    if (snapshot.serverSession !== session) {
      if (session !== undefined) retired.add(session);
      session = snapshot.serverSession;
      version = -1;
    }
    if (snapshot.canvasVersion < version) return false;
    version = snapshot.canvasVersion;
    return true;
  };
}
