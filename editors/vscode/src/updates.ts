import * as vscode from "vscode";
import { spawn } from "child_process";

const LAST_CHECK_KEY = "meshfox.lastUpdateCheckAt";
const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;
const REPO = "orofarne/meshfox";
const INSTALL_COMMAND = "curl -fsSL https://raw.githubusercontent.com/orofarne/meshfox/main/scripts/install.sh | sh";

/** Mirrors `check_updates`'s own `is_release_tag` check in
 * crates/cli/src/main.rs — a `v<digit>...` label means this build was cut
 * from a release tag and has a real version to compare; anything else
 * (`commit <hash> (<date>)`, a local/dev build) has nothing to compare
 * against, same as the CLI's own no-op case. */
async function installedVersionTag(exe: string): Promise<string | undefined> {
  const output = await runCapture(exe, ["--version"]);
  if (output === undefined) {
    return undefined;
  }
  // First line is "meshfox v1.2.3 (2026-08-15)" for a release build, or
  // "meshfox commit 4cd82d8bdac1 (2026-09-01)" (two words before the date)
  // for anything else — the capture group only matches the former shape.
  const match = output.split("\n")[0]?.match(/^meshfox\s+(v\d[^\s(]*)\s+\(/);
  return match?.[1];
}

export async function isExecutableAvailable(exe: string): Promise<boolean> {
  return (await runCapture(exe, ["--version"])) !== undefined;
}

function runCapture(exe: string, args: string[]): Promise<string | undefined> {
  return new Promise((resolve) => {
    const proc = spawn(exe, args, { stdio: ["ignore", "pipe", "ignore"] });
    let out = "";
    proc.stdout?.on("data", (chunk) => (out += chunk.toString("utf8")));
    proc.on("error", () => resolve(undefined));
    proc.on("close", (code) => resolve(code === 0 ? out : undefined));
  });
}

/** Numeric `vMAJOR.MINOR.PATCH` comparison — `self_update`'s own semver
 * compare on the Rust side, reimplemented just enough to answer "is a
 * newer tag available" without needing a semver dependency for three
 * numbers. Returns > 0 if `a` is newer than `b`. */
function compareVersionTags(a: string, b: string): number {
  const parse = (v: string) =>
    v
      .replace(/^v/, "")
      .split(".")
      .map((n) => parseInt(n, 10) || 0);
  const [aParts, bParts] = [parse(a), parse(b)];
  for (let i = 0; i < Math.max(aParts.length, bParts.length); i++) {
    const diff = (aParts[i] ?? 0) - (bParts[i] ?? 0);
    if (diff !== 0) {
      return diff;
    }
  }
  return 0;
}

/** Just a GitHub API read — never touches the installed binary. Actually
 * downloading/installing an update always goes through `meshfox
 * check-updates -y` (see `runInteractiveUpdate`), reusing its own tested
 * download/replace logic (`self_update`) rather than reimplementing that
 * riskier half here. */
async function latestReleaseTag(): Promise<string | undefined> {
  const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
    headers: { Accept: "application/vnd.github+json", "User-Agent": "meshfox-vscode" },
  });
  if (!res.ok) {
    return undefined;
  }
  const body = (await res.json()) as { tag_name?: string };
  return body.tag_name;
}

/** Throttled to once per `CHECK_INTERVAL_MS` (persisted in `globalState`,
 * so it survives across VS Code restarts, not just this session) and
 * purely informational — never installs anything on its own. Silent on
 * any failure (offline, rate-limited, dev build with nothing to compare):
 * this runs unprompted on every activation, so it must never surface an
 * error toast for something as ordinary as "no network right now". */
export async function maybeCheckForUpdatesOnStartup(
  context: vscode.ExtensionContext,
  exe: string,
  output: vscode.OutputChannel
): Promise<void> {
  const lastCheck = context.globalState.get<number>(LAST_CHECK_KEY, 0);
  if (Date.now() - lastCheck < CHECK_INTERVAL_MS) {
    return;
  }
  await context.globalState.update(LAST_CHECK_KEY, Date.now());

  try {
    const current = await installedVersionTag(exe);
    if (!current) {
      return;
    }
    const latest = await latestReleaseTag();
    if (!latest || compareVersionTags(latest, current) <= 0) {
      return;
    }
    const choice = await vscode.window.showInformationMessage(
      `meshfox ${latest} is available (you have ${current}).`,
      "Update",
      "Later"
    );
    if (choice === "Update") {
      await runInteractiveUpdate(exe, output);
    }
  } catch (err) {
    output.appendLine(`meshfox: startup update check failed: ${err instanceof Error ? err.message : String(err)}`);
  }
}

/** The actual download-and-replace path, for both the startup prompt's
 * own "Update" button and the standalone "meshfox: Check for Updates"
 * command — always behind an explicit VS Code confirmation first, since
 * `meshfox check-updates -y` itself skips its own interactive confirm
 * (`--yes` is required here in the first place: this is spawned with no
 * TTY on stdin, and the CLI refuses to prompt without one — see
 * `check_updates` in crates/cli/src/main.rs). */
export async function runInteractiveUpdate(exe: string, output: vscode.OutputChannel): Promise<void> {
  const confirmed = await vscode.window.showWarningMessage(
    "This checks github.com/orofarne/meshfox's releases and, if a newer one exists, downloads it and replaces " +
      "the meshfox binary in place. Continue?",
    { modal: true },
    "Check & Update"
  );
  if (confirmed !== "Check & Update") {
    return;
  }

  output.show(true);
  output.appendLine(`$ ${exe} check-updates -y`);
  const result = await runCapture(exe, ["check-updates", "-y"]);
  if (result === undefined) {
    vscode.window.showErrorMessage(`meshfox: update check failed — see the "meshfox" output channel.`);
    return;
  }
  output.append(result);
  const summary = result.trim().split("\n").pop() ?? result.trim();
  vscode.window.showInformationMessage(summary || "meshfox: update check finished.");
}

/** Reused across calls the same way VS Code's own built-in terminal-backed
 * tasks do — a second "meshfox: Install" while the first one's terminal
 * tab is still open just refocuses and re-sends into it, rather than
 * piling up a fresh terminal per click. `exitStatus` is `undefined` for as
 * long as the terminal itself is still open (whether or not a command is
 * running inside it), so that alone is enough to tell "reuse" from "the
 * user already closed that tab, make a new one" apart. */
let installTerminal: vscode.Terminal | undefined;

/** The install one-liner the root README documents, typed into a fresh (or
 * reused) integrated terminal but *not* submitted — `sendText`'s own
 * `addNewLine: false` leaves the actual Enter to the user. Same trust
 * boundary the old clipboard-only version drew, just one clipboard-paste
 * step shorter: this extension still never pipes a downloaded script into
 * a shell on someone's behalf without them explicitly choosing to run it
 * themselves. Backs both the standalone "meshfox: Install" command and
 * `showInstallInstructions`'s own prompt. */
export function openInstallTerminal(): void {
  if (!installTerminal || installTerminal.exitStatus !== undefined) {
    installTerminal = vscode.window.createTerminal("meshfox install");
  }
  installTerminal.show();
  installTerminal.sendText(INSTALL_COMMAND, false);
}

/** Shown wherever spawning the configured `meshfox` executable fails with
 * ENOENT. */
export async function showInstallInstructions(exe: string): Promise<void> {
  const choice = await vscode.window.showErrorMessage(
    `meshfox: couldn't find "${exe}" — install it, or set "meshfox.executablePath" if it's installed somewhere ` +
      "not on PATH.",
    "Install"
  );
  if (choice === "Install") {
    openInstallTerminal();
  }
}
