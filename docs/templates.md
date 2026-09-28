# Templates

A wiki has two kinds of pages under `template:`.

- A **template** is a component: an article calls it with `{{Name}}` and it
  becomes part of that article. An infobox is a template.
- A **starter** lays out a whole new page. **New page** offers it, and its
  text is copied into the page being written. A starter is an ordinary
  article with `<!-- starter: Label -->` on its first line.

Everything below is about templates, the components. A starter follows the
rules of an article and is translated like one.

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

A page or a template may write HTML from a fixed list of tags: `div`,
`span`, `p`, `small`, `sup`, `sub`, `b`, `i`, `u`, `s`, `mark`, `abbr`,
`code`, `kbd`, `br`, `hr`, tables (`table`, `caption`, `thead`, `tbody`,
`tfoot`, `tr`, `th`, `td`), lists (`ul`, `ol`, `li`, `dl`, `dt`, `dd`),
`blockquote`, `q`, `cite`, `details`, `summary`, `figure`, `figcaption`,
`ruby`, `rt`, `rp`, `del`, `ins`, `bdi` and `a`. A fragment with any other
tag (a script, a style sheet, a frame, a form, SVG) is dropped whole.

Attributes are cleaned: event handlers and `javascript:` links never survive.
`style` keeps looks only: colors, borders and their radius, shadows, fonts
within reason (up to 3em, 300% or 48px), text alignment and decoration,
padding and margins (no negative margins), `width`, `max-width`, `float`,
`clear`, `opacity`. Anything that positions, layers, transforms, animates or
loads a file (`url()`) is removed with its declaration.
