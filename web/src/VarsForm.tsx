import { useState, type FormEvent } from "react";
import type { VarOrigin, VarStatus } from "./types";

interface VarsFormProps {
  /** For the pre-run gate (`handleRun`), only the *missing* declared
   * variables — see App.tsx, which never opens this at all when
   * `fetchVars()` reports everything already resolved. For the `c`/
   * "configure" flow (`handleConfigure`), every declared non-secret
   * variable in the document, resolved or not — see `fetchConfigureVars`. */
  vars: VarStatus[];
  /** `saveSecrets` is the names of whichever `secret` fields the user
   * checked "save (plaintext)" on (see the per-field checkbox below) — the
   * caller only ever needs to look at it for the pre-run form, since the
   * "configure" flow never shows a `secret` field at all (`GET
   * /api/vars/configure` excludes them entirely) to have one checked in the
   * first place. */
  onSubmit: (answers: Record<string, string>, saveSecrets: string[], clearSecrets: string[]) => void;
  onCancel: () => void;
  /** Defaults to the pre-run gate's own copy — `handleConfigure` overrides
   * these three for the "configure every declared variable" flow, the
   * browser counterpart to `meshfox configure`/the TUI's `c` key. */
  title?: string;
  hint?: string;
  submitLabel?: string;
  /** The "configure every declared variable" flow: a `secret` field shows
   * whether a value is already stored (or inherited from config) instead of
   * a "save" checkbox, an empty one means "leave it alone", and a stored one
   * can be cleared. Typing a value is itself the request to save it. */
  configure?: boolean;
}

// Mirrors what `meshfox_core::vars::validate_value` itself accepts for an
// `Int` field — anything `i64::from_str` parses (an optional leading
// `+`/`-`, digits, nothing else — no decimal point, no scientific
// notation, no thousands separator) and nothing it doesn't. `bool`/
// `select` don't need an equivalent check here since their own controls
// (the checkbox/`<select>` below) can't produce anything invalid in the
// first place, unlike a free-text number input, which a browser's native
// `type="number"` validation only partially constrains (it still accepts
// "3.14", "1e5", ...).
const INT_PATTERN = /^[+-]?\d+$/;

function isValidValue(v: VarStatus, value: string, configure = false): boolean {
  if (configure && v.secret && value === "") return true;
  return v.type !== "int" || INT_PATTERN.test(value);
}

// Short label + title-attribute detail for an "inherited" badge — see
// `VarStatus.inheritedFrom`/`meshfox_core::shared_env`.
function inheritedLabel(origin: VarOrigin): { text: string; title: string } {
  if (origin.scope === "project") {
    return { text: "project", title: "Inherited from this project's .meshfox/config.toml" };
  }
  return {
    text: "global",
    title: origin.path
      ? `Inherited from ~/.meshfox/config.toml (scoped to ${origin.path})`
      : "Inherited from ~/.meshfox/config.toml",
  };
}

function initialValue(v: VarStatus): string {
  // `value` carries a suggestion to pre-fill even when `resolved` is
  // false — a `required` declaration's own `default`, offered so it can
  // just be confirmed as-is (see crates/server/src/lib.rs's `var_status`).
  if (v.value !== undefined) return v.value;
  if (v.type === "bool") return "false";
  if (v.type === "select") return v.choices?.[0] ?? "";
  return "";
}

/**
 * Blocking modal shown when `handleRun` finds one or more declared
 * `meshfox:var`s unresolved (see SPEC.md's "Variables") — the browser's
 * counterpart to `meshfox run`/`configure`'s terminal prompt. Answers are
 * submitted alongside the run request; the server persists whatever isn't
 * `secret` to the on-disk cache, so this only has to ask once per variable
 * (until the cache is cleared or a different value is needed) — a `secret`
 * field gets its own "save (plaintext)" checkbox (or "save to keychain", per `secret_store`) instead (TODO.canvas.md:
 * "Галочка \"сохранить\" у secret"), off by default, for opting a specific
 * secret into that same on-disk persistence anyway; there's no encryption
 * yet, so checking it really does write the value out in plain text.
 */
export function VarsForm({
  vars,
  onSubmit,
  onCancel,
  title = "Configure variables",
  hint,
  submitLabel = "run",
  configure = false,
}: VarsFormProps) {
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(vars.map((v) => [v.name, initialValue(v)])),
  );
  // Which `secret` fields' "save (plaintext)" checkbox is checked — see
  // `VarsFormProps.onSubmit`'s own doc comment. Absent from `values`
  // itself: it isn't a variable's *value*, and initializing it there would
  // mean threading a `secret`-only branch through every place `values` is
  // built/read.
  const [saveSecret, setSaveSecret] = useState<Record<string, boolean>>({});
  // Configure only: which stored `secret` fields get deleted on submit.
  const [clearSecret, setClearSecret] = useState<Record<string, boolean>>({});
  // Set by `handleSubmit` when an `int` field fails `isValidValue` — the
  // server would reject it too (`meshfox_core::validate_value`, wired
  // into `POST /api/vars/configure`/`/api/run`), but catching it here
  // means a bad value never leaves the browser as a request in the first
  // place, and the error shows up right where it was typed.
  const [error, setError] = useState<string | null>(null);

  const set = (name: string, value: string) => setValues((prev) => ({ ...prev, [name]: value }));

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    const invalid = vars.find((v) => !isValidValue(v, values[v.name], configure));
    if (invalid) {
      setError(`${invalid.prompt} needs a whole number (like 42 or -3), not ${JSON.stringify(values[invalid.name])}.`);
      return;
    }
    setError(null);
    onSubmit(
      values,
      Object.keys(saveSecret).filter((name) => saveSecret[name]),
      Object.keys(clearSecret).filter((name) => clearSecret[name]),
    );
  };

  const defaultHint = `This canvas needs a few values before it can run — answered once, then remembered${
    vars.some((v) => v.secret)
      ? " (secret ones aren't saved and are asked for again next time, unless you check \"save\")"
      : ""
  }.`;

  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <form className="vars-modal" onClick={(e) => e.stopPropagation()} onSubmit={handleSubmit}>
        <h3>{title}</h3>
        {error ? <p className="vars-modal-error">{error}</p> : <p className="vars-modal-hint">{hint ?? defaultHint}</p>}
        {vars.map((v, i) => (
          <div key={v.name} className="vars-modal-field-group">
          <label className="vars-modal-field">
            <span>
              {v.prompt}
              {v.inheritedFrom && (
                <span
                  className="vars-modal-inherited"
                  title={
                    configure && v.secret
                      ? `${inheritedLabel(v.inheritedFrom).title} — type a value to override`
                      : inheritedLabel(v.inheritedFrom).title
                  }
                >
                  {inheritedLabel(v.inheritedFrom).text}
                </span>
              )}
            </span>
            {v.type === "bool" ? (
              <input
                type="checkbox"
                checked={values[v.name] === "true"}
                onChange={(e) => set(v.name, e.target.checked ? "true" : "false")}
              />
            ) : v.type === "select" ? (
              <select
                value={values[v.name]}
                onChange={(e) => set(v.name, e.target.value)}
                autoFocus={i === 0}
              >
                {(v.choices ?? []).map((c) => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
            ) : (
              <input
                type={v.type === "int" ? "number" : v.secret ? "password" : "text"}
                step={v.type === "int" ? 1 : undefined}
                value={values[v.name]}
                onChange={(e) => set(v.name, e.target.value)}
                autoFocus={i === 0}
                // Only `int` genuinely can't be empty (it has to parse as a
                // whole number — see `isValidValue`/`INT_PATTERN`). A
                // `string` field is allowed to be blank on purpose (e.g. "no
                // fixed value" style defaults) — `meshfox_core::vars::
                // validate_value` already accepts "" for it.
                required={v.type === "int" && !(configure && v.secret)}
                placeholder={configure && v.secret ? (v.stored || v.inheritedFrom ? "•••••• (unchanged)" : "") : undefined}
              />
            )}
          </label>
          {v.secretError && (
            <p className="vars-modal-error" title={v.secretError}>
              Couldn't read the saved value from the secret store: {v.secretError}
            </p>
          )}
          {v.secret && configure && (v.stored || !v.inheritedFrom) && (
            <div className="vars-modal-secret-save">
              {v.stored ? (
                <label>
                  <input
                    type="checkbox"
                    checked={clearSecret[v.name] ?? false}
                    onChange={(e) => setClearSecret((prev) => ({ ...prev, [v.name]: e.target.checked }))}
                  />
                  <span title="Deletes the value stored for this variable.">
                    stored in {v.secretStore === "keychain" ? "keychain" : "secret store"} — clear it
                  </span>
                </label>
              ) : (
                <span>not set</span>
              )}
            </div>
          )}
          {v.secret && !configure && (
            <label className="vars-modal-secret-save">
              <input
                type="checkbox"
                checked={saveSecret[v.name] ?? false}
                onChange={(e) => setSaveSecret((prev) => ({ ...prev, [v.name]: e.target.checked }))}
              />
              {v.secretStore === "keychain" ? (
                <span title="Saved in the system keychain (secret_store = &quot;keychain&quot;).">
                  save to keychain
                </span>
              ) : (
                <span title="Not encrypted — written to the on-disk var cache in plain text.">
                  save (plaintext)
                </span>
              )}
            </label>
          )}
          </div>
        ))}
        <div className="vars-modal-actions">
          <button type="button" onClick={onCancel}>
            cancel
          </button>
          <button type="submit">{submitLabel}</button>
        </div>
      </form>
    </div>
  );
}
