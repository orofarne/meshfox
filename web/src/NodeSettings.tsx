import { useRef, useState } from "react";
import type { CanvasNode, ExtraEdgeDto, NodeType } from "./types";
import type { NodePatch } from "./api";
import { basicSlug, translitSlug } from "./slug";
import { TagEditor } from "./TagEditor";

/** Mirrors `crates/core/src/mdcanvas.rs::unique_slug`'s output shape: a
 * bare slug of `title` (falling back to `"node"` if the title has no
 * alphanumeric characters at all, same as the Rust side), optionally
 * followed by a `-<n>` de-dup suffix the server appends on collision —
 * e.g. two sibling nodes both titled "New Node" get `new-node` and
 * `new-node-2`. Used to tell whether an id "still looks auto-generated"
 * (never touched by hand) even when it happens to carry one of those
 * suffixes, which a plain `id === basicSlug(title)` check used to miss. */
function idMatchesAutoSlug(id: string, title: string): boolean {
  const base = basicSlug(title) || "node";
  if (id === base) return true;
  if (!id.startsWith(`${base}-`)) return false;
  const suffix = id.slice(base.length + 1);
  return suffix.length > 0 && /^[0-9]+$/.test(suffix);
}

interface NodeSettingsProps {
  node: CanvasNode;
  /** Every other node in the document, for the "add incoming edge" picker
   * (titles to show, ids to send) — excludes `node` itself. */
  allNodes: CanvasNode[];
  /** Commits every field except the id (see `onRenameId`) — fired once, by
   * the "ok" button, not per keystroke. Takes the node's *current* id
   * explicitly (rather than the caller closing over `node.id` itself):
   * `handleOk` below may have just renamed the node in the same click, and
   * a parent-side closure captured at render time goes stale the moment
   * that rename remounts this component under its new id — see `handleOk`'s
   * own comment. */
  onChange: (id: string, patch: NodePatch) => void;
  /** Renames the node's own id — a separate commit from `onChange`, unlike
   * every other field here: it needs a uniqueness check and rewrites
   * references in other nodes, so it isn't just "one more patch field".
   * Rejects (thrown error) if `newId` is empty or already used by another
   * node; `handleOk` below surfaces that inline and keeps the modal open
   * rather than closing on a half-applied change. */
  onRenameId: (oldId: string, newId: string) => Promise<void>;
  /** Commits the ID field being left empty: drops the node's explicit id,
   * handing it back to the parser's own title-slug fallback. Returns the
   * id the node actually ends up with (usually unchanged — an untouched
   * id is already a slug of the title — but not guaranteed), same
   * "current id, not `node.id`" reasoning `onChange` above needs. Can't
   * fail the way `onRenameId` can (nothing to collide with, always a
   * fresh derivation), so `handleOk` doesn't need an error path for it. */
  onClearId: (id: string) => Promise<string>;
  onClose: () => void;
  /** Fired only on a real "ok" commit (never on cancel/backdrop-click),
   * right before `onClose` — unlike `onChange`, which is skipped entirely
   * when nothing actually changed (see `handleOk`), this always fires on
   * "ok", so a caller that needs to know "the user committed this modal"
   * (e.g. App.tsx opening the body editor right after an "add child" +
   * "ok", regardless of whether any field actually changed) has something
   * to hook. Takes the post-rename id (same reasoning as `onChange`'s own
   * `id` parameter) and the node's final type, so a caller that only cares
   * about e.g. a freshly-created *text* node doesn't have to wait for the
   * `onChange` patch to round-trip back through a fresh `canvas` first. */
  onOk?: (id: string, type: NodeType) => void;
}

/** JSON Canvas colors are either a hex string or a preset `"1"`–`"6"` — the
 * empty entry clears back to "no color". Purely a convenience shortcut
 * alongside the freeform text input, same idea as `VarsForm`'s `select`. */
const COLOR_SWATCHES = ["", "1", "2", "3", "4", "5", "6"];

/**
 * Modal for one node's non-content settings: title, type, color, tags, a
 * file/link node's target, its extra incoming edges (`meshfox:edge`), and
 * deleting the node entirely — everything about a node except its Markdown
 * body, which the inline `NodeTextEditor` handles instead. Reuses
 * `VarsForm`'s modal/backdrop styling.
 *
 * Every field is a local draft until "ok" — unlike the rest of this app
 * (add-child, drag-connecting an edge), which persists immediately, a
 * settings form has enough fields that "changed my mind" needs to be a
 * single, obvious action rather than "manually undo each field back to
 * what it was". "cancel" (or clicking the backdrop) discards the draft
 * outright — nothing here reaches the server until "ok" is clicked, at
 * which point the whole patch commits in one request (a harmless no-op
 * for whatever specific fields weren't actually touched).
 *
 * Deleting a node lives elsewhere — a dedicated button in the node's own
 * title bar (see MeshNode's trash icon and DeleteNodeDialog) rather than
 * here, since it needs to ask what happens to the node's children, which
 * doesn't fit this modal's single ok/cancel shape.
 */
export function NodeSettings({ node, allNodes, onChange, onRenameId, onClearId, onClose, onOk }: NodeSettingsProps) {
  const [title, setTitle] = useState(node.title);
  const [id, setId] = useState(node.id);
  const [idError, setIdError] = useState<string | null>(null);
  // True only while "ok" is waiting on the id rename's round trip (the one
  // part of this form that isn't purely local) — disables the field/button
  // rather than let a second click race it.
  const [saving, setSaving] = useState(false);
  // The title as of when this modal opened (for this node — App.tsx keys
  // this component on `node.id`, so a successful rename remounts it fresh
  // with a new ref here too). Comparing the *current* id against this
  // fixed point (via `basicSlug`, the server's own non-transliterating
  // auto-slug) is what tells the suggestion hint below whether the id has
  // ever diverged from "whatever the server would auto-generate" — true
  // right after creation, and stays true across further title edits until
  // the user (or the hint's own "Apply") actually changes the id by hand.
  const initialTitle = useRef(node.title);
  const [nodeType, setNodeType] = useState<NodeType>(node.type ?? "text");
  const [color, setColor] = useState(node.color ?? "");
  const [target, setTarget] = useState(node.target ?? "");
  const [display, setDisplay] = useState<"link" | "code">(node.display ?? "link");
  const [lang, setLang] = useState(node.lang ?? "");
  const [interpreter, setInterpreter] = useState(node.interpreter ?? "");
  const [preview, setPreview] = useState(node.preview ?? false);
  const [tags, setTags] = useState<string[]>(node.tags ?? []);
  const [extraParents, setExtraParents] = useState<ExtraEdgeDto[]>(node.extraParents ?? []);
  const [addSource, setAddSource] = useState("");
  // Three states, not a checkbox: `node.fold` is itself optional (see
  // `CanvasNode.fold`'s doc comment) — "not set" (follow the document's own
  // default, `meshfox:option name="unfold"`) is a real, distinct value from
  // "explicitly folded"/"explicitly expanded", and this control needs a way
  // to get back to it, not just toggle between the other two.
  const initialFold: "default" | "true" | "false" = node.fold === undefined ? "default" : node.fold ? "true" : "false";
  const [fold, setFold] = useState<"default" | "true" | "false">(initialFold);

  const otherNodes = allNodes.filter((n) => n.id !== node.id);
  const addable = otherNodes.filter((n) => !extraParents.some((e) => e.from === n.id));
  const addSourceValue = addable.some((n) => n.id === addSource) ? addSource : addable[0]?.id ?? "";
  const titleOf = (nodeId: string) => allNodes.find((n) => n.id === nodeId)?.title ?? nodeId;
  // Every distinct tag already used anywhere in the document (this node's
  // own original tags included) — offered as suggestions by the Tags
  // field below (see `TagEditor`'s own `suggestions` prop).
  const documentTags = Array.from(new Set(allNodes.flatMap((n) => n.tags ?? []).concat(node.tags ?? [])));

  // See `initialTitle` above: the hint only shows while the id still looks
  // auto-generated (never diverged), and only when the current title's
  // slug would actually be different from the id already there.
  const idLooksAutoGenerated = idMatchesAutoSlug(id, initialTitle.current);
  const suggestedId = translitSlug(title);
  const showIdSuggestion = idLooksAutoGenerated && suggestedId !== "" && suggestedId !== id;

  // Guards against submitting a combination that's plainly incomplete
  // (still worth typing further on) rather than surfacing a 422 once "ok"
  // is clicked — a file/link/include node's target is required for its
  // body to be a valid single link. The id field itself has no such
  // guard: left empty, "ok" clears the id instead of renaming it (see
  // `handleOk`), so blank is a valid, deliberate state here, not an
  // incomplete one.
  const isSaveable =
    title.trim() !== "" &&
    (!(nodeType === "file" || nodeType === "link" || nodeType === "include") || target.trim() !== "");

  // Only whatever actually differs from `node`'s own original values —
  // matters a lot more here than it would look: `node.type` is never
  // literally `"include"` (the server always resolves one into a `group`
  // or `text` before it ever reaches the client — see
  // `crates/core/src/include.rs`), so unconditionally resending `nodeType`
  // (as this used to) would silently rewrite an include node's real
  // on-disk `type="include"` into whatever it happened to resolve to,
  // destroying the include — even from an "ok" that touched nothing at
  // all. Diffing against `node` is what keeps a no-op "open, ok" (or an
  // edit to some unrelated field) from ever touching a field the user
  // never actually looked at.
  const buildPatch = (): NodePatch => {
    const patch: NodePatch = {};
    if (title !== node.title) patch.title = title;
    if (nodeType !== (node.type ?? "text")) patch.nodeType = nodeType;
    if (color !== (node.color ?? "")) patch.color = color;
    if (JSON.stringify(tags) !== JSON.stringify(node.tags ?? [])) patch.tags = tags;
    if (JSON.stringify(extraParents) !== JSON.stringify(node.extraParents ?? [])) patch.extraParents = extraParents;
    if (
      (nodeType === "file" || nodeType === "link" || nodeType === "include") &&
      target !== (node.target ?? "")
    ) {
      patch.target = target;
    }
    if (nodeType === "file") {
      if (display !== (node.display ?? "link")) patch.display = display;
      if (lang !== (node.lang ?? "")) patch.lang = lang;
      if (interpreter !== (node.interpreter ?? "")) patch.interpreter = interpreter;
    }
    if (nodeType === "link" && preview !== (node.preview ?? false)) patch.preview = preview;
    if (fold !== initialFold) patch.fold = fold;
    return patch;
  };

  // "cancel" (and clicking the backdrop) — discards every field above
  // outright. Nothing here ever reached the server, so there's nothing to
  // revert; closing is the whole story.
  const handleCancel = () => onClose();

  // "ok" — the id rename (if any) goes first and is the one part of this
  // that can fail (a duplicate id), in which case the modal stays open
  // with the error shown rather than closing on a half-applied change.
  // Every other changed field commits together in one `onChange` call —
  // skipped entirely (no request at all) when nothing actually changed.
  //
  // `onChange` gets the node's id *after* whatever rename just happened,
  // not `node.id` (this component's own original prop) — App.tsx's
  // `onChange` closure is bound to whichever id was current when *this*
  // component instance was rendered, and a successful rename remounts a
  // fresh instance under the new id (see `App.tsx`'s `key={settingsNode.id}`)
  // without waiting for this still-running async handler. Passing the
  // post-rename id explicitly is what keeps this "ok" click's own patch
  // request targeting the node it actually just renamed, instead of the id
  // that rename just made stale — that mismatch used to 404 ("no node
  // ...") whenever a single "ok" both renamed the id and changed another
  // field at once.
  const handleOk = async () => {
    if (!isSaveable) return;
    const trimmedId = id.trim();
    let currentId = node.id;
    if (trimmedId === "") {
      setSaving(true);
      try {
        currentId = await onClearId(node.id);
      } catch (e) {
        setIdError(String(e));
        setSaving(false);
        return;
      }
      setSaving(false);
    } else if (trimmedId !== node.id) {
      setSaving(true);
      try {
        await onRenameId(node.id, trimmedId);
        currentId = trimmedId;
      } catch (e) {
        setId(node.id);
        setIdError(String(e));
        setSaving(false);
        return;
      }
      setSaving(false);
    }
    const patch = buildPatch();
    if (Object.keys(patch).length > 0) onChange(currentId, patch);
    onOk?.(currentId, nodeType);
    onClose();
  };

  return (
    <div className="vars-modal-backdrop" onClick={handleCancel}>
      <div className="vars-modal node-settings-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Node settings</h3>
        <label className="vars-modal-field">
          <span>Title</span>
          <input
            type="text"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            autoFocus
          />
        </label>
        <label className="vars-modal-field">
          <span>ID</span>
          <input
            type="text"
            value={id}
            placeholder="auto (slug of the title)"
            disabled={saving}
            onChange={(e) => {
              setId(e.target.value);
              setIdError(null);
            }}
          />
        </label>
        {idError && <p className="node-settings-id-error">{idError}</p>}
        {showIdSuggestion && !idError && (
          <p className="node-settings-id-hint">
            Change id to "{suggestedId}"?{" "}
            <button type="button" onClick={() => setId(suggestedId)}>
              apply
            </button>
          </p>
        )}
        <label className="vars-modal-field">
          <span>Type</span>
          <select value={nodeType} onChange={(e) => setNodeType(e.target.value as NodeType)}>
            <option value="text">text</option>
            <option value="file">file</option>
            <option value="link">link</option>
            <option value="include">include</option>
            <option value="group">group</option>
          </select>
        </label>
        {nodeType === "include" && (
          <p className="vars-modal-hint">
            Splices another file's content into this node, live, every time the canvas loads — see SPEC.md's
            "Includes". Never written back to the target; edit that file directly to change what shows up here.
          </p>
        )}
        {(nodeType === "file" || nodeType === "link" || nodeType === "include") && (
          <label className="vars-modal-field">
            <span>{nodeType === "file" ? "File path" : nodeType === "include" ? "Target file" : "URL"}</span>
            <input
              type="text"
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              placeholder={nodeType === "link" ? "https://…" : "./path/to/file.md"}
            />
          </label>
        )}
        {nodeType === "file" && (
          <label className="vars-modal-field">
            <span>Display</span>
            <select value={display} onChange={(e) => setDisplay(e.target.value as "link" | "code")}>
              <option value="link">link</option>
              <option value="code">code (read-only preview)</option>
            </select>
          </label>
        )}
        {nodeType === "file" && display === "code" && (
          <label className="vars-modal-field">
            <span>Language</span>
            <input
              type="text"
              value={lang}
              onChange={(e) => setLang(e.target.value)}
              placeholder="auto-detect from file extension"
            />
          </label>
        )}
        {nodeType === "link" && (
          <label className="vars-modal-field">
            <span>Show social preview below the link</span>
            <input
              type="checkbox"
              checked={preview}
              onChange={(e) => setPreview(e.target.checked)}
            />
          </label>
        )}
        {nodeType === "file" && (
          <label className="vars-modal-field">
            <span>Interpreter</span>
            <input
              type="text"
              value={interpreter}
              onChange={(e) => setInterpreter(e.target.value)}
              placeholder="e.g. python — makes this node runnable"
            />
          </label>
        )}
        <label className="vars-modal-field">
          <span>Color</span>
          <input
            type="text"
            value={color}
            onChange={(e) => setColor(e.target.value)}
            placeholder="hex, e.g. #ff8800, or a preset 1–6"
          />
          <div className="node-settings-swatches">
            {COLOR_SWATCHES.map((c) => (
              <button
                type="button"
                key={c || "none"}
                className="node-settings-swatch"
                data-swatch={c || "none"}
                title={c || "no color"}
                onClick={() => setColor(c)}
              />
            ))}
          </div>
        </label>
        <label className="vars-modal-field">
          <span>Fold</span>
          <select value={fold} onChange={(e) => setFold(e.target.value as "default" | "true" | "false")}>
            <option value="default">Document default</option>
            <option value="true">Always folded</option>
            <option value="false">Always expanded</option>
          </select>
        </label>
        <div className="vars-modal-field">
          <span>Tags</span>
          <TagEditor tags={tags} onChange={setTags} suggestions={documentTags} />
        </div>
        <div className="vars-modal-field">
          <span>Incoming edges (extra)</span>
          <p className="node-settings-edge-hint">
            Click an arrow on the canvas to set its text, color, line style, and arrowheads.
          </p>
          <div className="node-settings-chips">
            {extraParents.length === 0 && <em className="node-settings-empty-hint">none</em>}
            {extraParents.map((p) => (
              <span className="node-settings-chip" key={p.from}>
                {titleOf(p.from)}
                <button
                  type="button"
                  className="node-settings-chip-remove"
                  onClick={() => setExtraParents((prev) => prev.filter((x) => x.from !== p.from))}
                  title={`remove edge from ${titleOf(p.from)}`}
                >
                  ×
                </button>
              </span>
            ))}
          </div>
          {addable.length > 0 && (
            <div className="node-settings-add-edge">
              <select value={addSourceValue} onChange={(e) => setAddSource(e.target.value)}>
                {addable.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.title}
                  </option>
                ))}
              </select>
              <button
                type="button"
                onClick={() => setExtraParents((prev) => [...prev, { from: addSourceValue }])}
              >
                add edge
              </button>
            </div>
          )}
        </div>
        <div className="vars-modal-actions">
          <button type="button" onClick={handleCancel} disabled={saving}>
            cancel
          </button>
          <button type="submit" onClick={handleOk} disabled={!isSaveable || saving}>
            {saving ? "saving…" : "ok"}
          </button>
        </div>
      </div>
    </div>
  );
}
