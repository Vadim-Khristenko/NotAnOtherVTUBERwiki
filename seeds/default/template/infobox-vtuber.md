<!-- title: Infobox VTuber -->
<includeonly>:::infobox {{{name|}}}
{{#if:{{{image|}}}|![{{{name|}}}]({{{image}}})}}
{{#if:{{{caption|}}}|*{{{caption}}}*}}
Also known as = {{{aliases|}}}
Debut = {{{debut|}}}
Birthday = {{{birthday|}}}
Species = {{{species|}}}
Language = {{{language|}}}
Platform = {{{platform|}}}
Affiliation = {{{affiliation|}}}
Fans = {{{fans|}}}
Model = {{{model|}}}
Emote = {{{emote|}}}
Channel = {{{channel|}}}
Website = {{{website|}}}
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
and `:emotes:` work.

Here it is with a few fields filled in:

{{Infobox VTuber
| name     = Example VTuber
| debut    = 2021
| platform = Twitch
| fans     = Snackers
}}
</noinclude>
