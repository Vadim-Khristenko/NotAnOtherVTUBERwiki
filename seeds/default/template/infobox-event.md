<!-- title: Infobox event -->
<params>
name: text required
image: image
caption: text
date: date
end: date
place: text
organizer: text
participants: list
link: link
nocat: text
</params>
<includeonly>:::infobox {{{name|}}}
{{#if:{{{image|}}}|![{{{name|}}}]({{{image}}})}}
{{#if:{{{caption|}}}|*{{{caption}}}*}}
{{#label:date|Date}} = {{{date|}}} {{#if:{{{end|}}}|… {{{end}}}}}
{{#label:place|Where}} = {{{place|}}}
{{#label:organizer|Organized by}} = {{{organizer|}}}
{{#label:participants|Taking part}} = {{{participants|}}}
{{#label:link|Announcement}} = {{#if:{{{link|}}}|[{{#label:open|Open}}]({{{link}}})}}
:::
{{#if:{{{nocat|}}}||[[Category:Events]]}}</includeonly><noinclude>
The side card of an article about an event: a subathon, a birthday stream, a
collab, a meet-up or a community project. `end` is for events that last
several days. The page goes to **Events**.

{{Infobox event
```yaml
nocat: yes
name: Example event
date: 2026-01-01
end: 2026-01-03
place: Twitch
participants:
  - Example guest
```
}}
</noinclude>
