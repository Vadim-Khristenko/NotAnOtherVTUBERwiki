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
plum surfaces, pink primary, violet secondary, dark mode by default
because streams run late. Flat sticky header with a solid pink rule and a left-ruled pullquote.
The whole skin is one `_theme.html` on top of the default templates. Light
mode keeps the same structure in pastel, for daytime lurking.

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

A skin only ships what it changes. Every template it leaves out comes from
`skins/default`, so a skin never falls behind when the default one gains a
page or a fix.

1. Create `skins/yours/` with a copy of `skins/default/_theme.html`.
2. Put a `<style>` block in it that sets the `:root` variables for light
   and dark, plus whatever details you restyle. The layout includes this file
   after its own styles, so your rules win without `!important`.
3. Add `favicon/` if the wiki should have its own icon.
4. Point `skin_dir` at your directory. Changes load on the fly.

`skins/snackers/_theme.html` is a complete example: one file, no copied
templates.

Only override a template (`layout.html`, `_header.html`, `page.html`,
`errors/<kind>.html` and so on) when markup has to change, not color. Keep the
class names the default skin uses; the renderer and the editor script
address those hooks.

Rules that keep every skin fast: no external requests (fonts, scripts,
images), no JavaScript on reader pages, honor `prefers-reduced-motion`,
and keep contrast readable in both themes. The engine never inlines
unsafe styles, and neither should a skin.
