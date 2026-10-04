// Inline formatting that toggles, as in a word processor.
//
// Pressing Bold on text that is already bold makes it plain again; on part
// of a bold run it makes only that part plain; across two bold runs it
// joins them into one. The same goes for every mark written as a pair of
// markers (`**`, `*`, `~~`, `==`, `||`, `` ` ``, `++`, ...). Pairs whose
// two sides differ (`{{` and `}}`) toggle when the selection holds them or
// sits right inside them.
//
// Pure functions over the document text, so both the rich editor and the
// plain textarea use them, and they are tested on their own.

/// One replacement and the selection after it, in document offsets.
export type Edit = { from: number; to: number; insert: string; anchor: number; head: number };

/// Whether `mark` at `pos` is a marker of exactly that length: `*` inside
/// `**` is not an italic marker, and `**` inside `***` is not bold.
function exact(doc: string, pos: number, mark: string): boolean {
  if (!doc.startsWith(mark, pos)) return false;
  const c = mark[0];
  if (mark.split("").some((ch) => ch !== c)) return true;
  return doc[pos - 1] !== c && doc[pos + mark.length] !== c;
}

/// The marked runs on the lines from `start` to `end`, as [open, close) where
/// close is past the closing marker. Markers pair up in order.
function runs(doc: string, start: number, end: number, mark: string): [number, number][] {
  const found: number[] = [];
  for (let i = start; i <= end - mark.length; ) {
    if (exact(doc, i, mark)) {
      found.push(i);
      i += mark.length;
    } else {
      i += 1;
    }
  }
  const out: [number, number][] = [];
  for (let k = 0; k + 1 < found.length; k += 2) out.push([found[k], found[k + 1] + mark.length]);
  return out;
}

export function toggleMarks(doc: string, from: number, to: number, open: string, close = open): Edit {
  const sel = doc.slice(from, to);

  // The selection holds the markers: drop them.
  if (
    sel.length >= open.length + close.length &&
    exact(doc, from, open) &&
    exact(doc, to - close.length, close)
  ) {
    const inner = sel.slice(open.length, sel.length - close.length);
    return { from, to, insert: inner, anchor: from, head: from + inner.length };
  }
  // The markers sit right around the selection: drop them.
  if (from >= open.length && exact(doc, from - open.length, open) && exact(doc, to, close)) {
    const start = from - open.length;
    return { from: start, to: to + close.length, insert: sel, anchor: start, head: start + sel.length };
  }

  const symmetric = open === close;
  const lineStart = doc.lastIndexOf("\n", from - 1) + 1;
  const nl = doc.indexOf("\n", to);
  const lineEnd = nl === -1 ? doc.length : nl;
  const marked = symmetric ? runs(doc, lineStart, lineEnd, open) : [];
  const m = open.length;

  if (from === to) {
    // The cursor inside a run: that run becomes plain.
    const inside = marked.find(([a, b]) => a + m <= from && from <= b - m);
    if (inside) {
      const [a, b] = inside;
      const inner = doc.slice(a + m, b - m);
      return { from: a, to: b, insert: inner, anchor: from - m, head: from - m };
    }
    // Nowhere marked: a pair to type into.
    return { from, to, insert: open + close, anchor: from + open.length, head: from + open.length };
  }

  // Part of one run: only that part becomes plain, the rest stays marked.
  const holder = marked.find(([a, b]) => a + m <= from && to <= b - m);
  if (holder) {
    const [a, b] = holder;
    const left = doc.slice(a + m, from);
    const right = doc.slice(to, b - m);
    const leftPart = left ? open + left + close : "";
    const rightPart = right ? open + right + close : "";
    const insert = leftPart + sel + rightPart;
    const anchor = a + leftPart.length;
    return { from: a, to: b, insert, anchor, head: anchor + sel.length };
  }

  // Overlapping runs join the selection into one marked run.
  const touched = marked.filter(([a, b]) => a < to && b > from);
  if (touched.length > 0) {
    const start = Math.min(from, ...touched.map(([a]) => a));
    const end = Math.max(to, ...touched.map(([, b]) => b));
    // Drop the markers of the joined runs, keep everything else.
    const drop = new Set<number>();
    for (const [a, b] of touched) {
      drop.add(a);
      drop.add(b - m);
    }
    let body = "";
    for (let i = start; i < end; ) {
      if (drop.has(i)) {
        i += m;
      } else {
        body += doc[i];
        i += 1;
      }
    }
    const insert = open + body + close;
    return { from: start, to: end, insert, anchor: start + m, head: start + m + body.length };
  }

  return { from, to, insert: open + sel + close, anchor: from + open.length, head: from + open.length + sel.length };
}

/// Line prefixes that toggle: on every touched line when some lack it, off
/// every line when all have it. Returns the changes, earliest first.
export function togglePrefix(doc: string, from: number, to: number, prefix: string): { from: number; to: number; insert: string }[] {
  const start = doc.lastIndexOf("\n", from - 1) + 1;
  const nl = doc.indexOf("\n", to > from && doc[to - 1] === "\n" ? to - 1 : to);
  const end = nl === -1 ? doc.length : nl;
  const lines: number[] = [];
  for (let i = start; i <= end; ) {
    lines.push(i);
    const next = doc.indexOf("\n", i);
    if (next === -1 || next >= end) break;
    i = next + 1;
  }
  const all = lines.every((at) => doc.startsWith(prefix, at));
  return lines
    .filter((at) => all || !doc.startsWith(prefix, at))
    .map((at) => (all ? { from: at, to: at + prefix.length, insert: "" } : { from: at, to: at, insert: prefix }));
}
