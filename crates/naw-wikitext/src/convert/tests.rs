use super::*;

fn md(wikitext: &str) -> String {
    convert(
        wikitext,
        &Options {
            title: "Bao the Whale".into(),
            ..Options::default()
        },
    )
    .markdown
}

fn report(wikitext: &str) -> Report {
    convert(wikitext, &Options::default()).report
}

#[test]
fn headings_bold_and_italic() {
    assert_eq!(
        md("== Early life ==\n'''Bao''' is ''a whale'' and '''''both'''''."),
        "## Early life\n\n**Bao** is *a whale* and ***both***.\n"
    );
}

#[test]
fn emphasis_left_open_closes_at_the_end_of_the_line() {
    assert_eq!(
        md("'''bold to the end\nnext"),
        "**bold to the end**\nnext\n"
    );
}

#[test]
fn emphasis_never_hugs_a_space() {
    assert_eq!(md("a ''' bold ''' b"), "a  **bold**  b\n");
}

#[test]
fn links_get_addresses_and_keep_their_text() {
    assert_eq!(
        md("See [[Neuro-sama]], [[Neuro-sama/Evil|Evil Neuro]] and [[Whale]]s."),
        "See [[neuro-sama|Neuro-sama]], [[neuro-sama-evil|Evil Neuro]] and [[whale|Whales]].\n"
    );
    assert_eq!(md("[[lore]]"), "[[lore]]\n");
    assert_eq!(
        md("[[Filian#Early life|her start]] and [[#Trivia]]"),
        "[[filian#early-life|her start]] and [Trivia](#trivia)\n"
    );
    assert_eq!(md("[[Bao (VTuber)|]]"), "[[bao-vtuber|Bao]]\n");
}

#[test]
fn categories_files_and_other_namespaces() {
    assert_eq!(
        md("[[Category:English VTubers|Bao]]\n[[:Category:VTubers|all of them]]"),
        "[[Category:english-vtubers]]\n[[:Category:vtubers|all of them]]\n"
    );
    assert_eq!(
        md("[[File:Bao 2.0 New.jpg|thumb|250px|right|Bao's '''new''' model]]"),
        "![Bao's new model](image:bao-2-0-new.jpg)\n"
    );
    assert_eq!(md("[[es:Bao]]"), "");
    assert_eq!(md("[[User:Someone|someone]]"), "someone\n");
    assert_eq!(
        md("[[wikipedia:52-hertz whale|the whale]]"),
        "[the whale](https://en.wikipedia.org/wiki/52-hertz_whale)\n"
    );
    let r = report("[[File:A.png]] [[User:X]]");
    assert!(r.files.contains("A.png"));
    assert!(r.unsupported.contains_key("link to user:"));
}

#[test]
fn external_links_and_bare_addresses() {
    assert_eq!(
        md("[https://twitch.tv/bao Twitch] and [https://x.com/a_(b)] and https://example.com/a*b."),
        "[Twitch](https://twitch.tv/bao) and <https://x.com/a_%28b%29> and https://example.com/a*b.\n"
    );
}

#[test]
fn lists_and_definitions() {
    assert_eq!(
        md("Intro\n* one\n** nested\n# first\n#* mixed\nAfter"),
        "Intro\n\n- one\n  - nested\n1. first\n   - mixed\n\nAfter\n"
    );
    assert_eq!(
        md("; Fan name\n: Baobble Buddies\n; Emoji : 🐳"),
        "Fan name\n: Baobble Buddies\n\nEmoji\n: 🐳\n"
    );
}

#[test]
fn footnotes_number_in_order_and_reuse_names() {
    let out = md(
        "One.<ref name=\"a\">Source A</ref> Two.<ref>Source B</ref> Again.<ref name=a /> \
         Later.<ref name=\"c\"/>\n\n== References ==\n<references>\n<ref name=\"c\">Source C</ref>\n</references>",
    );
    assert_eq!(
        out,
        "One.[^1] Two.[^2] Again.[^1] Later.[^3]\n\n## References\n\n[^1]: Source A\n[^2]: Source B\n[^3]: Source C\n"
    );
}

#[test]
fn templates_stay_and_are_counted() {
    let wikitext = "{{Character\n|title1 = Bao the Whale\n|nick = Ojou-sama<br>Bobo (by [[Neuro-sama]])\n}}\n\
                    {{Quote|I hope|Bao}}\n{{Reflist}}";
    let out = convert(wikitext, &Options::default());
    assert_eq!(
        out.markdown,
        "{{Character\n|title1 = Bao the Whale\n|nick = Ojou-sama<br>Bobo (by [[neuro-sama|Neuro-sama]])\n}}\n{{Quote|I hope|Bao}}\n"
    );
    assert_eq!(out.report.templates.get("character"), Some(&1));
    assert_eq!(out.report.templates.get("quote"), Some(&1));
}

#[test]
fn magic_words_and_parser_functions() {
    assert_eq!(md("{{PAGENAME}} is {{lc:LOUD}}"), "Bao the Whale is loud\n");
    assert_eq!(
        md("{{#ev:youtube|HSvxxKDv3Oc|360px|left|Self-Introduction}}"),
        "[Self-Introduction](https://www.youtube.com/watch?v=HSvxxKDv3Oc)\n"
    );
    assert_eq!(md("{{#if: x | yes | no }}"), "{{#if: x | yes | no }}\n");
    let r = report("{{#invoke:Foo|bar}}{{CURRENTYEAR}}");
    assert!(r.unsupported.contains_key("Lua module call"));
    assert!(r.unsupported.contains_key("{{CURRENTYEAR}}"));
}

#[test]
fn a_plain_table_becomes_markdown() {
    assert_eq!(
        md(
            "{| class=\"wikitable\"\n|+ Collabs\n! Name !! Date\n|-\n| [[Neuro-sama]] || 2025\n|-\n| style=\"color:red\" | Bao || 2024\n|}"
        ),
        "**Collabs**\n\n| Name | Date |\n|---|---|\n| [[neuro-sama|Neuro-sama]] | 2025 |\n| Bao | 2024 |\n"
    );
}

#[test]
fn a_table_with_spans_becomes_html_with_markdown_inside() {
    let out = md("{|\n! colspan=\"2\" | Both\n|-\n| '''a'''\n| b\n* list in a cell\n|}");
    assert!(
        out.starts_with("<table>\n<tr>\n<th colspan=\"2\">\n\nBoth\n\n</th>"),
        "{out}"
    );
    assert!(out.contains("<td>\n\n**a**\n\n</td>"), "{out}");
    assert!(out.contains("b\n\n- list in a cell"), "{out}");
    assert!(out.trim_end().ends_with("</table>"), "{out}");
}

#[test]
fn code_nowiki_and_preformatted_text_are_left_alone() {
    assert_eq!(
        md("<nowiki>'''not bold''' [[no link]]</nowiki>"),
        "'''not bold''' \\[\\[no link\\]\\]\n"
    );
    assert_eq!(
        md("Text\n code with '''x'''\n more\nAfter"),
        "Text\n\n```\ncode with '''x'''\nmore\n```\n\nAfter\n"
    );
    assert_eq!(
        md("<syntaxhighlight lang=\"rust\">\nfn main() {}\n</syntaxhighlight>"),
        "```rust\nfn main() {}\n```\n"
    );
    assert_eq!(md("Use <code>naw serve</code>."), "Use `naw serve`.\n");
}

#[test]
fn tabs_galleries_and_switches() {
    let out = md("__NOTOC__\n<tabber>\nTwitch=Hi on Twitch\n|-|\nYouTube=Hi on YouTube\n</tabber>");
    assert_eq!(
        out,
        ":::details Twitch\n\nHi on Twitch\n\n:::\n\n:::details YouTube\n\nHi on YouTube\n\n:::\n"
    );
    let gallery = md("<gallery>\nFile:A.png|First\nB.jpg\n</gallery>");
    assert_eq!(gallery, "![First](image:a.png)\n![](image:b.jpg)\n");
    assert_eq!(md("__TOC__\n== A =="), "[[toc]]\n\n## A\n");
}

#[test]
fn block_html_lines_let_the_markdown_inside_render() {
    assert_eq!(
        md("<div style=\"height:auto;\">\n'''Hi'''\n</div>\n<center>''Centered''</center>"),
        "<div style=\"height:auto;\">\n\n**Hi**\n\n</div>\n\n<div style=\"text-align: center\">\n\n*Centered*\n\n</div>\n"
    );
}

#[test]
fn text_that_reads_as_markdown_is_escaped() {
    assert_eq!(
        md("1. not a list\n5*3 ~ about ^ x"),
        "1\\. not a list\n5\\*3 \\~ about \\^ x\n"
    );
    assert_eq!(md("- dash"), "\\- dash\n");
    assert_eq!(md("a < b"), "a &lt; b\n");
}

#[test]
fn redirects_and_comments() {
    let out = convert(
        "#REDIRECT [[Bao the Whale#Lore]] <!-- old -->",
        &Options::default(),
    );
    assert_eq!(out.redirect.as_deref(), Some("Bao the Whale"));
    assert_eq!(md("Kept<!-- gone --> text<!-- unclosed"), "Kept text\n");
}

#[test]
fn a_converted_page_renders_cleanly() {
    let wikitext = "'''Bao''' is a [[whale]].<ref>Source</ref>\n\n== Trivia ==\n* Sings\n* Streams\n\n\
                    {| class=\"wikitable\"\n! A !! B\n|-\n| 1 || 2\n|}\n\n<div>\n''Quiet''\n</div>";
    let html = naw_markdown::render_html(&md(wikitext));
    assert!(html.contains("<strong>Bao</strong>"), "{html}");
    assert!(html.contains("<li>Sings</li>"), "{html}");
    assert!(html.contains("<th>A</th>"), "{html}");
    assert!(html.contains("<em>Quiet</em>"), "{html}");
    assert!(html.contains("footnote"), "{html}");
    assert!(
        !html.contains("'''") && !html.contains("[[") && !html.contains(OPEN),
        "{html}"
    );
}

#[test]
fn templates_the_engine_cannot_spell_are_renamed_and_tags_built() {
    let out = convert("{{Status/Legend|x}} {{MAHA5}}", &Options::default());
    assert_eq!(out.markdown, "{{status-legend|x}} {{MAHA5}}\n");
    assert_eq!(out.report.templates.get("status-legend"), Some(&1));
    assert_eq!(out.report.templates.get("maha5"), Some(&1));
    assert_eq!(
        md("Said.{{#tag:ref|A source|name=s}} Again.<ref name=\"s\"/>"),
        "Said.[^1] Again.[^1]\n\n[^1]: A source\n"
    );
    assert_eq!(
        md("{{#tag:tabber|Solo=One{{!}}-{{!}}Duo=Two}}"),
        ":::details Solo\n\nOne\n\n:::\n\n:::details Duo\n\nTwo\n\n:::\n"
    );
}

#[test]
fn a_portable_infobox_template_and_its_calls_carry_over() {
    let template = "<infobox>\n<title source=\"title1\"><default>{{PAGENAME}}</default></title>\n\
                    <image source=\"image1\"><caption source=\"caption1\"/></image>\n\
                    <group><header>Basic Details</header>\n<data source=\"debut\"><label>Debut Date</label></data></group>\n\
                    </infobox><noinclude>Example usage.</noinclude>";
    let options = Options {
        title: "Character".into(),
        template: true,
        ..Options::default()
    };
    let out = convert(template, &options);
    assert_eq!(
        out.markdown,
        ":::infobox {{{title1|}}}\n\n{{#if:{{{image1|}}}|![{{{caption1|}}}]({{{image1}}})}}\n\
         {{#if:{{{caption1|}}}|*{{{caption1}}}*}}\n**Basic Details**\nDebut Date = {{{debut|}}}\n\n:::\n\n\
         <noinclude>Example usage.</noinclude>\n"
    );
    let images = crate::image_params(template);
    let article = Options {
        title: "Bao".into(),
        image_params: BTreeMap::from([("character".to_string(), images)]),
        ..Options::default()
    };
    let call = convert(
        "{{Character\n|title1 = Bao\n|image1 = Bao 2.0 New.jpg\n|debut = 2020\n}}",
        &article,
    );
    assert_eq!(
        call.markdown,
        "{{Character\n|title1 = Bao\n|image1 = image:bao-2-0-new.jpg\n|debut = 2020\n}}\n"
    );
    assert!(call.report.files.contains("Bao 2.0 New.jpg"));
}

#[test]
fn template_sections_stay_for_the_engine() {
    assert_eq!(
        md("<includeonly>{{{1}}}</includeonly><noinclude>Docs</noinclude>"),
        "<includeonly>{{{1}}}</includeonly><noinclude>Docs</noinclude>\n"
    );
}

#[test]
fn unknown_tags_are_reported_and_their_text_kept() {
    let out = convert(
        "<mainpage-leftcolumn-start />Hello <youtube>x</youtube>",
        &Options::default(),
    );
    assert_eq!(out.markdown, "Hello x\n");
    assert!(
        out.report
            .unsupported
            .contains_key("tag <mainpage-leftcolumn-start>")
    );
    assert!(out.report.unsupported.contains_key("tag <youtube>"));
}
