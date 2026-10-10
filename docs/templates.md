# Templates

A wiki has two kinds of templates.

- A **template** (`/template:infobox-vtuber`) is a component: an article calls
  it with `{{Name}}` and it becomes part of that article. An infobox is a
  template.
- A **page template** (`/page-template:vtuber-article`) lays out a whole new
  page. **New page** offers it, and its text is copied into the page being
  written. It is an ordinary article; `<!-- starter: Label -->` on its first
  line names it on New page, and without that line its title does. Text in
  `<noinclude>` stays on the page template and is not copied. Old links to
  `/template:` a page template lived at redirect to its new address.

Curators and up create and edit both kinds, since one edit changes many
pages. Everything below is about templates, the components. A page template
follows the rules of an article and is translated like one: where it has no
version in the reader's language, a new page starts from the one in the
wiki's language.

## The parts of a template

```
<!-- title: Infobox VTuber -->
<params>
name: text required
image: image
debut: date
fans: list
</params>
<includeonly>:::infobox {{{name|}}}
{{#label:debut|Debut}} = {{{debut|}}}
{{#label:fans|Fans}} = {{{fans|}}}
:::</includeonly>
<noinclude>
The side card at the top of an article about a VTuber.
</noinclude>
```

| Part | What it is |
|---|---|
| `<includeonly>` | The code: what a page that calls the template receives. |
| `<noinclude>` | The documentation, shown only on the template's own page. |
| `<params>` | The fields, one `name: kind` a line, with `required` when the call must fill it. Kinds: `text`, `image`, `date`, `number`, `link`, `list`. |
| `<labels>` | The words for the fields in one language (see below). |

`{{{name|}}}` is a field, empty when the call leaves it out;
`{{{name|default}}}` gives it a default. The template's page builds the list
of fields and ready examples of a call from `<params>`, and a call that
brings a field the template does not know, or leaves a required one empty,
gets a short note saying which.

Functions: `{{#if: test | then | else}}`, `{{#ifeq: a | b | then | else}}`,
`{{#switch: value | case = result | #default = other}}`, `{{#label: key | Default}}`
and `{{PAGELANGUAGE}}`.

## One template in every language

A template has one code. It lives in the **main version**: the one in the
wiki's own language, or the first one written when there is none in that
language yet. Every language runs that code.

A translation of a template changes only words: its documentation in
`<noinclude>` and the labels of its fields.

```
<noinclude>
Карточка сбоку в начале статьи о VTuber.
</noinclude>
<labels>
debut = Дебют
fans = Фанаты
</labels>
```

`{{#label:debut|Debut}}` in the code reads the label from the translation in
the page's language, and falls back to the text after the bar. A translation
that carries code of its own is refused when it is saved, with a link to the
main version, so translating a template can never turn it into a different
template.

A main version that ends up calling itself, directly or through another
template, is refused too: every page using it would show a loop note instead
of the template. An example of the template in its own `<noinclude>`
documentation is fine.

## Calling a template

A template answers to its address (`{{Infobox VTuber}}` is `template:infobox-vtuber`)
and to the title of any of its versions in this wiki, in any script:
`{{Карточка VTuber}}` calls the same template as `{{Infobox VTuber}}` once its
Russian version is titled so. Case, `_` and extra spaces do not matter, and
`Template:` or `Шаблон:` in front is optional. Only this wiki's templates answer.

With fields after bars:

```
{{Infobox VTuber
| name  = Filian
| debut = 2021
}}
```

Or with the fields as YAML, which reads and edits more easily for long
cards:

````
{{Infobox VTuber
```yaml
name: Filian
debut: 2021
fans:
  - Snackers
  - Cats
bio: |
  Two lines,
  kept as they are.
```
}}
````

The YAML is a strict subset: one flat set of `field: value` lines. A value
is plain text, text in `"double"` or `'single'` quotes, a `|` block (lines
kept) or `>` block (lines joined), or a list of `- item` lines, which reads
as the items joined by commas. `#` starts a comment. Anchors, aliases, tags,
inline `{}` and `[]` collections and nested fields are refused with the line
they are on, and the reason reads in the page's language.

Field names and values may be in any language (`дебют: 2021`, `名前: フィリアン`).
The full-width colon a Japanese or Chinese keyboard types works as a colon and
needs no space after it; a no-break space after a colon, Windows line ends and
a byte order mark are all fine.

## HTML and styles

A page or a template may write HTML from a fixed list of tags:

- structure: `div`, `span`, `section`, `article`, `aside`, `header`,
  `footer`, `p`, `h2` to `h6`, `hr`, `br`, `wbr`;
- text: `b`, `strong`, `i`, `em`, `u`, `s`, `del`, `ins`, `mark`,
  `small`, `sup`, `sub`, `abbr`, `cite`, `q`, `code`, `kbd`, `samp`,
  `var`, `time`, `data`, `bdi`, `ruby`, `rt`, `rp`, `a`;
- lists and tables: `ul`, `ol`, `li`, `dl`, `dt`, `dd`, `table`,
  `caption`, `colgroup`, `col`, `thead`, `tbody`, `tfoot`, `tr`,
  `th`, `td`;
- blocks: `blockquote`, `details`, `summary`, `figure`, `figcaption`;
- indicators: `progress` and `meter`.

A fragment with any other tag (a script, a style sheet, a frame, a form, SVG,
an `img`) is dropped whole. Pictures come from the wiki's own files, written
`![Alt](image:name.png)`, so a page never loads anything from elsewhere.

Attributes are cleaned: event handlers and `javascript:` links never survive.
Every tag may have `class`, `id`, `style`, `title`, `lang`, `dir`, `role`,
`aria-label` and `aria-hidden`; lists, `time`, `data`, `col` and the
indicators keep their own (`start`, `reversed`, `value`, `datetime`,
`max`, `span` and so on).

`style` keeps looks and layout inside the element:

- colours and backgrounds, gradients included (`linear-gradient(...)`);
- borders, their radius and colours per side, outlines and shadows;
- sizes and boxes: `width`, `height`, their `min-` and `max-`,
  `aspect-ratio`, `padding`, `margin` (never negative), `overflow`,
  `display`, `float`;
- flex and grid: `display: flex` or `grid`, `gap`, `grid-template-columns`,
  `justify-content`, `align-items` and the rest of the family;
- text: fonts (up to 3em, 300% or 48px), weights, alignment, decoration,
  spacing, wrapping, columns;
- lists and tables: markers, borders, layout.

Anything that positions, layers, transforms, animates or loads a file
(`url()`) is removed with its declaration. The skin's colours are variables:
`color: var(--accent)` follows the reader's theme, light or dark.

The skin also draws a few classes, so a template looks at home without
styles of its own: `wiki-card` (a framed box), `wiki-grid` (cards in a
grid that wraps on a phone), `wiki-badge` (a small pill), `wiki-stat` (a
big number over a label: `<div class="wiki-stat"><strong>140</strong><span>pages</span></div>`)
and `wiki-muted` (secondary text).
