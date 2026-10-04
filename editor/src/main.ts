// The rich editor: CodeMirror over the page's own textarea.
//
// The server-rendered editor stays the real one. Its textarea is still the
// form field, and the page's own script still owns drafts, the toolbar,
// emotes, images and the live preview. This island only puts a better
// surface over the textarea and keeps the two in step both ways:
//
//   typing here      -> textarea value and selection, then an `input` event
//   the page writes  -> the textarea's `input` event, picked up here
//
// So without JavaScript, or with this file missing or failing, nothing is
// lost: the textarea is simply what people type in. Nothing here loads on
// reading pages.

import { EditorSelection, EditorState, type Extension } from "@codemirror/state";
import {
  EditorView,
  keymap,
  drawSelection,
  highlightActiveLine,
  placeholder as placeholderExt,
  dropCursor,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, undo, redo } from "@codemirror/commands";
import {
  syntaxHighlighting,
  HighlightStyle,
  bracketMatching,
  Language,
  LanguageSupport,
  defineLanguageFacet,
} from "@codemirror/language";
import { parser as markdownParser, GFM } from "@lezer/markdown";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import {
  autocompletion,
  completionKeymap,
  type CompletionContext,
  type CompletionResult,
} from "@codemirror/autocomplete";
import { tags as t } from "@lezer/highlight";

const PREF_KEY = "naw-editor";

// Markdown with GitHub's extensions, straight from the Lezer parser. The
// stock language package also parses HTML, CSS and JavaScript inside
// Markdown, which would triple the download for colours few pages need.
const markdownSupport = new LanguageSupport(
  new Language(defineLanguageFacet({ commentTokens: { block: { open: "<!--", close: "-->" } } }), markdownParser.configure(GFM), [], "markdown"),
);

function preferred(): boolean {
  try {
    return localStorage.getItem(PREF_KEY) !== "plain";
  } catch {
    return true;
  }
}

function remember(rich: boolean) {
  try {
    localStorage.setItem(PREF_KEY, rich ? "rich" : "plain");
  } catch {
    /* private mode: the choice lasts this page only */
  }
}

// Markdown colours from the skin's own variables, so every skin and both
// themes look right without a stylesheet of their own.
const highlight = HighlightStyle.define([
  { tag: t.heading1, fontWeight: "800", fontSize: "1.25em" },
  { tag: t.heading2, fontWeight: "800", fontSize: "1.15em" },
  { tag: [t.heading3, t.heading4, t.heading5, t.heading6], fontWeight: "700" },
  { tag: t.strong, fontWeight: "700" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: [t.link, t.url], color: "var(--accent)" },
  { tag: t.monospace, fontFamily: "var(--mono, ui-monospace, monospace)", color: "var(--ink-soft)" },
  { tag: t.quote, color: "var(--muted)", fontStyle: "italic" },
  { tag: [t.processingInstruction, t.meta, t.contentSeparator], color: "var(--muted)" },
  { tag: t.list, color: "var(--accent)" },
]);

const theme = EditorView.theme({
  // The frame around the toolbar and the text draws the border and the focus.
  "&": {
    color: "var(--ink)",
    backgroundColor: "var(--surface)",
    fontSize: "0.95rem",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-content": {
    fontFamily: "var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace)",
    lineHeight: "1.6",
    padding: "0.75rem 0",
    caretColor: "var(--accent)",
  },
  ".cm-line": { padding: "0 0.9rem" },
  ".cm-scroller": { minHeight: "22rem", maxHeight: "75vh", overflow: "auto" },
  ".cm-activeLine": { backgroundColor: "var(--accent-soft, rgba(127,127,127,0.08))" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--accent-soft, rgba(127,127,127,0.2)) !important",
  },
  ".cm-cursor": { borderLeftColor: "var(--accent)" },
  ".cm-tooltip": {
    backgroundColor: "var(--surface)",
    color: "var(--ink)",
    border: "1px solid var(--line)",
    borderRadius: "var(--radius, 8px)",
  },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
    backgroundColor: "var(--accent)",
    color: "var(--accent-ink)",
  },
  ".cm-placeholder": { color: "var(--faint, var(--muted))" },
});

type Item = { label: string; detail?: string; apply?: string };

const cache = new Map<string, Promise<Item[]>>();

function fetchItems(url: string): Promise<Item[]> {
  let hit = cache.get(url);
  if (!hit) {
    hit = fetch(url, { headers: { Accept: "application/json" } })
      .then((r) => (r.ok ? r.json() : []))
      .catch(() => []);
    cache.set(url, hit);
  }
  return hit;
}

// `:fill` -> emotes from the wiki's own list.
async function emotes(ctx: CompletionContext): Promise<CompletionResult | null> {
  const word = ctx.matchBefore(/:[\w+-]{2,}$/);
  if (!word) return null;
  const list = (await fetchItems("/emotes.json")) as unknown as { n: string }[];
  const q = word.text.slice(1).toLowerCase();
  const options = (Array.isArray(list) ? list : [])
    .filter((e) => e.n && e.n.toLowerCase().includes(q))
    .slice(0, 40)
    .map((e) => ({ label: `:${e.n}:`, type: "constant", apply: `:${e.n}: ` }));
  return { from: word.from, options, validFor: /^:[\w+-]*$/ };
}

// `{{Name` -> templates, `[[slug` -> pages, `[[Category:x` -> categories.
async function wikiRefs(ctx: CompletionContext): Promise<CompletionResult | null> {
  const template = ctx.matchBefore(/\{\{[^{}|\n]*$/);
  if (template) {
    const q = template.text.slice(2);
    const items = await fetchItems(`/api/complete?kind=template&q=${encodeURIComponent(q)}`);
    return {
      from: template.from + 2,
      options: items.map((i) => ({ label: i.label, detail: i.detail, type: "class", apply: i.apply ?? i.label })),
      validFor: /^[^{}|\n]*$/,
    };
  }
  const category = ctx.matchBefore(/\[\[(?:Category|Категория):[^\]|\n]*$/i);
  if (category) {
    const colon = category.text.indexOf(":");
    const q = category.text.slice(colon + 1);
    const items = await fetchItems(`/api/complete?kind=category&q=${encodeURIComponent(q)}`);
    return {
      from: category.from + colon + 1,
      options: items.map((i) => ({ label: i.label, detail: i.detail, type: "namespace", apply: i.apply ?? i.label })),
      validFor: /^[^\]|\n]*$/,
    };
  }
  const page = ctx.matchBefore(/\[\[[^\]|:\n]*$/);
  if (page) {
    const q = page.text.slice(2);
    if (!q && !ctx.explicit) return null;
    const items = await fetchItems(`/api/complete?kind=page&q=${encodeURIComponent(q)}`);
    return {
      from: page.from + 2,
      options: items.map((i) => ({ label: i.label, detail: i.detail, type: "text", apply: i.apply ?? i.label })),
      validFor: /^[^\]|:\n]*$/,
    };
  }
  return null;
}

function wrap(view: EditorView, before: string, after = before): boolean {
  const changes = view.state.changeByRange((range) => {
    const text = view.state.sliceDoc(range.from, range.to) || "text";
    return {
      changes: { from: range.from, to: range.to, insert: before + text + after },
      range: EditorSelection.range(range.from + before.length, range.from + before.length + text.length),
    };
  });
  view.dispatch(view.state.update(changes, { scrollIntoView: true, userEvent: "input" }));
  return true;
}

function linkCmd(view: EditorView): boolean {
  const { from, to } = view.state.selection.main;
  const text = view.state.sliceDoc(from, to) || "text";
  const insert = `[${text}](https://)`;
  const urlStart = from + text.length + 3;
  view.dispatch({
    changes: { from, to, insert },
    selection: { anchor: urlStart, head: urlStart + 8 },
    scrollIntoView: true,
    userEvent: "input",
  });
  return true;
}

/// Prefixes every line the selection touches, once.
function prefixLines(view: EditorView, prefix: string): boolean {
  const { state } = view;
  const changes: { from: number; insert: string }[] = [];
  const seen = new Set<number>();
  for (const range of state.selection.ranges) {
    for (let pos = range.from; pos <= range.to; ) {
      const line = state.doc.lineAt(pos);
      if (!seen.has(line.number)) {
        seen.add(line.number);
        if (!line.text.startsWith(prefix)) changes.push({ from: line.from, insert: prefix });
      }
      pos = line.to + 1;
    }
  }
  view.dispatch({ changes, scrollIntoView: true, userEvent: "input" });
  return true;
}

/// A block on lines of its own, around the selection when there is one.
function insertBlock(view: EditorView, template: string): boolean {
  const { from, to } = view.state.selection.main;
  const selected = view.state.sliceDoc(from, to);
  const body = selected ? template.replace("\n\n", `\n${selected}\n`) : template;
  const before = from > 0 && view.state.sliceDoc(from - 1, from) !== "\n" ? "\n" : "";
  const insert = `${before}${body}\n`;
  view.dispatch({
    changes: { from, to, insert },
    selection: { anchor: from + insert.length },
    scrollIntoView: true,
    userEvent: "input",
  });
  return true;
}

function insertSnippet(view: EditorView, snippet: string): boolean {
  const { from, to } = view.state.selection.main;
  view.dispatch({
    changes: { from, to, insert: snippet },
    selection: { anchor: from + snippet.length },
    scrollIntoView: true,
    userEvent: "input",
  });
  return true;
}

/// `[^n]` at the cursor and its definition at the end, numbered after the
/// highest one already there.
function insertFootnote(view: EditorView): boolean {
  const doc = view.state.doc.toString();
  let n = 1;
  for (const m of doc.matchAll(/\[\^(\d+)\]/g)) n = Math.max(n, Number(m[1]) + 1);
  const mark = `[^${n}]`;
  const { from, to } = view.state.selection.main;
  const end = view.state.doc.length;
  const tail = `${doc.endsWith("\n") ? "" : "\n"}\n${mark}: `;
  view.dispatch({
    changes: [
      { from, to, insert: mark },
      { from: end, insert: tail },
    ],
    selection: { anchor: end + mark.length - (to - from) + tail.length },
    scrollIntoView: true,
    userEvent: "input",
  });
  return true;
}

/// Runs a toolbar button's action in the editor. `true` when it was one.
function runButton(view: EditorView, btn: HTMLElement): boolean {
  if (btn.hasAttribute("data-rich-undo")) return undo(view);
  if (btn.hasAttribute("data-rich-redo")) return redo(view);
  if (btn.hasAttribute("data-wrap")) {
    const before = btn.getAttribute("data-wrap") ?? "";
    return wrap(view, before, btn.getAttribute("data-suffix") ?? before);
  }
  if (btn.hasAttribute("data-prefix")) return prefixLines(view, btn.getAttribute("data-prefix") ?? "");
  if (btn.hasAttribute("data-link")) return linkCmd(view);
  if (btn.hasAttribute("data-footnote")) return insertFootnote(view);
  if (btn.hasAttribute("data-block")) return insertBlock(view, btn.getAttribute("data-block") ?? "");
  if (btn.hasAttribute("data-snippet")) return insertSnippet(view, btn.getAttribute("data-snippet") ?? "");
  return false;
}

const BUTTONS =
  "button[data-wrap],button[data-prefix],button[data-link],button[data-footnote],button[data-block],button[data-snippet],button[data-rich-undo],button[data-rich-redo]";

/// Words, characters and the cursor's line and column, in the page's words.
function status(view: EditorView, el: HTMLElement | null) {
  if (!el) return;
  const format = el.dataset.format;
  if (!format) return;
  const doc = view.state.doc;
  const text = doc.toString();
  const words = (text.match(/[\p{L}\p{N}]+/gu) ?? []).length;
  const head = view.state.selection.main.head;
  const line = doc.lineAt(head);
  el.textContent = format
    .replace("{words}", String(words))
    .replace("{chars}", String(text.length))
    .replace("{line}", String(line.number))
    .replace("{col}", String(head - line.from + 1));
}

type Mounted = { view: EditorView; destroy: () => void };

function mount(area: HTMLTextAreaElement): Mounted {
  const form = area.form;
  const statusEl = document.getElementById("editor-status");
  // Every listener of this mount goes when it is turned off.
  const off = new AbortController();
  let syncing = false;

  // A text change goes to the textarea with an `input` event (drafts,
  // preview, limits); a cursor move only moves the textarea's selection.
  const pushToArea = (view: EditorView, docChanged: boolean) => {
    const sel = view.state.selection.main;
    syncing = true;
    if (docChanged) area.value = view.state.doc.toString();
    try {
      area.setSelectionRange(sel.from, sel.to);
    } catch {
      /* a hidden field may refuse; the value is what matters */
    }
    if (docChanged) area.dispatchEvent(new Event("input", { bubbles: true }));
    syncing = false;
  };

  const extensions: Extension[] = [
    history(),
    drawSelection(),
    dropCursor(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    bracketMatching(),
    EditorView.lineWrapping,
    markdownSupport,
    syntaxHighlighting(highlight),
    theme,
    autocompletion({ override: [emotes, wikiRefs], activateOnTyping: true, maxRenderedOptions: 40 }),
    placeholderExt(area.getAttribute("placeholder") || ""),
    EditorView.contentAttributes.of({
      "aria-label": document.querySelector(`label[for="${area.id}"]`)?.textContent?.trim() || "Text",
      spellcheck: "true",
      lang: document.documentElement.lang || "en",
    }),
    keymap.of([
      { key: "Mod-b", run: (v) => wrap(v, "**") },
      { key: "Mod-i", run: (v) => wrap(v, "*") },
      { key: "Mod-k", run: linkCmd },
      {
        key: "Mod-s",
        run: () => {
          if (form) form.requestSubmit();
          return true;
        },
      },
      ...completionKeymap,
      ...searchKeymap,
      ...historyKeymap,
      ...defaultKeymap,
    ]),
    EditorView.updateListener.of((update) => {
      if (update.docChanged || update.selectionSet) {
        pushToArea(update.view, update.docChanged);
        status(update.view, statusEl);
      }
    }),
    // Pictures pasted or dropped go through the page's own upload, which
    // puts their Markdown at the cursor.
    EditorView.domEventHandlers({
      paste: (event) => uploadFiles(event.clipboardData?.files),
      drop: (event) => uploadFiles(event.dataTransfer?.files),
    }),
  ];

  const view = new EditorView({
    state: EditorState.create({ doc: area.value, extensions }),
  });

  // The page writes into the textarea (emotes, images, a draft coming
  // back): the editor follows, cursor included.
  area.addEventListener(
    "input",
    () => {
      if (syncing) return;
      const doc = view.state.doc.toString();
      const next = area.value;
      const anchor = Math.min(area.selectionStart ?? next.length, next.length);
      const head = Math.min(area.selectionEnd ?? anchor, next.length);
      if (doc !== next) {
        view.dispatch({ changes: { from: 0, to: doc.length, insert: next }, selection: { anchor, head } });
      } else {
        view.dispatch({ selection: { anchor, head } });
      }
      view.focus();
    },
    { signal: off.signal },
  );

  // Whatever sends the focus to the hidden textarea (its label, the page's
  // script, a validation bubble) sends it on to the editor.
  area.addEventListener("focus", () => view.focus(), { signal: off.signal });

  // Toolbar presses act here, before the page script would act on the
  // hidden textarea, and are marked done so it leaves them alone.
  document.addEventListener(
    "click",
    (event) => {
      const btn = (event.target as HTMLElement | null)?.closest<HTMLElement>(BUTTONS);
      if (!btn || !btn.closest("#editor-bar, #selbar")) return;
      if (runButton(view, btn)) {
        event.preventDefault();
        event.stopPropagation();
        // The press stops here, so the menu it came from closes here too.
        btn.closest("details")?.removeAttribute("open");
        view.focus();
      }
    },
    { capture: true, signal: off.signal },
  );

  area.classList.add("rich-hidden");
  area.setAttribute("tabindex", "-1");
  area.setAttribute("aria-hidden", "true");
  area.insertAdjacentElement("afterend", view.dom);
  view.dom.classList.add("rich-editor");
  document.querySelectorAll<HTMLElement>("[data-rich-only]").forEach((el) => (el.hidden = false));
  document.getElementById("editor-frame")?.classList.add("is-rich");
  status(view, statusEl);

  return {
    view,
    destroy: () => {
      off.abort();
      view.destroy();
      area.classList.remove("rich-hidden");
      area.removeAttribute("tabindex");
      area.removeAttribute("aria-hidden");
      document.querySelectorAll<HTMLElement>("[data-rich-only]").forEach((el) => (el.hidden = true));
      document.getElementById("editor-frame")?.classList.remove("is-rich");
      if (statusEl) statusEl.textContent = "";
    },
  };
}

function uploadFiles(files: FileList | null | undefined): boolean {
  if (!files || files.length === 0) return false;
  const images = Array.from(files).filter((f) => f.type.startsWith("image/"));
  const input = document.getElementById("image-file") as HTMLInputElement | null;
  if (images.length === 0 || !input) return false;
  const transfer = new DataTransfer();
  images.forEach((f) => transfer.items.add(f));
  input.files = transfer.files;
  input.dispatchEvent(new Event("change"));
  return true;
}

function start() {
  const area = document.getElementById("body-md") as HTMLTextAreaElement | null;
  const toggle = document.getElementById("editor-mode") as HTMLInputElement | null;
  if (!area) return;
  let mounted: Mounted | null = null;
  const turnOn = () => {
    if (mounted) return;
    mounted = mount(area);
  };
  const turnOff = () => {
    if (!mounted) return;
    mounted.destroy();
    mounted = null;
    area.focus();
  };
  if (toggle) {
    toggle.closest<HTMLElement>("[data-needs-rich]")?.removeAttribute("hidden");
    toggle.checked = preferred();
    toggle.addEventListener("change", () => {
      remember(toggle.checked);
      if (toggle.checked) turnOn();
      else turnOff();
    });
  }
  if (preferred()) turnOn();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", start);
} else {
  start();
}
