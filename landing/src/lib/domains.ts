/* Which addresses are ours, which are friends, and which only look like us. */
export const OURS = [
  { host: "filian.wiki", note: "domains.main" },
  { host: "snackers.wiki", note: "domains.also" },
  { host: "filian.ru", note: "domains.ru" },
  { host: "snackers.vai-rice.space", note: "domains.old" },
];
export const FRIENDS = [
  { host: "filian.rip", note: "domains.rip" },
  { host: "filian.mom", note: "domains.mom" },
  { host: "filian.org", note: "domains.org" },
];
export type Verdict = "ours" | "friend" | "fake" | "other" | "empty";

/** The bare host of whatever was typed: no scheme, no path, no www. */
export function hostOf(input: string): string {
  let s = input.trim().toLowerCase();
  s = s.replace(/^[a-z][a-z0-9+.-]*:\/\//, "").replace(/^\/\//, "");
  s = s.split(/[/?#\s]/)[0].replace(/:\d+$/, "").replace(/\.$/, "");
  s = s.replace(/^[^@]*@/, ""); // user@host
  return s.replace(/^www\./, "");
}

/* Letters that pass for other letters: digits, doubled shapes, and the
   Cyrillic letters that look Latin. The skeleton of a lookalike is ours. */
const LOOKS: Record<string, string> = {
  "0": "o", "1": "l", "3": "e", "5": "s", "7": "t", "!": "i", "|": "l",
  "а": "a", "в": "b", "е": "e", "ё": "e", "і": "i", "ї": "i", "к": "k", "м": "m", "н": "h", "о": "o", "р": "p", "с": "c", "т": "t", "у": "y", "х": "x", "ѕ": "s", "ј": "j", "ԁ": "d", "ӏ": "l",
};
const skeleton = (s: string) =>
  [...s].map((c) => LOOKS[c] ?? c).join("").replace(/rn/g, "m").replace(/vv/g, "w").replace(/i/g, "l").replace(/[-_.]/g, "");

function distance(a: string, b: string) {
  const d = Array.from({ length: a.length + 1 }, (_, i) => [i, ...Array(b.length).fill(0)]);
  for (let j = 1; j <= b.length; j++) d[0][j] = j;
  for (let i = 1; i <= a.length; i++)
    for (let j = 1; j <= b.length; j++)
      d[i][j] = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
  return d[a.length][b.length];
}

/** Positions of letters outside plain Latin, digits, dot and dash: the tell of a homoglyph. */
export function foreignLetters(input: string): number[] {
  const host = hostOf(input);
  return [...host].flatMap((c, i) => (/[a-z0-9.-]/.test(c) ? [] : [i]));
}

const under = (host: string, zone: string) => host === zone || host.endsWith(`.${zone}`);

export function verdict(input: string): Verdict {
  const host = hostOf(input);
  if (!host) return "empty";
  if (OURS.some((o) => under(host, o.host))) return "ours";
  if (FRIENDS.some((f) => under(host, f.host))) return "friend";
  const sk = skeleton(host);
  const close = [...OURS, ...FRIENDS].some((o) => {
    const target = skeleton(o.host);
    return sk === target || distance(sk, target) <= 2 || sk.includes(skeleton(o.host.split(".")[0]));
  });
  if (close || /f[i1l!|]l[i1l!|]an|snack/.test(host)) return "fake";
  return "other";
}
