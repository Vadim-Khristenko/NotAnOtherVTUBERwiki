import { expect, test } from "bun:test";
import { templateCall } from "./calls";

const fields = [
  { name: "image", kind: "image", required: false },
  { name: "name", kind: "text", required: true },
  { name: "debut", kind: "date", required: false },
];

test("a call lists the required fields first and puts the cursor in the first", () => {
  const call = templateCall("infobox-vtuber", fields, false);
  expect(call.text).toBe("infobox-vtuber\n|name = \n|image = \n|debut = \n}}");
  expect(call.text.slice(0, call.cursor)).toBe("infobox-vtuber\n|name = ");
});

test("a call that is already closed keeps its own braces", () => {
  const call = templateCall("quote", [{ name: "1", kind: "text", required: false }], true);
  expect(call.text).toBe("quote\n|1 = \n");
});

test("a template without fields is only its name", () => {
  expect(templateCall("stub", [], false)).toEqual({ text: "stub\n}}", cursor: 4 });
});
