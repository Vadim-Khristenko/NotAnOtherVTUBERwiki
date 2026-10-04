import { describe, expect, test } from "bun:test";
import { toggleMarks, togglePrefix, type Edit } from "./marks";

/// The document after an edit, with the selection shown as [ and ].
function apply(doc: string, e: Edit): string {
  const out = doc.slice(0, e.from) + e.insert + doc.slice(e.to);
  return out.slice(0, e.anchor) + "[" + out.slice(e.anchor, e.head) + "]" + out.slice(e.head);
}

/// Runs a toggle on `doc` where [ and ] mark the selection.
function run(marked: string, open: string, close = open): string {
  const from = marked.indexOf("[");
  const to = marked.indexOf("]") - 1;
  const doc = marked.replace("[", "").replace("]", "");
  return apply(doc, toggleMarks(doc, from, to, open, close));
}

describe("toggleMarks", () => {
  test("marks plain text", () => {
    expect(run("a [word] b", "**")).toBe("a **[word]** b");
  });
  test("a second press takes the mark off, markers outside", () => {
    expect(run("a **[word]** b", "**")).toBe("a [word] b");
  });
  test("a second press takes the mark off, markers inside", () => {
    expect(run("a [**word**] b", "**")).toBe("a [word] b");
  });
  test("part of a run becomes plain, the rest stays", () => {
    expect(run("**one [two] three**", "**")).toBe("**one **[two]** three**");
  });
  test("the start of a run becomes plain", () => {
    expect(run("**[one] two**", "**")).toBe("[one]** two**");
  });
  test("two overlapping runs join into one", () => {
    expect(run("**ab**c[d**ef**g]h", "**")).toBe("**ab**c**[defg]**h");
    expect(run("**a[b** c **d]e**", "**")).toBe("**[ab c de]**");
  });
  test("italic is not confused with bold", () => {
    expect(run("**[bold]**", "*")).toBe("***[bold]***");
    expect(run("*[it]*", "*")).toBe("[it]");
  });
  test("the cursor inside a run takes it off", () => {
    expect(run("a ~~wo[]rd~~ b", "~~")).toBe("a wo[]rd b");
  });
  test("the cursor elsewhere gets a pair to type into", () => {
    expect(run("a [] b", "**")).toBe("a **[]** b");
  });
  test("pairs with two sides toggle too", () => {
    expect(run("[Infobox]", "{{", "}}")).toBe("{{[Infobox]}}");
    expect(run("{{[Infobox]}}", "{{", "}}")).toBe("[Infobox]");
  });
  test("runs on other lines are left alone", () => {
    expect(run("**x**\nab[c]d", "**")).toBe("**x**\nab**[c]**d");
  });
});

describe("togglePrefix", () => {
  const applyAll = (doc: string, changes: { from: number; to: number; insert: string }[]) =>
    [...changes].reverse().reduce((d, c) => d.slice(0, c.from) + c.insert + d.slice(c.to), doc);
  test("adds to every line that lacks it", () => {
    const doc = "one\n- two\nthree";
    expect(applyAll(doc, togglePrefix(doc, 0, doc.length, "- "))).toBe("- one\n- two\n- three");
  });
  test("takes it off when every line has it", () => {
    const doc = "- one\n- two";
    expect(applyAll(doc, togglePrefix(doc, 2, 8, "- "))).toBe("one\ntwo");
  });
});
