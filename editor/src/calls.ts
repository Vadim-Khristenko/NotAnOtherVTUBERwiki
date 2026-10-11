// A template call written out from the template's `<params>`.

export type Field = { name: string; kind: string; required: boolean };

// A template with fields comes in as a whole call, one field a line, the
// required ones first, and the cursor in the first: `{{Name\n|a = \n}}`.
// `closed` when a `}}` already follows, which the call then keeps.
export function templateCall(name: string, fields: Field[], closed: boolean): { text: string; cursor: number } {
  const ordered = [...fields.filter((f) => f.required), ...fields.filter((f) => !f.required)];
  let text = name;
  let cursor = -1;
  for (const f of ordered) {
    text += `\n|${f.name} = `;
    if (cursor < 0) cursor = text.length;
  }
  text += closed ? "\n" : "\n}}";
  return { text, cursor: cursor < 0 ? name.length : cursor };
}
