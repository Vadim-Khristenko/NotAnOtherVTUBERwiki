<!-- title: Navbox -->
<params>
title: text required
items: list required
more: link
</params>
<includeonly><div class="wiki-navbox">

**{{{title|}}}** · {{{items|}}}{{#if:{{{more|}}}| · [{{#label:more|More}}]({{{more}}})}}

</div></includeonly><noinclude>
A box of links at the bottom of related articles, so a reader can go from one
to the next. List the links in `items`; `more` points to the category or the
page with all of them.

{{Navbox
```yaml
title: Example series
items:
  - "[First](home)"
  - "[Second](community)"
more: system:all-pages
```
}}
</noinclude>
