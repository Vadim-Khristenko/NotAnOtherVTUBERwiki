<!-- title: Update -->
<params>
reason: text
since: date
nocat: text
</params>
<includeonly><div class="wiki-notice wiki-notice-update">

**{{#label:title|This page must be updated.}}** {{#if:{{{reason|}}}|{{{reason}}}|{{#label:text|Some of what it says is out of date. Check it against the latest streams and sources, then remove this notice.}}}} {{#if:{{{since|}}}|*{{#label:since|Since}} {{{since}}}.*}}

</div>

{{#if:{{{nocat|}}}||[[Category:Maintenance/Pages to update]]}}</includeonly><noinclude>
A notice at the top of a page whose facts have gone stale. It puts the page
in **Maintenance/Pages to update**, so somebody can find it and fix it.

`reason` says what is out of date, `since` from when. `nocat=yes` shows the
notice without the category, as in the example below.

{{Update|reason=The schedule section still lists last year's streams.|since=2026-09|nocat=yes}}
</noinclude>
