# Skins

A skin is MiniJinja templates plus CSS variables. It changes how every wiki
looks without touching Rust, and a wiki can switch skins without a deploy.
The reader path stays server rendered HTML with zero JavaScript, so a skin
can never slow down reading. It can only dress it up.

## The lineup

### default: the engine reference

AMOLED white and AMOLED black, one blue accent, everything else neutral.
It is the skin the engine tests run against and the baseline every other
skin is compared to. Calm type, honest buttons, focus rings you can see.
If you build a skin, start by copying this one.

### snackers: home of the Snackers

The FilianWIKI skin, built for the Snackers community around Filian. Deep
plum surfaces, neon pink primary, violet secondary, dark mode by default
because streams run late. Sticky header with a pink to violet gradient
rule, a tilted gradient brand mark, pill buttons with a soft pink shadow
that lift on hover, rounded cards, and a gradient pullquote for the lines
worth quoting. It looks like a sticker wall at a convention: loud in the
right places, readable everywhere else. Light mode keeps the same energy
in pastel, for daytime lurking.

The snackers skin carries its own license terms, see
[snackers/LICENSE](snackers/LICENSE). Reuse outside this project needs
written permission.

## Customize through variables

Both skins build every component from the same variable names. Override
the values, keep the names, and the whole skin follows:

| Variable | What it paints |
|----------|----------------|
| `--bg`, `--bg-elevated` | Page and header backgrounds |
| `--surface`, `--surface-sunken` | Cards, code blocks, wells |
| `--ink`, `--ink-soft`, `--muted`, `--faint` | Text scale, body to whisper |
| `--line`, `--border-strong` | Hairlines and input borders |
| `--accent`, `--accent-ink`, `--accent-soft` | Links, primary buttons, selection |
| `--accent-2` | Snackers secondary gradient stop |
| `--radius`, `--content-width` | Corner roundness and measure |
| `--font-body`, `--font-mono` | System stacks, no webfonts, no requests |

Formatting elements (`spoiler`, `mark`, `kbd`, alerts, details,
pullquotes, diagrams, math) all consume these variables, so a recolor
reaches rendered articles with no extra work.

## Make your own

1. Copy `skins/default` to `skins/yours`.
2. Keep the template names (`layout.html`, `page.html`, `edit.html`,
   `404.html`, `_header.html`, `_footer.html`) and the class names inside
   them. The renderer and the editor JS address those hooks.
3. Repaint the `:root` variables, then restyle details in the polish layer
   at the bottom of `layout.html`.
4. Point `skin_dir` at your directory and reload. No rebuild needed.

Rules that keep every skin fast: no external requests (fonts, scripts,
images), no JavaScript on reader pages, honor `prefers-reduced-motion`,
and keep contrast readable in both themes. The engine never inlines
unsafe styles, and neither should a skin.
