//! Readable text extraction for the optional raw-content results.
//!
//! This is not a full readability implementation: it takes the largest of the
//! article/main containers, falls back to the body, drops script and style
//! text, and normalises the whitespace, which is enough for an agent to read
//! the page behind a search result.
//!
//! Block boundaries (paragraphs, headings, list items, table rows, `<br>`)
//! become line breaks, and `<pre>` blocks keep their own lines and
//! indentation, so code, configuration files and lists stay usable.

use scraper::{ElementRef, Html};

/// Longest text returned per page, in characters, unless the caller asks for
/// a different limit.
const MAX_CHARS: usize = 4_000;

/// Containers that may hold the article body.
const CONTAINERS: &str = "article, main, [role=main]";

/// Elements whose text is markup or scripting rather than readable content.
const NON_CONTENT: &[&str] = &["script", "style", "template", "noscript"];

/// The readable text of a parsed HTML document.
pub fn main_text(html: &Html) -> String {
    main_text_limited(html, MAX_CHARS)
}

/// The readable text of a parsed HTML document, capped at the given length.
///
/// The container is chosen before the cap applies, so a long article is not
/// lost to a shorter sibling that happens to come first in the document.
pub fn main_text_limited(html: &Html, max: usize) -> String {
    let text = largest_container(html)
        .or_else(|| body_text(html))
        .unwrap_or_default();
    truncate(&text, max)
}

/// The readable text of the largest article-like container that has any.
fn largest_container(html: &Html) -> Option<String> {
    let selector = scraper::Selector::parse(CONTAINERS).ok()?;
    html.select(&selector)
        .map(readable_text)
        .filter(|text| !text.is_empty())
        .max_by_key(|text| text.chars().count())
}

fn body_text(html: &Html) -> Option<String> {
    let selector = scraper::Selector::parse("body").ok()?;
    html.select(&selector)
        .map(readable_text)
        .find(|text| !text.is_empty())
}

fn readable_text(element: ElementRef<'_>) -> String {
    let mut segments = Vec::new();
    collect_text(element, &mut segments);
    join_lines(&segments)
}

/// A piece of extracted text, before whitespace is normalised.
enum Segment {
    /// Flowing text whose whitespace collapses.
    Flow(String),
    /// A block boundary: the following text starts on a new line.
    Break,
    /// Preformatted text whose lines and indentation are kept.
    Pre(String),
}

/// Elements that start and end a line of their own.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "section",
    "summary",
    "tr",
    "ul",
];

/// A step of the walk: visit a node, or close the block it opened.
enum Step<N> {
    Enter(N),
    Break,
}

/// Collect every text node under the element except non-content subtrees,
/// marking block boundaries and keeping `<pre>` text verbatim.
///
/// The walk is iterative so that a deeply nested page cannot overflow the
/// stack of the process that extracts its text.
fn collect_text(element: ElementRef<'_>, out: &mut Vec<Segment>) {
    let mut stack = vec![Step::Enter(*element)];
    while let Some(step) = stack.pop() {
        let node = match step {
            Step::Break => {
                out.push(Segment::Break);
                continue;
            }
            Step::Enter(node) => node,
        };
        if let Some(text) = node.value().as_text() {
            out.push(Segment::Flow(text.to_string()));
            continue;
        }
        let Some(element) = ElementRef::wrap(node) else {
            stack.extend(node.children().rev().map(Step::Enter));
            continue;
        };
        let name = element.value().name();
        if NON_CONTENT.contains(&name) {
            continue;
        }
        match name {
            "br" => out.push(Segment::Break),
            "pre" => out.push(Segment::Pre(preformatted_text(element))),
            "table" => out.push(Segment::Pre(render_table(element))),
            "math" => out.push(Segment::Flow(math_text(element))),
            _ if repeats_a_formula(&element)
                || is_page_chrome(&element)
                || is_boilerplate(&element) => {}
            _ if BLOCKS.contains(&name) => {
                out.push(Segment::Break);
                stack.push(Step::Break);
                stack.extend(node.children().rev().map(Step::Enter));
            }
            _ => stack.extend(node.children().rev().map(Step::Enter)),
        }
    }
}

/// Navigation, footers and explicitly hidden subtrees are page chrome, not
/// article text. Only structural signals are used: class names such as
/// `related` or `social` also appear in legitimate prose.
fn is_page_chrome(element: &ElementRef<'_>) -> bool {
    let value = element.value();
    if value.name() == "nav" {
        return true;
    }
    if value.name() == "footer" && !attributes_a_quotation(element) {
        return true;
    }
    if value.attr("hidden").is_some() {
        return true;
    }
    if value
        .attr("aria-hidden")
        .is_some_and(|hidden| hidden.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    value.attr("role").is_some_and(|role| {
        matches!(
            role.to_ascii_lowercase().as_str(),
            "navigation" | "banner" | "contentinfo" | "search"
        )
    })
}

/// A `<footer>` inside a quotation or a figure is the attribution of that
/// block rather than the footer of the page, so it is part of the text.
fn attributes_a_quotation(element: &ElementRef<'_>) -> bool {
    element
        .ancestors()
        .filter_map(|node| node.value().as_element())
        .any(|ancestor| matches!(ancestor.name(), "blockquote" | "figure"))
}

/// Names that mark advertising, a consent banner or a subscription box. They
/// are unambiguous where words such as `related` or `social` also appear in
/// legitimate prose, so only these are matched against `class` and `id`.
const BOILERPLATE_NAMES: &[&str] = &[
    "ad",
    "ads",
    "adslot",
    "advert",
    "advertisement",
    "sponsor",
    "sponsored",
    "cookie",
    "consent",
    "gdpr",
    "newsletter",
    "subscribe",
];

/// An advertising, consent or subscription block that no structural signal
/// marks: pages name these in `class` and `id` instead of using `role`.
fn is_boilerplate(element: &ElementRef<'_>) -> bool {
    ["class", "id"].iter().any(|attribute| {
        element.value().attr(attribute).is_some_and(|names| {
            names.split_whitespace().any(|name| {
                name.split('-').next().is_some_and(|head| {
                    BOILERPLATE_NAMES.contains(&head.to_ascii_lowercase().as_str())
                })
            })
        })
    })
}

/// Renderers paint a formula twice: KaTeX keeps the MathML plus a visual HTML
/// copy, MathJax adds an assistive copy. Only one of them may reach the text.
fn repeats_a_formula(element: &ElementRef<'_>) -> bool {
    if matches!(
        element.value().name(),
        "mjx-assistive-mml" | "mjx-assistive"
    ) {
        return true;
    }
    element.value().attr("class").is_some_and(|classes| {
        classes
            .split_whitespace()
            .any(|class| matches!(class, "katex-html" | "MJX-TEX"))
    })
}

/// The readable text of one formula: the original TeX annotation when the
/// renderer left one, else the concatenated MathML text.
fn math_text(math: ElementRef<'_>) -> String {
    static ANNOTATION: std::sync::OnceLock<scraper::Selector> = std::sync::OnceLock::new();
    let selector = ANNOTATION.get_or_init(|| {
        scraper::Selector::parse("annotation").expect("static annotation selector")
    });
    if let Some(annotation) = math.select(selector).next() {
        let tex = normalise(&annotation.text().collect::<String>());
        if !tex.is_empty() {
            return tex;
        }
    }
    normalise(&math.text().collect::<String>())
}

/// The text of a `<pre>` subtree as written, without non-content subtrees;
/// `<br>` inside it is a line break. Syntax highlighting splits code into many
/// inline elements, so text nodes are joined without separators.
fn preformatted_text(pre: ElementRef<'_>) -> String {
    let mut out = String::new();
    let mut stack = vec![*pre];
    while let Some(node) = stack.pop() {
        if let Some(text) = node.value().as_text() {
            out.push_str(text);
            continue;
        }
        match node.value().as_element().map(|element| element.name()) {
            Some(name) if NON_CONTENT.contains(&name) => {}
            Some("br") => out.push('\n'),
            _ => stack.extend(node.children().rev()),
        }
    }
    out
}

/// Largest table this renderer keeps: extra rows and columns are dropped so a
/// pathological page cannot turn extraction into unbounded text.
const MAX_TABLE_ROWS: usize = 200;
const MAX_TABLE_COLUMNS: usize = 32;
const MAX_TABLE_CELL_CHARS: usize = 160;

/// Render a table as Markdown-style rows so the columns stay readable.
///
/// Row and column boundaries are the information an AI agent needs from a
/// table; the flat text of a wide table otherwise reads as one run of words.
fn render_table(table: ElementRef<'_>) -> String {
    let mut rows = Vec::new();
    collect_rows(table, &mut rows, 0);
    let mut lines = Vec::new();
    // The caption names the table; the row walk never reaches it.
    if let Some(caption) = table
        .children()
        .filter_map(ElementRef::wrap)
        .find(|child| child.value().name() == "caption")
        .map(table_cell_text)
        .filter(|text| !text.is_empty())
    {
        lines.push(caption);
    }
    for (index, row) in rows.iter().take(MAX_TABLE_ROWS).enumerate() {
        let cells: Vec<ElementRef<'_>> = row
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|cell| matches!(cell.value().name(), "td" | "th"))
            .collect();
        if cells.is_empty() {
            continue;
        }
        let header = cells.iter().all(|cell| cell.value().name() == "th");
        let text: Vec<String> = cells
            .iter()
            .take(MAX_TABLE_COLUMNS)
            .map(|cell| table_cell_text(*cell))
            .collect();
        lines.push(format!("| {} |", text.join(" | ")));
        if header && index == 0 {
            lines.push(format!("|{}", " --- |".repeat(text.len())));
        }
    }
    lines.join("\n")
}

/// Rows of `table` in document order, ignoring rows of nested tables.
fn collect_rows<'a>(element: ElementRef<'a>, rows: &mut Vec<ElementRef<'a>>, depth: usize) {
    if depth > 8 {
        return;
    }
    for child in element.children() {
        let Some(child) = ElementRef::wrap(child) else {
            continue;
        };
        match child.value().name() {
            "tr" => rows.push(child),
            "table" => {}
            _ => collect_rows(child, rows, depth + 1),
        }
    }
}

/// One cell on a single line: pipes escaped, newlines flattened, bounded.
fn table_cell_text(cell: ElementRef<'_>) -> String {
    let text = readable_text(cell);
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let escaped = flat.replace('|', "\\|");
    truncate(&escaped, MAX_TABLE_CELL_CHARS)
}

/// Join the segments into lines: flowing text is normalised line by line and
/// empty lines are dropped. Preformatted blocks keep their lines and
/// indentation, minus trailing spaces and the blank lines around them.
fn join_lines(segments: &[Segment]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let flush = |line: &mut String, lines: &mut Vec<String>| {
        let collapsed = tidy(&normalise(line));
        if !collapsed.is_empty() {
            lines.push(collapsed);
        }
        line.clear();
    };
    for segment in segments {
        match segment {
            Segment::Flow(text) => {
                line.push_str(text);
                line.push(' ');
            }
            Segment::Break => flush(&mut line, &mut lines),
            Segment::Pre(text) => {
                flush(&mut line, &mut lines);
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                let pre: Vec<&str> = text.lines().map(str::trim_end).collect();
                let first = pre.iter().position(|line| !line.is_empty());
                let last = pre.iter().rposition(|line| !line.is_empty());
                if let (Some(first), Some(last)) = (first, last) {
                    lines.extend(pre[first..=last].iter().map(|line| line.to_string()));
                }
            }
        }
    }
    flush(&mut line, &mut lines);
    lines.join("\n")
}

/// Drop the space that joining inline elements leaves before closing
/// punctuation (`config.yaml :` becomes `config.yaml:`).
fn tidy(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut pending = false;
    for character in line.chars() {
        if character == ' ' {
            pending = true;
            continue;
        }
        if pending {
            let tight = matches!(
                character,
                '.' | ',' | ':' | ';' | '!' | '?' | ')' | ']' | '%'
            );
            if !tight && !out.is_empty() {
                out.push(' ');
            }
            pending = false;
        }
        out.push(character);
    }
    out
}

/// Collapse whitespace and drop empty lines.
pub(crate) fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_wins_over_body() {
        let html = Html::parse_document(
            "<html><body><nav>menu</nav><article><p>Hello   world</p></article></body></html>",
        );
        assert_eq!(main_text(&html), "Hello world");
    }

    #[test]
    fn plain_body_is_the_fallback() {
        let html = Html::parse_document("<html><body><p>Just text</p></body></html>");
        assert_eq!(main_text(&html), "Just text");
    }

    #[test]
    fn long_text_is_truncated() {
        let body = "a".repeat(MAX_CHARS + 100);
        let html = Html::parse_document(&format!("<html><body><p>{body}</p></body></html>"));
        let text = main_text(&html);
        assert!(text.chars().count() <= MAX_CHARS + 1);
    }

    #[test]
    fn largest_article_wins_over_a_leading_advertisement() {
        let article = "本記事".repeat(50);
        let html = Html::parse_document(&format!(
            "<html><body><article><p>広告</p></article><article><p>{article}</p></article></body></html>"
        ));
        assert_eq!(main_text(&html), article);
    }

    #[test]
    fn script_style_template_and_noscript_are_excluded() {
        let html = Html::parse_document(
            "<html><body><article><p>本文</p><script>secret=1</script>\
             <style>.ad{color:red}</style><template><p>雛形</p></template>\
             <noscript>JSなし</noscript><p>続き</p></article></body></html>",
        );
        assert_eq!(main_text(&html), "本文\n続き");
    }

    #[test]
    fn body_fallback_also_excludes_scripts() {
        let html =
            Html::parse_document("<html><body><script>secret=1</script><p>本文</p></body></html>");
        assert_eq!(main_text(&html), "本文");
    }

    #[test]
    fn limit_applies_after_picking_the_largest_article() {
        let long = "x".repeat(40);
        let html = Html::parse_document(&format!(
            "<html><body><article><p>短い</p></article><article><p>{long}</p></article></body></html>"
        ));
        assert_eq!(main_text_limited(&html, 10), "xxxxxxxxxx…");
    }

    #[test]
    fn blocks_become_lines_and_inline_text_stays_on_one_line() {
        let html = Html::parse_document(
            "<html><body><article><h2>설치</h2><p>먼저 <b>tmux</b>를   설치한다.</p>\
             <ul><li>하나</li><li>둘<br>셋</li></ul>\
             <table><tr><td>키</td><td>값</td></tr><tr><td>a</td><td>1</td></tr></table>\
             </article></body></html>",
        );
        assert_eq!(
            main_text(&html),
            "설치\n먼저 tmux 를 설치한다.\n하나\n둘\n셋\n| 키 | 값 |\n| a | 1 |"
        );
    }

    #[test]
    fn preformatted_code_keeps_lines_and_indentation() {
        let html = Html::parse_document(
            "<html><body><article><p>Put this in <code>config.yaml</code>:</p>\
             <pre><code>\n<span class=\"k\">server</span>:\n  port: <span>8080</span>\n    \n  \
             host: 0.0.0.0   \n</code></pre><p>Then restart.</p></article></body></html>",
        );
        assert_eq!(
            main_text(&html),
            "Put this in config.yaml:\nserver:\n  port: 8080\n\n  host: 0.0.0.0\nThen restart."
        );
    }

    #[test]
    fn tables_keep_rows_columns_and_a_header_separator() {
        let html = Html::parse_document(
            "<html><body><article><table>\
             <thead><tr><th>Name</th><th>Default</th></tr></thead>\
             <tbody><tr><td><code>--port</code></td><td>29483</td></tr>\
             <tr><td>--bind</td><td>127.0.0.1</td></tr></tbody>\
             </table></article></body></html>",
        );
        assert_eq!(
            main_text(&html),
            "| Name | Default |\n| --- | --- |\n| --port | 29483 |\n| --bind | 127.0.0.1 |"
        );
    }

    #[test]
    fn table_cells_escape_pipes_and_stay_bounded() {
        let long = "x".repeat(400);
        let html = Html::parse_document(&format!(
            "<html><body><article><table><tr><td>a|b</td><td>{long}</td></tr></table></article></body></html>"
        ));
        let text = main_text(&html);
        assert!(text.contains("| a\\|b |"), "{text}");
        assert!(text.contains('…'), "a long cell is truncated: {text}");
        assert!(text.lines().next().unwrap().chars().count() < 250);
    }

    #[test]
    fn a_nested_table_does_not_repeat_its_rows() {
        let html = Html::parse_document(
            "<html><body><article><table><tr><td>outer<div><table><tr><td>inner</td></tr></table></div>\
             </td><td>second</td></tr></table></article></body></html>",
        );
        let text = main_text(&html);
        assert_eq!(text.matches("inner").count(), 1, "{text}");
        assert!(text.contains("outer"), "{text}");
        assert!(text.contains("second"), "{text}");
    }

    #[test]
    fn page_chrome_is_not_part_of_the_article_text() {
        let html = Html::parse_document(
            "<html><body>\
             <nav><a href=\"/\">Home</a><a href=\"/docs\">Docs</a></nav>\
             <p>The main paragraph.</p>\
             <footer>Copyright 2026 Example</footer>\
             <div hidden>hidden text</div>\
             <div aria-hidden=\"true\">decorative text</div>\
             <div role=\"navigation\">menu text</div>\
             </body></html>",
        );
        let text = main_text(&html);
        assert_eq!(text, "The main paragraph.");
    }

    #[test]
    fn an_article_header_is_kept_even_though_page_chrome_is_not() {
        let html = Html::parse_document(
            "<html><body><nav>menu</nav><article><header><h1>Ownership</h1></header>\
             <p>Ownership is the core idea.</p><footer>tags: rust</footer></article>\
             <footer>site footer</footer></body></html>",
        );
        let text = main_text(&html);
        assert!(text.contains("Ownership"), "{text}");
        assert!(text.contains("Ownership is the core idea."), "{text}");
        assert!(!text.contains("menu"), "{text}");
        assert!(!text.contains("site footer"), "{text}");
    }

    #[test]
    fn a_footer_inside_a_quotation_is_attribution_not_page_chrome() {
        let html = Html::parse_document(
            "<html><body><article><figure><blockquote><p>Quoted.</p>\
             <footer>— A. Editor, <cite>Notes on Style</cite>, 1998</footer></blockquote></figure>\
             <footer>tags: rust</footer></article><footer>site footer</footer></body></html>",
        );
        let text = main_text(&html);
        assert!(text.contains("— A. Editor, Notes on Style, 1998"), "{text}");
        assert!(!text.contains("tags: rust"), "{text}");
        assert!(!text.contains("site footer"), "{text}");
    }

    #[test]
    fn named_advertising_consent_and_subscription_blocks_are_not_readable_text() {
        let html = Html::parse_document(
            "<html><body><article><p>Body</p>\
             <div class=\"ad-slot\">Buy the widget</div>\
             <aside class=\"newsletter-signup\">Subscribe weekly</aside>\
             <div id=\"cookie-banner\">We use cookies</div>\
             <div class=\"sponsor\">Sponsored by WidgetCo</div>\
             <div class=\"advice\">Advice stays.</div></article></body></html>",
        );
        let text = main_text(&html);
        assert!(text.contains("Body"), "{text}");
        assert!(text.contains("Advice stays."), "{text}");
        for dropped in [
            "Buy the widget",
            "Subscribe weekly",
            "We use cookies",
            "Sponsored by WidgetCo",
        ] {
            assert!(!text.contains(dropped), "{dropped} kept: {text}");
        }
    }

    #[test]
    fn footnote_references_and_their_notes_both_survive() {
        let html = Html::parse_document(
            "<html><body><article><p>A claim<sup><a href=\"#fn1\" id=\"r1\">[1]</a></sup>.</p>\
             <ol><li id=\"fn1\">The source.</li></ol></article></body></html>",
        );
        let text = main_text(&html);
        assert!(text.contains("A claim [1]."), "{text}");
        assert!(text.contains("The source."), "{text}");
        assert_eq!(text.matches("[1]").count(), 1, "{text}");
    }

    #[test]
    fn definition_lists_keep_terms_and_definitions_on_their_own_lines() {
        let html = Html::parse_document(
            "<html><body><article><dl><dt>port</dt><dd>The listening port.</dd>\
             <dt>bind</dt><dd>The listening address.</dd></dl></article></body></html>",
        );
        assert_eq!(
            main_text(&html),
            "port\nThe listening port.\nbind\nThe listening address."
        );
    }

    #[test]
    fn a_table_caption_is_kept_above_the_rows() {
        let html = Html::parse_document(
            "<html><body><article><table><caption>Table 1: Ports</caption>\
             <tr><th>Name</th><th>Port</th></tr><tr><td>http</td><td>29483</td></tr>\
             </table></article></body></html>",
        );
        assert_eq!(
            main_text(&html),
            "Table 1: Ports\n| Name | Port |\n| --- | --- |\n| http | 29483 |"
        );
    }

    #[test]
    fn wide_tables_are_cut_at_the_column_limit() {
        let cells = "<td>c</td>".repeat(40);
        let html = Html::parse_document(&format!(
            "<html><body><article><table><tr>{cells}</tr></table></article></body></html>"
        ));
        let text = main_text(&html);
        let columns = text.matches("| c ").count();
        assert_eq!(columns, 32, "columns are capped: {text}");
    }

    #[test]
    fn katex_markup_yields_the_formula_once() {
        let html = Html::parse_document(
            "<html><body><article><p>Euler:</p><span class=\"katex\">\
             <span class=\"katex-mathml\"><math><semantics><mrow><msup><mi>e</mi></msup></mrow>\
             <annotation encoding=\"application/x-tex\">e^{i\\pi}</annotation></semantics></math></span>\
             <span class=\"katex-html\" aria-hidden=\"true\"><span class=\"base\">\
             <span class=\"mord\">e</span><span class=\"mord\">i</span><span class=\"mord\">π</span>\
             </span></span></span></article></body></html>",
        );
        let text = main_text(&html);
        assert_eq!(
            text.matches("π").count(),
            0,
            "the visual copy is skipped: {text}"
        );
        assert!(text.contains("e^{i\\pi}"), "{text}");
    }

    #[test]
    fn mathml_keeps_the_annotation_instead_of_the_markup_text() {
        let html = Html::parse_document(
            "<html><body><article><math><semantics><mrow><mi>a</mi><mo>+</mo><mi>b</mi></mrow>\
             <annotation encoding=\"application/x-tex\">a+b</annotation></semantics></math></article></body></html>",
        );
        assert_eq!(main_text(&html), "a+b");
    }

    #[test]
    fn mathjax_assistive_copy_is_not_repeated() {
        let html = Html::parse_document(
            "<html><body><article><mjx-container class=\"MathJax\"><mjx-math><mi>x</mi></mjx-math>\
             <mjx-assistive-mml><math><mi>x</mi></math></mjx-assistive-mml></mjx-container></article></body></html>",
        );
        assert_eq!(main_text(&html), "x");
    }

    #[test]
    fn preformatted_text_skips_scripts_and_honours_br() {
        let html = Html::parse_document(
            "<html><body><pre>a<br>  b<script>x=1</script>\r\nc</pre></body></html>",
        );
        assert_eq!(main_text(&html), "a\n  b\nc");
    }
}
