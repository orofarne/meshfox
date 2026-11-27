import { useId, type InputHTMLAttributes } from "react";

interface SuggestInputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "list" | "value" | "onChange"> {
  value: string;
  onChange: (value: string) => void;
  /** `suggest=`/`suggest_var=` of the variable. Offered, never enforced: any
   * text stays a valid answer (unlike `ChoiceSelect`'s `choices`). */
  suggest?: readonly string[];
}

/**
 * A `string` variable's text input with completion candidates, as the
 * browser's own `<datalist>`: typing narrows the offered list, picking one
 * fills the field, and typing something else is just as valid.
 */
export function SuggestInput({ value, onChange, suggest, ...rest }: SuggestInputProps) {
  const listId = useId();
  const has = !!suggest && suggest.length > 0;
  return (
    <>
      <input {...rest} type="text" list={has ? listId : undefined} value={value} onChange={(e) => onChange(e.target.value)} />
      {has && (
        <datalist id={listId}>
          {suggest.map((s) => (
            <option key={s} value={s} />
          ))}
        </datalist>
      )}
    </>
  );
}
