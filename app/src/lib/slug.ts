/** A name as you type it: lowercase, with each space a dash (macOS
 *  capitalizes the first letter, and people type spaces). The same as
 *  `normalize_name` in drovr-core, one character for one, so the caret
 *  doesn't move. Other characters are left for validation to flag. */
export function typedName(text: string): string {
  return text.replace(/\s/g, "-").toLowerCase();
}

/** For a name field's oninput: normalizes what's typed in place, keeping
 *  the caret where it was, and returns the new value. */
export function nameInput(e: Event & { currentTarget: HTMLInputElement }): string {
  const el = e.currentTarget;
  const v = typedName(el.value);
  if (v !== el.value) {
    const at = el.selectionStart;
    el.value = v;
    if (at !== null) el.setSelectionRange(at, at);
  }
  return v;
}

/** Attributes for name fields: no autocapitalizing, autocorrect or spelling. */
export const NAME_FIELD = { autocapitalize: "off", autocorrect: "off", spellcheck: false } as const;

/** A drovr name from free text: lowercase letters, digits and hyphens,
 *  starting with a letter (same rules as `slug` in drovr-core). */
export function slug(text: string): string {
  const s = text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 30)
    .replace(/-+$/, "");
  return /^[a-z]/.test(s) ? s : s ? `a-${s}` : "";
}
