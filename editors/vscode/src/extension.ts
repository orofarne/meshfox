import * as vscode from "vscode";
import { Coordinator } from "./coordinator";
import { CanvasEditorProvider } from "./canvasEditorProvider";
import { VIEW_TYPE, VIEW_TYPE_ANY, resolveExecutablePath } from "./constants";
import {
  isExecutableAvailable,
  maybeCheckForUpdatesOnStartup,
  runInteractiveUpdate,
  showInstallInstructions,
} from "./updates";

let coordinator: Coordinator | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const output = vscode.window.createOutputChannel("meshfox");
  context.subscriptions.push(output);

  coordinator = new Coordinator(output);
  context.subscriptions.push(coordinator);

  try {
    await coordinator.start();
  } catch (err) {
    vscode.window.showErrorMessage(`meshfox: ${err instanceof Error ? err.message : String(err)}`);
    return;
  }

  const provider = new CanvasEditorProvider(coordinator);
  const registrationOptions = {
    webviewOptions: { retainContextWhenHidden: true },
    supportsMultipleEditorsPerDocument: false,
  };
  context.subscriptions.push(
    vscode.window.registerCustomEditorProvider(VIEW_TYPE, provider, registrationOptions),
    vscode.window.registerCustomEditorProvider(VIEW_TYPE_ANY, provider, registrationOptions),
    vscode.commands.registerCommand("meshfox.openAsCanvas", async (uri?: vscode.Uri) => {
      const target = uri ?? vscode.window.activeTextEditor?.document.uri;
      if (!target) {
        vscode.window.showErrorMessage("meshfox: no file to open as a canvas — no argument and no active editor.");
        return;
      }
      await vscode.commands.executeCommand("vscode.openWith", target, VIEW_TYPE);
    }),
    vscode.commands.registerCommand("meshfox.checkForUpdates", async () => {
      const exe = resolveExecutablePath();
      if (!(await isExecutableAvailable(exe))) {
        await showInstallInstructions(exe);
        return;
      }
      await runInteractiveUpdate(exe, output);
    })
  );

  // Fire-and-forget: throttled to once a day (see updates.ts), purely
  // informational, never installs without an explicit click on the toast
  // it shows. Must never block or fail activation.
  void maybeCheckForUpdatesOnStartup(context, resolveExecutablePath(), output);
}

export function deactivate(): void {
  coordinator?.dispose();
  coordinator = undefined;
}
