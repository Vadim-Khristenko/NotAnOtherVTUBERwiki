<!-- title: Quote -->
<params>
1: text required
by: text
source: link
</params>
<includeonly>:::pullquote
{{{1}}}

{{#if:{{{by|}}}|<cite>{{{by}}}{{#if:{{{source|}}}|, [{{#label:source|source}}]({{{source}}})}}</cite>}}
:::</includeonly><noinclude>
A quote set apart from the text, with who said it and where. Quote only what
was really said, word for word, and link the stream or clip in `source`:

```
{{Quote|The words exactly as they were said.|by=Who said them|source=https://example.com/clip}}
```

{{Quote|The words exactly as they were said.|by=Who said them}}
</noinclude>
