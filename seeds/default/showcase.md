# Formatting showcase

Every style the {wiki_name} editor understands, on one page. If it renders
here, it renders the same in preview and after saving: one renderer serves
both.

[[toc]]

## Inline styles

**Bold**, *italic*, __italic with double underscores__, ~~gone~~,
++underline++, ==highlight==, ==red|hot==, ==blue|cool==, ||spoiler with **bold** inside||,
`code`, ((Ctrl+C)) keys, and shortcodes like :fire: :tada: :sparkles:.
Times like 12:30 and links like https://example.test/docs stay literal.

Small notes: H ~2~ O for subscript, E=mc ^2^ for superscript.

## Quotes

> A plain quote.

>! Click to expand
> The hidden half of the quote.

:::details Lore vault
Secret **history** with a [link](/about) inside.
:::

:::pullquote
She is fast.
:::

> [!NOTE]
> Notes, tips, important notes, warnings and cautions each get a color.

## Lists and tasks

- First
- Second
  - Nested

1. Numbered
2. Steps

- [x] Done
- [ ] Todo

Term
  : Definition of the term.

## Tables

| Left | Center | Right |
|:--|:--:|--:|
| lore | VODs | art |
| memes | clips | music |

## Anchors and footnotes

Two headings can share a name and both stay reachable:

## Echo

First echo.

## Echo

Second echo lands on `#echo-2`.

A heading called `1` and footnote `[^1]` never share an anchor: the
heading keeps `#1` while the footnote lives at `#fn-1`. Text[^1].

[^1]: Footnotes live at the bottom and link back and forth.

## Math and diagrams

Einstein says $E=mc^2$ inline, and bigger ideas get a display line:

$$x + y$$

```mermaid
graph TD;
```

Diagrams keep their text until the worker renders them as pictures.

## Links

See [[home|the home page]] or [About](/about).
