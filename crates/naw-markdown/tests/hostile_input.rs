//! Everything people write reaches readers through `render_html`: articles,
//! templates, discussion messages, imported pages. These inputs try to get
//! script, event handlers or the engine's own markers past it.

use naw_markdown::render_html;

/// Tags that must never come out of the renderer.
const FORBIDDEN_TAGS: &[&str] = &[
    "script",
    "iframe",
    "object",
    "embed",
    "form",
    "input",
    "button",
    "meta",
    "link",
    "base",
    "style",
    "naw-module",
    "frame",
    "frameset",
    "applet",
    "textarea",
    "select",
];

/// Attributes that may not appear at all.
const FORBIDDEN_IN_TAGS: &[&str] = &["srcdoc=", "formaction="];

/// Attributes a browser follows as an address.
const URL_ATTRIBUTES: &[&str] = &[
    "href",
    "src",
    "action",
    "poster",
    "data",
    "xlink:href",
    "background",
];

/// Schemes that run code when followed.
const RUNNING_SCHEMES: &[&str] = &[
    "javascript:",
    "vbscript:",
    "data:text/html",
    "data:application",
];

/// The values of `tag`'s address attributes, quoted or not.
fn url_values(tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    for name in URL_ATTRIBUTES {
        let mut from = 0;
        let key = format!("{name}=");
        while let Some(at) = tag[from..].find(&key) {
            let start = from + at;
            let before = tag[..start].chars().last();
            from = start + key.len();
            if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':') {
                continue;
            }
            let rest = &tag[from..];
            let value = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
                _ => rest
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or(""),
            };
            out.push(value.to_string());
        }
    }
    out
}

/// Whether following `value` would run code: its scheme, once the
/// browser has dropped the blanks, entities and controls it ignores.
fn runs_code(value: &str) -> bool {
    let decoded = value
        .replace("&#x3a;", ":")
        .replace("&#58;", ":")
        .replace("&colon;", ":")
        .replace("&tab;", "")
        .replace("&newline;", "");
    let squeezed: String = decoded
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_lowercase();
    RUNNING_SCHEMES.iter().any(|s| squeezed.starts_with(s))
}

/// Text about a script is fine, escaped; a tag is not. So the check reads
/// the output's real tags, `<...>`, never the escaped `&lt;...&gt;`.
fn assert_clean(input: &str) {
    let html = render_html(input);
    let lower = html.to_lowercase();
    assert!(
        !lower.contains("<!--"),
        "a comment got through for {input:?}:\n{html}"
    );
    let mut rest = lower.as_str();
    while let Some(at) = rest.find('<') {
        let tag_end = rest[at..].find('>').map_or(rest.len(), |e| at + e + 1);
        let tag = &rest[at..tag_end];
        let name: String = tag
            .trim_start_matches('<')
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        assert!(
            !FORBIDDEN_TAGS.contains(&name.as_str()),
            "<{name}> got through for {input:?}:\n{html}"
        );
        for bad in FORBIDDEN_IN_TAGS {
            assert!(
                !tag.contains(bad),
                "{bad:?} in {tag:?} for {input:?}:\n{html}"
            );
        }
        for value in url_values(tag) {
            assert!(
                !runs_code(&value),
                "a running address {value:?} in {tag:?} for {input:?}:\n{html}"
            );
        }
        // An event handler: ` on…=` inside the tag.
        let handler = tag.split_whitespace().skip(1).any(|attr| {
            attr.starts_with("on")
                && attr[2..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .count()
                    > 0
                && attr.contains('=')
        });
        assert!(
            !handler,
            "an event handler in {tag:?} for {input:?}:\n{html}"
        );
        rest = &rest[tag_end..];
    }
}

#[test]
fn script_and_handlers_never_get_through() {
    for input in [
        "<script>alert(1)</script>",
        "<SCRIPT SRC=//x.example/x.js></SCRIPT>",
        "<img src=x onerror=alert(1)>",
        "<img src=\"/media/a.png\" onerror=\"alert(1)\">",
        "<svg onload=alert(1)><circle /></svg>",
        "<a href=\"javascript:alert(1)\">x</a>",
        "<a href=\"JaVaScRiPt:alert(1)\">x</a>",
        "<a href=\"java&#115;cript:alert(1)\">x</a>",
        "<a href=\" javascript:alert(1)\">x</a>",
        "[click](javascript:alert(1))",
        "[click](JAVASCRIPT:alert(1))",
        "[click](vbscript:msgbox(1))",
        "[click](data:text/html;base64,PHNjcmlwdD4=)",
        "![x](javascript:alert(1))",
        "<iframe src=\"https://x.example\"></iframe>",
        "<object data=x></object><embed src=x>",
        "<form action=/x><input name=a><button formaction=javascript:alert(1)>go</button></form>",
        "<div style=\"background:url(javascript:alert(1))\">x</div>",
        "<div style=\"width: expression(alert(1))\">x</div>",
        "<p onclick=\"alert(1)\">x</p>",
        "<details open ontoggle=alert(1)><summary>x</summary></details>",
        "<meta http-equiv=refresh content=0;url=javascript:alert(1)>",
        "<base href=javascript:alert(1)//>",
        "<style>body{display:none}</style>",
        "<math><mi xlink:href=\"javascript:alert(1)\">x</mi></math>",
        "<a href=\"//evil.example\" onmouseover=alert(1)>x</a>",
        "[[javascript:alert(1)|x]]",
        "[[  javascript:alert(1)]]",
        "<naw-module data-module=\"stats\" data-function=\"line\">x</naw-module>",
        "a <!--naw:post--> b",
        "<iframe srcdoc=\"<script>alert(1)</script>\"></iframe>",
        "```html\n<script>alert(1)</script>\n```\n",
        ":::details <script>alert(1)</script>\nbody\n:::\n",
        ":::infobox <img src=x onerror=alert(1)>\nKey = <script>x</script>\n:::\n",
        ":::tabs\n## <script>alert(1)</script>\nx\n:::\n",
        ":::navbox <svg onload=alert(1)>\nA = [x](javascript:alert(1))\n:::\n",
        ":::timeline <script>x</script>\n2021 | <img src=x onerror=alert(1)>\n:::\n",
        "| a | b |\n|---|---|\n| <script>x</script> | [x](javascript:alert(1)) |\n",
        "> [!NOTE] <img src=x onerror=alert(1)>\n",
        "Footnote[^a]\n\n[^a]: <script>alert(1)</script>\n",
    ] {
        assert_clean(input);
    }
}

#[test]
fn the_check_itself_sees_what_runs() {
    assert!(runs_code("javascript:alert(1)"));
    assert!(runs_code(" JaVa\tScRiPt:alert(1)"));
    assert!(runs_code("javascript&#x3a;alert(1)"));
    assert!(runs_code("data:text/html;base64,x"));
    assert!(!runs_code("/wiki/javascript:notes"));
    assert!(!runs_code("%20javascript:x"));
    assert_eq!(
        url_values("<a href=\"javascript:x\" title='y' src=z>"),
        vec!["javascript:x".to_string(), "z".to_string()]
    );
}

/// A small deterministic generator, so a failure always reproduces.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Pieces markup is made of, glued at random.
const PIECES: &[&str] = &[
    "<",
    ">",
    "</",
    "/>",
    "\"",
    "'",
    "=",
    "[",
    "]",
    "(",
    ")",
    "{{",
    "}}",
    "{{{",
    "}}}",
    "|",
    "!",
    "*",
    "_",
    "`",
    "```",
    "~",
    "^",
    "#",
    ":::",
    "\n",
    "\n\n",
    " ",
    "-",
    ">",
    "&",
    "&#",
    ";",
    "\\",
    "%",
    "script",
    "img",
    "src",
    "onerror",
    "javascript:",
    "href",
    "a",
    "svg",
    "style",
    "details",
    "infobox",
    "tabs",
    "timeline",
    "navbox",
    "hatnote",
    "[[",
    "]]",
    "[^",
    "![",
    "](",
    "@",
    "naw-module",
    "<!--",
    "-->",
    "\u{0}",
    "\u{1}",
    "é",
    "ж",
    "🐳",
    "\u{202e}",
    "\u{feff}",
    "\t",
    "1.",
    "- ",
    "> ",
];

#[test]
fn random_markup_never_lets_script_through_or_panics() {
    let mut rng = Lcg(0x5eed_cafe);
    for _ in 0..3000 {
        let len = 1 + rng.next() % 40;
        let mut input = String::new();
        for _ in 0..len {
            input.push_str(PIECES[(rng.next() as usize) % PIECES.len()]);
        }
        assert_clean(&input);
    }
}
