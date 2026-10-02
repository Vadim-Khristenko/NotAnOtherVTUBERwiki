<!-- title: Infobox stream -->
<params>
title: text required
image: image
caption: text
date: date
platform: text
duration: text
series: text
games: list
guests: list
vod: link
previous: link
next: link
nocat: text
</params>
<includeonly>:::infobox {{{title|}}}
{{#if:{{{image|}}}|![{{{title|}}}]({{{image}}})}}
{{#if:{{{caption|}}}|*{{{caption}}}*}}
{{#label:date|Date}} = {{{date|}}}
{{#label:platform|Platform}} = {{{platform|}}}
{{#label:duration|Length}} = {{{duration|}}}
{{#label:series|Series}} = {{{series|}}}
{{#label:games|Played}} = {{{games|}}}
{{#label:guests|Guests}} = {{{guests|}}}
{{#label:vod|Recording}} = {{#if:{{{vod|}}}|[{{#label:watch|Watch}}]({{{vod}}})}}
{{#label:previous|Previous}} = {{{previous|}}}
{{#label:next|Next}} = {{{next|}}}
:::
{{#if:{{{nocat|}}}||[[Category:Streams]]}}</includeonly><noinclude>
The side card of an article about one stream or a special broadcast. Link the
recording in `vod` when there is one; `previous` and `next` take links to the
streams around it in a series. The page goes to **Streams**.

{{Infobox stream
```yaml
nocat: yes
title: Example stream
date: 2026-01-01
platform: Twitch
duration: 4 h
games:
  - Example game
```
}}
</noinclude>
