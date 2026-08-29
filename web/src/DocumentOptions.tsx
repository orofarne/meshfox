import { useState } from "react";

interface KnownOption {
  name: string;
  label: string;
  hint: string;
}

// Every option meshfox itself currently understands — see SPEC.md's
// "Options". Deliberately a plain array, not derived from anything the
// server sends: an unrecognized `meshfox:option` name is valid (future
// meshfox versions, or a hand-written one this UI doesn't know about yet)
// and is preserved untouched (see `DocumentOptions`' own doc comment)
// rather than rejected.
const KNOWN_OPTIONS: KnownOption[] = [
  {
    name: "unfold",
    label: "Expand everything by default",
    hint: "Without this, every subtree (except root) opens folded to a compact title-only row, and you unfold what you need as you go. A node's own \"fold\" setting (in its own settings modal) still overrides this either way.",
  },
  {
    name: "auto-timestamps",
    label: "Auto-stamp createdAt/updatedAt",
    hint: "Off by default — meshfox is first and foremost a documentation format, and most documents don't want bookkeeping churn on every regeneration. With this enabled, meshfox stamps createdAt automatically when a node is created and bumps updatedAt whenever its body text actually changes (see SPEC.md's \"Timestamps\"). An explicit `node meta --created-at` still works either way.",
  },
];

interface DocumentOptionsProps {
  /** Every `meshfox:option` this document currently declares — see
   * `CanvasDoc.options`. May include names `KNOWN_OPTIONS` doesn't
   * recognize; those are shown read-only and always carried through
   * unchanged on submit. */
  options: string[];
  onSubmit: (options: string[]) => void;
  onCancel: () => void;
}

/**
 * Toolbar "options" modal — the browser's write path for document-wide
 * `<!-- meshfox:option name="..." -->` declarations (see SPEC.md's
 * "Options", `PUT /api/options`). Unlike `VarsForm`'s "configure" flow
 * (which only ever resolves/caches a *value* for a declaration someone
 * already hand-wrote into the file), an option is a bare on/off flag with
 * nothing to prompt for, so this lets the checkbox itself add or remove
 * the declaration.
 *
 * Starts from `options` verbatim (as a `Set`) so any unrecognized name
 * already declared stays in the set — and therefore in the submitted
 * list — the whole time; only `KNOWN_OPTIONS`' own checkboxes can add or
 * remove membership.
 */
export function DocumentOptions({ options, onSubmit, onCancel }: DocumentOptionsProps) {
  const [selected, setSelected] = useState<Set<string>>(() => new Set(options));

  const toggle = (name: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });

  const unknown = options.filter((o) => !KNOWN_OPTIONS.some((k) => k.name === o));

  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <div className="vars-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Document options</h3>
        <p className="vars-modal-hint">
          Document-wide settings, written as a <code>meshfox:option</code> comment in the root node — see SPEC.md's
          "Options".
        </p>
        {KNOWN_OPTIONS.map((opt) => (
          <div key={opt.name}>
            <label className="vars-modal-field">
              <span>{opt.label}</span>
              <input type="checkbox" checked={selected.has(opt.name)} onChange={() => toggle(opt.name)} />
            </label>
            <p className="vars-modal-hint">{opt.hint}</p>
          </div>
        ))}
        {unknown.length > 0 && (
          <p className="vars-modal-hint">
            Also declared, not recognized by this version of meshfox (left as-is): {unknown.join(", ")}
          </p>
        )}
        <div className="vars-modal-actions">
          <button type="button" onClick={onCancel}>
            cancel
          </button>
          <button type="button" onClick={() => onSubmit([...selected])}>
            save
          </button>
        </div>
      </div>
    </div>
  );
}
