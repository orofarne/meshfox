import { useState, type FormEvent } from "react";
import type { VarStatus } from "./types";

interface VarsFormProps {
  /** For the pre-run gate (`handleRun`), only the *missing* declared
   * variables — see App.tsx, which never opens this at all when
   * `fetchVars()` reports everything already resolved. For the `c`/
   * "configure" flow (`handleConfigure`), every declared non-secret
   * variable in the document, resolved or not — see `fetchConfigureVars`. */
  vars: VarStatus[];
  onSubmit: (answers: Record<string, string>) => void;
  onCancel: () => void;
  /** Defaults to the pre-run gate's own copy — `handleConfigure` overrides
   * these three for the "configure every declared variable" flow, the
   * browser counterpart to `meshfox configure`/the TUI's `c` key. */
  title?: string;
  hint?: string;
  submitLabel?: string;
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
 * (until the cache is cleared or a different value is needed).
 */
export function VarsForm({
  vars,
  onSubmit,
  onCancel,
  title = "Configure variables",
  hint,
  submitLabel = "run",
}: VarsFormProps) {
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(vars.map((v) => [v.name, initialValue(v)])),
  );

  const set = (name: string, value: string) => setValues((prev) => ({ ...prev, [name]: value }));

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    onSubmit(values);
  };

  const defaultHint = `This canvas needs a few values before it can run — answered once, then remembered${
    vars.some((v) => v.secret) ? " (secret ones aren't saved, and are asked for again next time)" : ""
  }.`;

  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <form className="vars-modal" onClick={(e) => e.stopPropagation()} onSubmit={handleSubmit}>
        <h3>{title}</h3>
        <p className="vars-modal-hint">{hint ?? defaultHint}</p>
        {vars.map((v, i) => (
          <label key={v.name} className="vars-modal-field">
            <span>{v.prompt}</span>
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
                value={values[v.name]}
                onChange={(e) => set(v.name, e.target.value)}
                autoFocus={i === 0}
                required
              />
            )}
          </label>
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
