# Modules

A module is a small JavaScript program that the server runs to make part of a
page. Where a template pastes the same text in many places, a module can
compute it: count the wiki's pages, pick a line by today's date, build a
table from arguments. The result is Markdown, rendered with the rest of the
page, so it shows with JavaScript off.

## Writing a module

A module is a page in the `module:` namespace, such as `/module:stats`. Its
first ` ```js ` block is the code; everything else on the page is its
documentation. Only admins may create or edit modules, because the code runs
on the server.

````markdown
# Stats

Shows the wiki's numbers.

```js
function summary(args, wiki, page) {
  const s = wiki.stats;
  return `${fmt(s.articles)} articles, ${fmt(s.edits)} edits`;
}
```
````

A function receives three objects:

| Object | What it holds |
|---|---|
| `args` | The call's `key = value` lines, as strings |
| `wiki` | `name`, `lang` (the page's language), `today` (`YYYY-MM-DD`), and `stats` |
| `page` | `path` and `lang` of the calling page |

`wiki.stats` has `articles`, `pages`, `edits`, `files`, `editors`,
`activeEditors` (who edited in the last 30 days) and `editsMonth` (edits in
the last 30 days). It is counted at most once a minute.

Two helpers are always there: `fmt(n, sep)` groups digits (`12 345`, with a
no-break space unless `sep` says otherwise), and `plural(n, one, few, many)`
picks a word form, the Russian way when given three forms and the English way
when given two.

The function returns Markdown. It may use everything a page may, the HTML
and the `wiki-card`, `wiki-grid` and `wiki-stat` classes included (see
[templates.md](templates.md#html-and-styles)).

## Calling a module

On a line of its own:

```markdown
:::Module:Stats:summary
```

With arguments, `key = value` lines and a closing `:::`:

```markdown
:::Module:Stats:cards
title = The wiki in numbers
:::
```

A template may call a module too, so a module can sit inside an infobox.

## Limits and safety

The code runs in boa, a JavaScript engine inside the wiki engine. It has no
access to files, the network or the database, and no `require`, `fetch` or
`process`: everything a module knows comes in through its three arguments.
A call may run a million loop iterations and nest 256 calls deep; the code
may be 64 KB and the result 64 KB; a page may make 20 calls. A result is kept
for a minute, so a busy page does not run its modules on every view.

When a call fails, the page shows a warning box with the module, the
function and the reason, and the rest of the page renders as usual.
