<!-- title: Infobox VTuber -->
<includeonly>:::infobox {{{name|}}}
{{#if:{{{image|}}}|![{{{name|}}}]({{{image}}})}}
{{#if:{{{caption|}}}|*{{{caption}}}*}}
{{#switch:{{PAGELANGUAGE}}|ru=Также известна как|#default=Also known as}} = {{{aliases|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Дебют|#default=Debut}} = {{{debut|}}}
{{#switch:{{PAGELANGUAGE}}|ru=День рождения|#default=Birthday}} = {{{birthday|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Вид|#default=Species}} = {{{species|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Язык|#default=Language}} = {{{language|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Платформа|#default=Platform}} = {{{platform|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Агентство|#default=Affiliation}} = {{{affiliation|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Фанаты|#default=Fans}} = {{{fans|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Модель|#default=Model}} = {{{model|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Эмоут|#default=Emote}} = {{{emote|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Канал|#default=Channel}} = {{{channel|}}}
{{#switch:{{PAGELANGUAGE}}|ru=Сайт|#default=Website}} = {{{website|}}}
:::</includeonly><noinclude>
The side card at the top of an article about a VTuber. Put it on the first
line of the article. Every field is optional: one left empty is not shown.

```
{{Infobox VTuber
| name        = Filian
| image       = /media/ab/cdef.png
| caption     = Filian, as drawn by the community
| aliases     = Fil
| debut       = 2021
| birthday    =
| species     =
| language    = English
| platform    = Twitch
| affiliation = Independent
| fans        = Snackers
| model       =
| emote       =
| channel     = [twitch.tv/filian](https://twitch.tv/filian)
| website     =
}}
```

`image` takes an address from the wiki's own uploads (**Media** in the menu),
since outside images do not render. Values are Markdown, so links, bold text
and `:emotes:` work. Field labels follow the page language: a Russian page reads
Дебют where an English one reads Debut.

Here it is with a few fields filled in:

{{Infobox VTuber
| name     = Example VTuber
| debut    = 2021
| platform = Twitch
| fans     = Snackers
}}
</noinclude>
