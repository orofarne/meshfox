import * as vscode from "vscode";
import { Coordinator } from "./coordinator";
import { canvasAppHtml, errorHtml, loadingHtml } from "./canvasHtml";

const INDEX_FETCH_RETRIES = 5;
const INDEX_FETCH_RETRY_DELAY_MS = 200;

/**
 * A `.canvas.md` tab's own webview *is* the meshfox web UI the
 * `Coordinator` gets running for it — its `index.html`, fetched from the
 * worker and set as this panel's own document (see `canvasHtml.ts`'s own
 * doc comment for why not an `<iframe>`: nesting one was the actual cause
 * of copy/paste and the right-click context menu not working inside the
 * canvas, a long-standing upstream VS Code limitation specific to nested
 * cross-origin iframes in a webview, not to top-level webview content).
 * `CustomReadonlyEditorProvider` because meshfox's own file-sync/save
 * story already lives entirely in that server (the same "opens read-only,
 * click Edit to unlock" model the browser UI uses — see README.md's
 * "Concept"), so VS Code's own text-document model has nothing to do here.
 *
 * Registered under two `viewType`s (see `constants.ts` and package.json's
 * `contributes.customEditors`) that both resolve through this same class:
 * `VIEW_TYPE` auto-opens for `*.canvas.md`; `VIEW_TYPE_ANY` never
 * auto-opens, it just offers this editor as an "Open With..." choice for
 * any `.md`. No content-sniffing here on purpose — an earlier version
 * tried to guess "is this actually a canvas" and bail out to the built-in
 * text editor when it wasn't, registered as the default for `*.md`
 * broadly; disposing this panel mid-resolution to redirect corrupted VS
 * Code's own custom-editor bookkeeping for that resource (reproduced: it
 * threw "OverlayWebview has been disposed" on the *next* open of the same
 * file). Not needed anyway — `meshfox view` renders any Markdown as a
 * one-node canvas fine even without the `<!-- meshfox:canvas -->` marker
 * (see `meshfox_core::mdcanvas::has_marker`'s own doc comment: it's a
 * discovery hint, `parse` never requires it), so whichever `viewType` a
 * document was actually opened through, this always just works.
 */
export class CanvasEditorProvider implements vscode.CustomReadonlyEditorProvider {
  constructor(private readonly coordinator: Coordinator) {}

  async openCustomDocument(uri: vscode.Uri): Promise<vscode.CustomDocument> {
    return { uri, dispose: () => {} };
  }

  async resolveCustomEditor(document: vscode.CustomDocument, panel: vscode.WebviewPanel): Promise<void> {
    panel.webview.options = { enableScripts: true };
    panel.webview.html = loadingHtml();

    panel.onDidDispose(() => this.coordinator.killWorker(document.uri.fsPath));

    let port: number;
    try {
      port = await this.coordinator.getOrSpawnWorker(document.uri.fsPath);
    } catch (err) {
      panel.webview.html = errorHtml(err instanceof Error ? err.message : String(err));
      return;
    }

    const fragment = this.coordinator.consumePendingFragment(document.uri.fsPath);
    const external = await vscode.env.asExternalUri(vscode.Uri.parse(`http://127.0.0.1:${port}/`));
    const baseUrl = external.toString();

    let indexHtml: string;
    try {
      indexHtml = await fetchIndexHtml(baseUrl);
    } catch (err) {
      panel.webview.html = errorHtml(err instanceof Error ? err.message : String(err));
      return;
    }

    panel.webview.html = canvasAppHtml(indexHtml, baseUrl, fragment);
  }
}

/** A few quick retries against the freshly-`Ready` worker: its `Ready`
 * message fires right after `TcpListener::bind` succeeds (see
 * `watcher_protocol.rs`'s own doc comment), which doesn't strictly
 * guarantee the accept loop is already actively serving connections that
 * exact instant — a bare failed `fetch` here would otherwise surface as a
 * spurious error on an otherwise-healthy worker. */
async function fetchIndexHtml(baseUrl: string): Promise<string> {
  let lastErr: unknown;
  for (let attempt = 0; attempt < INDEX_FETCH_RETRIES; attempt++) {
    try {
      const res = await fetch(baseUrl);
      if (!res.ok) {
        throw new Error(`meshfox server responded ${res.status} ${res.statusText}`);
      }
      return await res.text();
    } catch (err) {
      lastErr = err;
      await new Promise((resolve) => setTimeout(resolve, INDEX_FETCH_RETRY_DELAY_MS));
    }
  }
  throw lastErr instanceof Error ? lastErr : new Error(String(lastErr));
}
