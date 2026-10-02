<!-- title: Infobox song -->
<params>
title: text required
image: image
caption: text
artist: text
featuring: list
original: text
released: date
length: text
genre: text
language: text
listen: link
nocat: text
</params>
<includeonly>:::infobox {{{title|}}}
{{#if:{{{image|}}}|![{{{title|}}}]({{{image}}})}}
{{#if:{{{caption|}}}|*{{{caption}}}*}}
{{#label:artist|Artist}} = {{{artist|}}}
{{#label:featuring|Featuring}} = {{{featuring|}}}
{{#label:original|Original by}} = {{{original|}}}
{{#label:released|Released}} = {{{released|}}}
{{#label:length|Length}} = {{{length|}}}
{{#label:genre|Genre}} = {{{genre|}}}
{{#label:language|Language}} = {{{language|}}}
{{#label:listen|Listen}} = {{#if:{{{listen|}}}|[{{#label:link|Link}}]({{{listen}}})}}
:::
{{#if:{{{nocat|}}}||[[Category:Music]]}}</includeonly><noinclude>
The side card of an article about a song, an original or a cover. For a
cover, `original` names who first made it. The page goes to **Music**.

{{Infobox song
```yaml
nocat: yes
title: Example song
artist: Example artist
released: 2026-01-01
length: "3:30"
```
}}
</noinclude>
