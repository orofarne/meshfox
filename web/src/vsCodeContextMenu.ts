import type * as MonacoNS from "monaco-editor";
import { isVSCodeHost } from "./vscodeHost";

const MENU_CLASS = "mesh-vscode-context-menu";

function removeMenu() {
  document.querySelector(`.${MENU_CLASS}`)?.remove();
}

interface MenuItemSpec {
  label: string;
  shortcut: string;
  enabled: boolean;
  onClick: () => void;
}

/** A tiny, bare-bones popup — not a real menu component — styled off VS
 * Code's own webview CSS custom properties so it at least looks native-
 * adjacent rather than generic, without needing this app's own theme
 * plumbing threaded in. Mirrors the shape (Cut/Copy/Paste, shortcut hints
 * right-aligned) of the native macOS menu it replaces here, confirmed by a
 * screenshot of that exact menu. */
function showMenu(x: number, y: number, items: MenuItemSpec[]) {
  removeMenu();
  const menu = document.createElement("div");
  menu.className = MENU_CLASS;
  Object.assign(menu.style, {
    position: "fixed",
    left: `${x}px`,
    top: `${y}px`,
    zIndex: "100000",
    background: "var(--vscode-menu-background, #ffffff)",
    color: "var(--vscode-menu-foreground, #1e1e1e)",
    border: "1px solid var(--vscode-menu-border, rgba(0,0,0,0.15))",
    borderRadius: "5px",
    boxShadow: "0 2px 8px rgba(0,0,0,0.3)",
    padding: "4px",
    fontSize: "13px",
    fontFamily: "var(--vscode-font-family, sans-serif)",
    minWidth: "140px",
  } satisfies Partial<CSSStyleDeclaration>);

  for (const spec of items) {
    const item = document.createElement("div");
    Object.assign(item.style, {
      display: "flex",
      justifyContent: "space-between",
      gap: "16px",
      padding: "4px 10px",
      borderRadius: "3px",
      cursor: spec.enabled ? "pointer" : "default",
      opacity: spec.enabled ? "1" : "0.4",
    } satisfies Partial<CSSStyleDeclaration>);

    const label = document.createElement("span");
    label.textContent = spec.label;
    const shortcut = document.createElement("span");
    shortcut.textContent = spec.shortcut;
    shortcut.style.opacity = "0.7";
    item.append(label, shortcut);

    if (spec.enabled) {
      item.addEventListener("mouseenter", () => {
        item.style.background = "var(--vscode-menu-selectionBackground, #0078d4)";
        item.style.color = "var(--vscode-menu-selectionForeground, #ffffff)";
      });
      item.addEventListener("mouseleave", () => {
        item.style.background = "";
        item.style.color = "";
      });
      item.addEventListener("click", (e) => {
        e.stopPropagation();
        removeMenu();
        spec.onClick();
      });
    }
    menu.appendChild(item);
  }

  document.body.appendChild(menu);

  const dismiss = (e: Event) => {
    if (e instanceof MouseEvent && menu.contains(e.target as Node)) return;
    removeMenu();
    document.removeEventListener("mousedown", dismiss, true);
    document.removeEventListener("keydown", dismiss, true);
  };
  // Deferred one tick — the same right-click that opened this menu would
  // otherwise immediately dismiss it (its own "mousedown" hasn't happened
  // yet at the point `showMenu` runs, from a "contextmenu" listener, but a
  // stray follow-up click shouldn't close it before it's even visible).
  setTimeout(() => {
    document.addEventListener("mousedown", dismiss, true);
    document.addEventListener("keydown", dismiss, true);
  }, 0);
}

/**
 * Replaces the right-click menu Monaco's `EditContext` input surface gets
 * in a real VS Code window — confirmed directly (a real VS Code window,
 * `editors/vscode/e2e/`) that it's the OS's/Electron's own native
 * Cut/Copy/Paste menu there (not a scripted one — `MONACO_OPTIONS`'
 * `contextmenu: false` already rules that out), and confirmed just as
 * directly that its Paste item doesn't work: the exact same underlying gap
 * a real Ctrl/Cmd+V hits (TODO.canvas.md: "VSCode: вставка текста... не
 * работает") — VS Code's own nested webview iframe not correctly
 * delivering a native paste command to this specific input surface,
 * regardless of what triggers it. `data-vscode-context` on the editor's
 * container (tried first, on the theory that VS Code's own webview-context-
 * menu contribution point might route around it) made no observable
 * difference.
 *
 * `event.preventDefault()` here does suppress that native menu — confirmed
 * directly, by hand, in a real VS Code window (only a real one could:
 * there's no automated way to observe a real native OS menu at all — a
 * real, physical right-click shows one; a Playwright/CDP-synthesized one
 * doesn't trigger the same path in the first place). Replacing the whole
 * menu rather than only its broken Paste item means Cut/Copy need
 * reimplementing here too, via `navigator.clipboard.writeText()` — the
 * native menu's own Cut/Copy presumably worked (never reported broken),
 * but there's no way to keep *only* the native Paste item disabled and
 * the rest native, so this is all-or-nothing once the native menu itself
 * is suppressed.
 *
 * Scoped to `isVSCodeHost` specifically — a plain browser tab's own native
 * menu already works fine end to end (confirmed directly,
 * `web/e2e/copy-paste.spec.ts`), so this only ever attaches where the
 * thing it works around is actually broken.
 */
export function attachVSCodeContextMenu(editor: MonacoNS.editor.IStandaloneCodeEditor): () => void {
  if (!isVSCodeHost) return () => {};
  const node = editor.getDomNode();
  if (!node) return () => {};

  const onContextMenu = (event: MouseEvent) => {
    if (!(event.target instanceof Node) || !node.contains(event.target)) return;
    event.preventDefault();

    const selection = editor.getSelection();
    const model = editor.getModel();
    const selectedText = selection && model ? model.getValueInRange(selection) : "";
    const hasSelection = selectedText.length > 0;

    const copySelection = () => navigator.clipboard.writeText(selectedText).catch(() => {});

    showMenu(event.clientX, event.clientY, [
      {
        label: "Cut",
        shortcut: "⌘X",
        enabled: hasSelection,
        onClick: () => {
          copySelection();
          if (selection) {
            editor.executeEdits("vscode-context-menu-cut", [{ range: selection, text: "", forceMoveMarkers: true }]);
          }
          editor.focus();
        },
      },
      {
        label: "Copy",
        shortcut: "⌘C",
        enabled: hasSelection,
        onClick: () => {
          copySelection();
          editor.focus();
        },
      },
      {
        label: "Paste",
        shortcut: "⌘V",
        enabled: true,
        onClick: () => {
          navigator.clipboard.readText().then(
            (text) => {
              if (!text) return;
              const pasteSelection = editor.getSelection();
              if (!pasteSelection) return;
              editor.executeEdits("vscode-context-menu-paste", [
                { range: pasteSelection, text, forceMoveMarkers: true },
              ]);
              editor.focus();
            },
            () => {
              // clipboard-read unavailable/denied — nothing more to try.
            },
          );
        },
      },
    ]);
  };

  document.addEventListener("contextmenu", onContextMenu, true);
  return () => {
    document.removeEventListener("contextmenu", onContextMenu, true);
    removeMenu();
  };
}
