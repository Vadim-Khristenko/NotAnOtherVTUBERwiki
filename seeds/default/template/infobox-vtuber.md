<!-- title: Infobox VTuber -->
<params>
name: text
image: image
caption: text
aliases: list
debut: date
birthday: date
species: text
language: text
platform: text
affiliation: text
fans: text
model: text
emote: text
channel: link
website: link
</params>
<includeonly>:::infobox {{{name|}}}
{{#if:{{{image|}}}|![{{{name|}}}]({{{image}}})}}
{{#if:{{{caption|}}}|*{{{caption}}}*}}
{{#label:aliases|Also known as}} = {{{aliases|}}}
{{#label:debut|Debut}} = {{{debut|}}}
{{#label:birthday|Birthday}} = {{{birthday|}}}
{{#label:species|Species}} = {{{species|}}}
{{#label:language|Language}} = {{{language|}}}
{{#label:platform|Platform}} = {{{platform|}}}
{{#label:affiliation|Affiliation}} = {{{affiliation|}}}
{{#label:fans|Fans}} = {{{fans|}}}
{{#label:model|Model}} = {{{model|}}}
{{#label:emote|Emote}} = {{{emote|}}}
{{#label:channel|Channel}} = {{{channel|}}}
{{#label:website|Website}} = {{{website|}}}
:::</includeonly><noinclude>
The side card at the top of an article about a VTuber. Put it on the first
line of the article. Every field is optional: one left empty is not shown.

`image` takes an address from the wiki's own uploads (**Media** in the menu),
since outside images do not render. Values are Markdown, so links, bold text
and `:emotes:` work. The labels follow the page language: each translation of
this template brings its own words, the code stays the same.

Here it is with a few fields filled in:

{{Infobox VTuber
```yaml
name: Example VTuber
debut: 2021
platform: Twitch
fans: Snackers
```
}}
</noinclude>
