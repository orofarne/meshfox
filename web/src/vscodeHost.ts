/**
 * Whether this page is currently running as a real VS Code webview
 * (`editors/vscode/src/canvasHtml.ts` injects the `<meta name="meshfox-
 * host">` tag this checks — a plain browser tab, including "meshfox: Open
 * in Browser", never has it) — `false` everywhere else. Read once at
 * module load: which host this document is depends on how it was served,
 * never changes over the page's lifetime.
 *
 * Exists specifically for `textPasteFallback.ts`'s companion right-click
 * handling (TODO.canvas.md: "VSCode: вставка текста (Cmd+V и контекстное
 * меню) в редактор ноды не работает") — confirmed directly, in a real VS
 * Code window, that the native OS/Electron context menu that appears for
 * Monaco's `EditContext` input surface there (`.native-edit-context`) does
 * show Cut/Copy/Paste, but its Paste item is exactly as broken as a native
 * Ctrl/Cmd+V is (same underlying focus-resolution gap through VS Code's
 * own nested webview iframe — not something `data-vscode-context` on the
 * editor's container, tried first, changed at all). A plain browser tab's
 * own native context-menu Paste already works fine there (confirmed
 * directly, same investigation) — this flag is what keeps the fix scoped
 * to only the environment that actually needs it, rather than replacing a
 * context menu that isn't broken everywhere else.
 */
export const isVSCodeHost: boolean = document.querySelector('meta[name="meshfox-host"]')?.getAttribute("content") === "vscode";
