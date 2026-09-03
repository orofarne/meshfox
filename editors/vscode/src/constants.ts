import * as vscode from "vscode";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { isExecutableAvailable } from "./updates";

// Must match the `viewType`s a `resolveCustomEditor`/`openCustomDocument`
// provider is registered under in package.json's `contributes.customEditors`.
// `VIEW_TYPE` is the *default* editor for `*.canvas.md` — auto-opens on
// double-click. `VIEW_TYPE_ANY` is the same provider registered again for
// `*.md` broadly, at `priority: "option"` — never auto-opens, just adds
// "meshfox canvas" to a plain `.md` file's own "Open With..." picker (e.g.
// for a marker-carrying file without the suffix, like this project's own
// README.md). See `canvasEditorProvider.ts`'s own doc comment for why
// there's no content-sniffing default-then-bail-out here instead.
export const VIEW_TYPE = "meshfox.canvasEditor";
export const VIEW_TYPE_ANY = "meshfox.canvasEditorAny";

/** `scripts/install.sh`'s own default (`$HOME/.local/bin`), plus the two
 * other directories a CLI tool commonly ends up in on macOS/Linux outside
 * of it. Only consulted as a fallback — see `resolveExecutablePath`. */
function defaultInstallCandidates(): string[] {
  return [
    path.join(os.homedir(), ".local", "bin", "meshfox"),
    "/opt/homebrew/bin/meshfox",
    "/usr/local/bin/meshfox",
  ];
}

/**
 * Resolves the `meshfox` binary to actually spawn. An explicit
 * `meshfox.executablePath` override (anything other than the bare
 * `"meshfox"` default) is trusted as-is — a user who set an absolute path
 * on purpose shouldn't have that second-guessed. For the default, plain
 * PATH resolution is tried first; if that fails, falls back to checking
 * `scripts/install.sh`'s own default install location and a couple of
 * other common ones directly.
 *
 * This exists because a GUI app launched from Finder/Dock/Spotlight
 * (rather than a shell) doesn't reliably inherit a login shell's PATH —
 * VS Code does try to resolve the shell environment itself on startup,
 * but that isn't 100% reliable either — so a perfectly real install at
 * the documented default location can otherwise still show as "not
 * found".
 */
export async function resolveExecutablePath(): Promise<string> {
  const configured = vscode.workspace.getConfiguration("meshfox").get<string>("executablePath", "meshfox");
  if (configured !== "meshfox") {
    return configured;
  }
  if (await isExecutableAvailable(configured)) {
    return configured;
  }
  for (const candidate of defaultInstallCandidates()) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return configured;
}
