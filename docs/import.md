# Importing from MediaWiki and FANDOM

`naw import` reads a MediaWiki XML dump, converts each page's wikitext to the
engine's Markdown, and reports what carried over. With `--write` it brings the
pages into a wiki, history included.

## Getting a dump

Any of these writes the same format:

- **Special:Export** on the source wiki, with "Include only the current
  revision" unticked for the history.
- **The API**: `api.php?action=query&export=1&exportnowrap=1&titles=A|B|C`
  for named pages, or `&generator=allpages&gapnamespace=10&gaplimit=500` for a
  namespace (10 is templates).
- **A database dump**: FANDOM offers one under Special:Statistics. It holds
  every page with its full history.

Export the templates along with the articles. Without them, articles show a
"template not found" link where an infobox would be.

## A dry run

```sh
naw import wiki.xml --out converted --report report.md
```

Nothing is written to the database. `--out` writes each page's Markdown to a
folder to read through, and the report lists:

- how many pages converted with nothing left out;
- what was left out, how often, and the first place it appeared;
- the templates the pages call, and which of them the dump lacks;
- how many files the pages show (files are not in an XML dump);
- links to pages the dump does not have.

## Writing

```sh
naw import wiki.xml --write --wiki filian
```

Articles (namespace 0), templates (10) and categories (14) come in with every
revision the dump holds, converted one by one. Each revision keeps its date,
its edit summary and its minor flag. Its author is credited by the name they
had on the source wiki, shown in the history as `Name (Source Wiki)`, since
they have no account here. Edits made without an account are credited to
nobody: their addresses are not kept.

A page that already exists here is left alone, never overwritten, and listed
in the report. A redirect becomes a redirect unless a page is at its address.
The wiki is reindexed at the end, so categories, links and search see the new
pages.

Content on FANDOM and most MediaWiki sites is under CC BY-SA. The history
keeps the authors' names; say on the wiki where its imported pages came from.

## What converts

| Wikitext | Here |
|---|---|
| `== Heading ==`, `'''bold'''`, `''italic''` | Markdown headings and emphasis |
| `[[Page]]`, `[[Page\|text]]`, `[[Page#Section]]` | Links to the page's address here: `Bao the Whale` is `bao-the-whale` |
| `[[Category:X]]` | The category |
| `[[File:X.png\|thumb\|Caption]]`, `<gallery>` | `![Caption](image:x.png)` |
| `[https://x text]`, bare addresses | Markdown links |
| `*`, `#`, `;` and `:` lists | Lists and definition lists |
| `{\| tables \|}` | Markdown tables; HTML tables when cells span or hold blocks |
| `<ref>`, `<references />`, `{{Reflist}}` | Footnotes, numbered in order, named ones reused |
| `{{Template\|a=b}}`, `{{#if:}}`, `{{#switch:}}` | Kept as they are: the engine's templates take the same calls |
| FANDOM portable infoboxes (`<infobox>`) | A `:::infobox` block; picture parameters in the calls become file addresses |
| `<tabber>` | A `:::details` block per tab |
| `<nowiki>`, `<pre>`, `<syntaxhighlight>`, `<code>`, `<math>` | Escaped text, code blocks, code, math |
| `{{#ev:youtube\|id}}` | A link to the video |
| `__TOC__` | `[[toc]]` |

Titles in other scripts get an address made from a hash of the title, the
same every time, so links to them still work.

## What does not

Lua modules (`{{#invoke:}}`), parser functions the engine lacks (`#expr`,
`#time`, `#ifexist`...), `{{PAGENAME}}` inside templates, and extension tags
such as `<dpl>` or `<choose>` are left out and counted in the report. Pages
in other namespaces (user pages, talk pages, files) are not imported. Files
themselves come over separately: the report lists how many the pages use.
