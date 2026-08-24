/**
 * Light/dark theme preference — a manual override on top of the OS-level
 * `prefers-color-scheme` this app already followed everywhere (index.css's
 * `@media` block, `<ReactFlow colorMode>`, `usePrefersDark`'s Monaco/
 * `TtyPanel` consumers). `"system"` (the default) keeps following the OS;
 * `"light"`/`"dark"` pins it regardless of what the OS says.
 */
export type ThemePreference = "system" | "light" | "dark";

const STORAGE_KEY = "meshfox:theme";

/** Fired on `window` after `applyThemePreference` changes `data-theme` (or
 * the OS preference does, indirectly, while set to `"system"`) — the hook
 * side, `usePrefersDark` (see NodeTextEditor.tsx), listens for this to
 * re-render without needing a shared React context. */
export const THEME_CHANGE_EVENT = "meshfox:theme-change";

export function getThemePreference(): ThemePreference {
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" ? stored : "system";
}

/** Stamps `document.documentElement`'s `data-theme` to match `pref` — see
 * index.css's `:root[data-theme="dark"]`/`:root[data-theme="light"]`
 * overrides, which outrank the plain `:root`/`@media` defaults regardless
 * of source order (an attribute selector is more specific). `"system"`
 * clears the attribute outright, falling back to the OS media query.
 * Safe to call before React mounts (see main.tsx) so there's no
 * flash-of-wrong-theme on load. */
export function applyThemePreference(pref: ThemePreference): void {
  if (pref === "system") {
    delete document.documentElement.dataset.theme;
  } else {
    document.documentElement.dataset.theme = pref;
  }
  window.dispatchEvent(new Event(THEME_CHANGE_EVENT));
}

/** Persists `pref` (or clears the override for `"system"`) and applies it
 * immediately — the toolbar toggle's click handler. */
export function setThemePreference(pref: ThemePreference): void {
  if (pref === "system") {
    localStorage.removeItem(STORAGE_KEY);
  } else {
    localStorage.setItem(STORAGE_KEY, pref);
  }
  applyThemePreference(pref);
}
