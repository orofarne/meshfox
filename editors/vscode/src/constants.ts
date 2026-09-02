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
