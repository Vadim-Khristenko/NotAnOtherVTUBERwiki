# Categories

A category gathers pages on one subject. A page joins one by saying so
anywhere in its text:

```
[[Category:VTubers]]
```

The line shows nothing where it is written; the page lists its categories
in a box at the bottom. `[[Категория:Витуберы]]` works the same way, and a
template can carry the line, so every page that uses an infobox can join the
infobox's category.

| You write | What it does |
|---|---|
| `[[Category:VTubers]]` | puts the page in VTubers |
| `[[Category:VTubers\|Filian]]` | the same, filed under "Filian" instead of the page title |
| `[[:Category:VTubers]]` | an ordinary link to the category, not a membership |

## Addresses

A category lives at `/category:` and its key: the name in lowercase, with
spaces as `-`, in any script. `[[Category:Snack Lore]]` is
`/category:snack-lore`, `[[Категория:Витуберы]]` is `/category:витуберы`.
Case, `_` and extra spaces do not matter, and `/категория:` works as a
prefix too: every spelling lands on the same address.

## Categories inside categories

A slash or a colon in the name makes levels:

```
[[Category:Streams/ARG]]
```

puts the page in ARG, which is inside Streams. `Streams:ARG` and
`Streams/Sub:ARG` mean the same. The key joins the levels with `:`, so the
address is `/category:streams:arg`, and `/category:streams/arg` leads there
as well.

Streams needs no page of its own for this: its page lists ARG among its
subcategories with how many pages are inside. A category page can also
join another category the ordinary way, with `[[Category:...]]` in its
description, and it shows up in that one's subcategories.

- `/category:streams` lists the pages in Streams itself, and its
  subcategories.
- `/category:streams/*`, or the switch "With everything inside it", lists
  the pages of Streams and of every category inside it.
- `/category:streams/arg/filian` opens the article `filian` when it is in
  Streams/ARG or inside it.

## The description

A category's description is an ordinary page at the category's address,
with its history and its translations. Write it from the category page
("Write a description"). Without one, the category still has its page:
the list of what is in it.

## Special pages

- `System:categories` lists every category with its size.
- `System:wanted-categories` lists the categories pages use that have no
  description yet.
- `System:uncategorized` lists articles in no category at all.
