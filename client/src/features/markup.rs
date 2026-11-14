use dioxus::prelude::*;

use crate::protocol::Id;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Text(String),
    Bold(Vec<Node>),
    Italic(Vec<Node>),
    Underline(Vec<Node>),
    Strike(Vec<Node>),
    /// Hidden until the reader asks for it.
    Spoiler(Vec<Node>),
    Code(String),
    /// `None` when the target is not a web address: the text shows, nothing opens.
    Link(Option<String>, Vec<Node>),
    Paragraph(Vec<Node>),
    Heading(u8, Vec<Node>),
    Quote(Vec<Node>),
    List(Option<u64>, Vec<Node>),
    Item(Vec<Node>),
    CodeBlock(String),
    Rule,
}

const MARKS: [&str; 6] = ["**", "__", "||", "~~", "*", "_"];
const MARK_CHARS: [char; 4] = ['*', '_', '|', '~'];
const SPOILER: &str = "||";
const MAX_DEPTH: usize = 6;

/// `**bold**`, `__underline__`, `~~strike~~`, `||spoiler||`, `*italic*` and `_italic_`,
/// nested; an unmatched mark is text.
pub fn inline(s: &str) -> Vec<Node> {
    // Where each opening mark closes. One only reads those after it, so filling
    // right to left needs no recursion and keeps a hostile message quadratic at worst.
    let mut span: Vec<Option<(usize, usize)>> = vec![None; s.len() + 1];
    for i in (0..s.len()).rev() {
        if s.is_char_boundary(i) {
            span[i] = open_at(s, i, &span);
        }
    }
    build(s, 0, s.len(), &span, 0)
}

fn open_at(s: &str, i: usize, span: &[Option<(usize, usize)>]) -> Option<(usize, usize)> {
    MARKS.iter().enumerate().find_map(|(k, mark)| {
        if !s[i..].starts_with(mark) {
            return None;
        }
        // snake_case_name is a name, not emphasis.
        if mark.starts_with('_')
            && s[..i]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        {
            return None;
        }
        // A run of marks opens as a whole, so `** b` is no italic around `* b`.
        let mark_char = mark.chars().next().unwrap_or_default();
        let past_run = s[i..].trim_start_matches(mark_char);
        if past_run.chars().next().is_none_or(char::is_whitespace) {
            return None;
        }
        closing(s, i + mark.len(), mark, span).map(|c| (k, c))
    })
}

fn closing(s: &str, from: usize, mark: &str, span: &[Option<(usize, usize)>]) -> Option<usize> {
    let (mut j, mut content) = (from, false);
    while j < s.len() {
        if content && s[j..].starts_with(mark) && closes(s, j, mark) {
            return Some(j);
        }
        if let Some((k, c)) = span[j] {
            j = c + MARKS[k].len();
            content = true;
            continue;
        }
        let ch = s[j..].chars().next().unwrap_or_default();
        content |= !MARK_CHARS.contains(&ch);
        j += ch.len_utf8().max(1);
    }
    None
}

fn closes(s: &str, j: usize, mark: &str) -> bool {
    let mark_char = mark.chars().next().unwrap_or_default();
    let before = s[..j].trim_end_matches(mark_char).chars().next_back();
    let after = s[j + mark.len()..].chars().next();
    before.is_some_and(|c| !c.is_whitespace())
        && !(mark.starts_with('_') && after.is_some_and(char::is_alphanumeric))
}

fn build(
    s: &str,
    from: usize,
    to: usize,
    span: &[Option<(usize, usize)>],
    depth: usize,
) -> Vec<Node> {
    if depth == MAX_DEPTH {
        return vec![Node::Text(s[from..to].to_string())];
    }
    let mut out = Vec::new();
    let mut text = String::new();
    let mut i = from;
    while i < to {
        if let Some((k, c)) = span[i]
            && c + MARKS[k].len() <= to
        {
            if !text.is_empty() {
                out.push(Node::Text(std::mem::take(&mut text)));
            }
            let inner = build(s, i + MARKS[k].len(), c, span, depth + 1);
            out.push(match MARKS[k] {
                "**" => Node::Bold(inner),
                "__" => Node::Underline(inner),
                "~~" => Node::Strike(inner),
                SPOILER => Node::Spoiler(inner),
                _ => Node::Italic(inner),
            });
            i = c + MARKS[k].len();
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or_default();
        text.push(ch);
        i += ch.len_utf8().max(1);
    }
    if !text.is_empty() {
        out.push(Node::Text(text));
    }
    out
}

enum Frame {
    Root,
    Plain,
    Paragraph,
    Heading(u8),
    Quote,
    CodeBlock,
    List(Option<u64>),
    Item,
    Italic,
    Bold,
    Underline,
    Strike,
    Link(Option<String>),
}

impl Frame {
    fn close(self, children: Vec<Node>) -> Vec<Node> {
        vec![match self {
            Frame::Root | Frame::Plain => return children,
            Frame::Paragraph => Node::Paragraph(children),
            Frame::Heading(level) => Node::Heading(level, children),
            Frame::Quote => Node::Quote(children),
            Frame::CodeBlock => Node::CodeBlock(
                children
                    .iter()
                    .filter_map(|n| match n {
                        Node::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
                    .trim_end_matches('\n')
                    .to_string(),
            ),
            Frame::List(start) => Node::List(start, children),
            Frame::Item => Node::Item(children),
            Frame::Italic => Node::Italic(children),
            Frame::Bold => Node::Bold(children),
            Frame::Underline => Node::Underline(children),
            Frame::Strike => Node::Strike(children),
            Frame::Link(href) if children.is_empty() => {
                let shown = href.clone().unwrap_or_default();
                Node::Link(href, vec![Node::Text(shown)])
            }
            Frame::Link(href) => Node::Link(href, children),
        }]
    }
}

fn push_text(into: &mut Vec<Node>, text: &str) {
    match into.last_mut() {
        Some(Node::Text(last)) => last.push_str(text),
        _ => into.push(Node::Text(text.to_string())),
    }
}

fn web(url: &str) -> Option<String> {
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_string())
}

/// CommonMark with strikethrough. HTML stays text and images become links: nothing
/// a message says is run, and nothing loads from a host the reader did not pick.
pub fn markdown(src: &str) -> Vec<Node> {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let mut stack: Vec<(Frame, Vec<Node>)> = vec![(Frame::Root, Vec::new())];
    for (event, range) in Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH).into_offset_iter() {
        let Some((_, children)) = stack.last_mut() else {
            break;
        };
        match event {
            Event::Start(tag) => {
                let frame = match tag {
                    Tag::Paragraph => Frame::Paragraph,
                    Tag::Heading { level, .. } => Frame::Heading(level as u8),
                    Tag::BlockQuote(_) => Frame::Quote,
                    Tag::CodeBlock(_) => Frame::CodeBlock,
                    Tag::List(start) => Frame::List(start),
                    Tag::Item => Frame::Item,
                    Tag::Emphasis => Frame::Italic,
                    // Discord's convention, which the rest of the app follows.
                    Tag::Strong if src[range.start..].starts_with("__") => Frame::Underline,
                    Tag::Strong => Frame::Bold,
                    Tag::Strikethrough => Frame::Strike,
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        Frame::Link(web(&dest_url))
                    }
                    _ => Frame::Plain,
                };
                stack.push((frame, Vec::new()));
            }
            Event::End(_) => {
                if stack.len() > 1
                    && let Some((frame, children)) = stack.pop()
                    && let Some((_, parent)) = stack.last_mut()
                {
                    parent.extend(frame.close(children));
                }
            }
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => push_text(children, &t),
            Event::Code(t) => children.push(Node::Code(t.to_string())),
            Event::SoftBreak | Event::HardBreak => push_text(children, "\n"),
            Event::Rule => children.push(Node::Rule),
            _ => {}
        }
    }
    spoilers(stack.into_iter().next().map(|(_, n)| n).unwrap_or_default())
}

/// CommonMark has no spoiler, so `||` is paired among siblings after the parse, by
/// `inline`'s rule: it opens before a word and closes after one.
fn spoilers(nodes: Vec<Node>) -> Vec<Node> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for node in nodes {
        let text = match node {
            Node::Text(text) => text,
            Node::Bold(c) => Node::Bold(spoilers(c)).pushed(&mut out),
            Node::Italic(c) => Node::Italic(spoilers(c)).pushed(&mut out),
            Node::Underline(c) => Node::Underline(spoilers(c)).pushed(&mut out),
            Node::Strike(c) => Node::Strike(spoilers(c)).pushed(&mut out),
            Node::Link(href, c) => Node::Link(href, spoilers(c)).pushed(&mut out),
            Node::Paragraph(c) => Node::Paragraph(spoilers(c)).pushed(&mut out),
            Node::Heading(level, c) => Node::Heading(level, spoilers(c)).pushed(&mut out),
            Node::Quote(c) => Node::Quote(spoilers(c)).pushed(&mut out),
            Node::List(start, c) => Node::List(start, spoilers(c)).pushed(&mut out),
            Node::Item(c) => Node::Item(spoilers(c)).pushed(&mut out),
            other => other.pushed(&mut out),
        };
        let mut rest = text.as_str();
        let mut plain = String::new();
        while let Some(at) = rest.find(SPOILER) {
            plain.push_str(&rest[..at]);
            let after = &rest[at + SPOILER.len()..];
            let word_before = !plain.chars().next_back().is_some_and(char::is_whitespace);
            let word_after = !after.chars().next().is_some_and(char::is_whitespace);
            match open {
                Some(start) if word_before && (start < out.len() || !plain.is_empty()) => {
                    Node::Text(std::mem::take(&mut plain)).pushed_if_text(&mut out);
                    let hidden = out.split_off(start);
                    out.push(Node::Spoiler(hidden));
                    open = None;
                }
                None if word_after => {
                    Node::Text(std::mem::take(&mut plain)).pushed_if_text(&mut out);
                    open = Some(out.len());
                }
                _ => plain.push_str(SPOILER),
            }
            rest = after;
        }
        plain.push_str(rest);
        Node::Text(plain).pushed_if_text(&mut out);
    }
    if let Some(start) = open {
        out.insert(start, Node::Text(SPOILER.to_string()));
    }
    out
}

impl Node {
    /// Pushes and yields no text, so a `match` arm can finish a node that is not one.
    fn pushed(self, into: &mut Vec<Node>) -> String {
        into.push(self);
        String::new()
    }

    fn pushed_if_text(self, into: &mut Vec<Node>) {
        if !matches!(&self, Node::Text(text) if text.is_empty()) {
            into.push(self);
        }
    }
}

/// A message as one line of plain words for an alert or a reply excerpt: marks dropped,
/// and what a spoiler hides replaced, since neither place can ask before showing.
pub fn plain(s: &str) -> String {
    fn flatten(nodes: &[Node], out: &mut String) {
        for node in nodes {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Spoiler(_) => out.push_str("▒▒▒▒▒"),
                Node::Bold(c) | Node::Italic(c) | Node::Underline(c) | Node::Strike(c) => {
                    flatten(c, out)
                }
                _ => {}
            }
        }
    }
    let mut out = String::with_capacity(s.len());
    flatten(&inline(s), &mut out);
    out
}

#[component]
pub fn Markup(
    content: String,
    markdown: bool,
    guild_id: Option<Id>,
    extra: Vec<(String, String)>,
    mentions: Vec<String>,
) -> Element {
    let nodes = use_memo(use_reactive(
        (&content, &markdown),
        |(content, markdown)| {
            if markdown {
                self::markdown(&content)
            } else {
                inline(&content)
            }
        },
    ));
    if markdown {
        rsx! {
            div { class: "dxf-md", Nodes { nodes: nodes(), guild_id, extra, mentions } }
        }
    } else {
        rsx! { Nodes { nodes: nodes(), guild_id, extra, mentions } }
    }
}

#[component]
fn Nodes(
    nodes: Vec<Node>,
    guild_id: Option<Id>,
    extra: Vec<(String, String)>,
    mentions: Vec<String>,
) -> Element {
    rsx! {
        for node in nodes {
            match node {
                Node::Text(text) => rsx! { PlainRun { text, guild_id, extra: extra.clone(), mentions: mentions.clone() } },
                Node::Bold(c) => rsx! { strong { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Italic(c) => rsx! { em { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Underline(c) => rsx! { u { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Strike(c) => rsx! { s { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Spoiler(c) => rsx! { Spoiler { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Code(text) => rsx! { code { "{text}" } },
                Node::Link(Some(href), c) => rsx! {
                    span {
                        class: "text-[var(--accent)] underline cursor-pointer hover:text-[var(--accent-strong)]",
                        title: "{href}",
                        onclick: move |_| crate::app::open_external(&href),
                        Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() }
                    }
                },
                Node::Link(None, c) => rsx! { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } },
                Node::Paragraph(c) => rsx! { p { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Heading(1, c) => rsx! { h1 { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Heading(2, c) => rsx! { h2 { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Heading(_, c) => rsx! { h3 { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Quote(c) => rsx! { blockquote { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::List(Some(start), c) => rsx! { ol { start: "{start}", Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::List(None, c) => rsx! { ul { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::Item(c) => rsx! { li { Nodes { nodes: c, guild_id, extra: extra.clone(), mentions: mentions.clone() } } },
                Node::CodeBlock(text) => rsx! { pre { code { "{text}" } } },
                Node::Rule => rsx! { hr {} },
            }
        }
    }
}

/// The inner span is what is hidden, so a link or a picture inside cannot be reached early.
#[component]
fn Spoiler(children: Element) -> Element {
    let mut shown = use_signal(|| false);
    rsx! {
        span {
            class: if shown() { "dxf-spoiler dxf-spoiler-shown" } else { "dxf-spoiler" },
            role: (!shown()).then_some("button"),
            tabindex: (!shown()).then_some("0"),
            aria_label: (!shown()).then(|| crate::i18n::text("Spoiler, press to show", &[])),
            onclick: move |event| {
                if !shown() {
                    event.stop_propagation();
                    shown.set(true);
                }
            },
            onkeydown: move |event| {
                if !shown() && matches!(event.key(), Key::Enter) {
                    event.prevent_default();
                    shown.set(true);
                }
            },
            span { aria_hidden: (!shown()).then_some("true"), {children} }
        }
    }
}

fn rich_word(word: &str) -> bool {
    word.starts_with("http://")
        || word.starts_with("https://")
        || (word.starts_with('@') && word.len() > 1)
        || crate::emoji::needs_rendering(word)
}

/// Splits on spaces, except inside a mention of one of `names`, whose
/// spaces would otherwise cut `@John Doe` down to `@John`.
fn words<'a>(line: &'a str, names: &[String]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = line;
    loop {
        let len = rest
            .strip_prefix('@')
            .and_then(|after| {
                names
                    .iter()
                    .filter_map(|n| crate::protocol::mention_len(after, n))
                    .max()
            })
            .map_or(0, |len| len + 1);
        match rest[len..].find(' ') {
            Some(i) => {
                out.push(&rest[..len + i]);
                rest = &rest[len + i + 1..];
            }
            None => {
                out.push(rest);
                return out;
            }
        }
    }
}

#[component]
fn PlainRun(
    text: String,
    guild_id: Option<Id>,
    extra: Vec<(String, String)>,
    mentions: Vec<String>,
) -> Element {
    let lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len().saturating_sub(1);
    rsx! {
        for (li, line) in lines.iter().enumerate() {
            {
                let rich = line.split(' ').any(rich_word);
                let words = if rich { words(line, &mentions) } else { Vec::new() };
                let lastw = words.len().saturating_sub(1);
                rsx! {
                    if !rich { "{line}" }
                    for (wi, word) in words.iter().enumerate() {
                        {
                            let w = word.to_string();
                            let trailing = if wi < lastw { " " } else { "" };
                            if web(&w).is_some() {
                                let href = w.clone();
                                rsx! {
                                    span {
                                        span {
                                            class: "text-[var(--accent)] underline cursor-pointer hover:text-[var(--accent-strong)]",
                                            onclick: move |_| crate::app::open_external(&href),
                                            "{w}"
                                        }
                                        "{trailing}"
                                    }
                                }
                            } else if w.starts_with('@') && w.len() > 1 {
                                rsx! {
                                    span {
                                        span { class: "text-[var(--accent)] bg-[var(--accent-soft)] rounded px-0.5", "{w}" }
                                        "{trailing}"
                                    }
                                }
                            } else if crate::emoji::needs_rendering(&w) {
                                rsx! { span { super::chat::EmojiText { text: w.clone(), guild_id, extra: extra.clone() } "{trailing}" } }
                            } else {
                                rsx! { span { "{w}{trailing}" } }
                            }
                        }
                    }
                    if li < last {
                        br {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_mentions_and_emoji_keep_their_rich_rendering() {
        for word in [
            "http://example.com",
            "https://example.com",
            "@someone",
            ":fire:",
            "🔥",
        ] {
            assert!(rich_word(word), "{word}");
        }
        for word in ["ordinary", "chat", "text", "@", "someone@example.com", ""] {
            assert!(!rich_word(word), "{word}");
        }
    }

    #[test]
    fn a_mention_keeps_the_spaces_of_a_known_name() {
        let names = vec!["John Doe".to_string(), "John Doe Jr".to_string()];
        assert_eq!(
            words("hi @john doe, and @John Doe Jr! @John Smith", &names),
            ["hi", "@john doe,", "and", "@John Doe Jr!", "@John", "Smith"]
        );
        assert_eq!(words("@John Doer", &names), ["@John", "Doer"]);
        assert_eq!(words("a  b ", &names), ["a", "", "b", ""]);
    }

    #[test]
    fn ordinary_paragraphs_keep_a_bounded_dom_when_opening_a_chat() {
        let line = "ordinary chat text ".repeat(200);
        let started = std::time::Instant::now();
        let mut dom = VirtualDom::new_with_props(
            |text: String| rsx! { PlainRun { text, guild_id: None, extra: Vec::new(), mentions: Vec::new() } },
            format!("{line}\nsecond line"),
        );
        let edits = dom.rebuild_to_vec().edits;
        let templates = edits
            .iter()
            .filter(|edit| matches!(edit, dioxus::core::Mutation::LoadTemplate { .. }))
            .count();
        eprintln!(
            "ordinary paragraph: {templates} template instances, {:?}",
            started.elapsed()
        );
        let text: String = edits
            .iter()
            .filter_map(|edit| match edit {
                dioxus::core::Mutation::CreateTextNode { value, .. } => Some(value.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains(&line));
        assert!(text.contains("second line"));
        assert!(
            templates < 8,
            "plain text must not create one element per word: {templates}"
        );
    }

    fn t(s: &str) -> Node {
        Node::Text(s.into())
    }

    #[test]
    fn the_three_marks_and_their_nesting() {
        assert_eq!(
            inline("a **b** *c* __d__ _e_"),
            vec![
                t("a "),
                Node::Bold(vec![t("b")]),
                t(" "),
                Node::Italic(vec![t("c")]),
                t(" "),
                Node::Underline(vec![t("d")]),
                t(" "),
                Node::Italic(vec![t("e")]),
            ]
        );
        assert_eq!(
            inline("***x***"),
            vec![Node::Bold(vec![Node::Italic(vec![t("x")])])]
        );
        assert_eq!(
            inline("*a **b** c*"),
            vec![Node::Italic(vec![
                t("a "),
                Node::Bold(vec![t("b")]),
                t(" c")
            ])]
        );
        assert_eq!(
            inline("__**x**__"),
            vec![Node::Underline(vec![Node::Bold(vec![t("x")])])]
        );
    }

    #[test]
    fn strike_and_spoiler_are_marks_like_the_rest() {
        assert_eq!(
            inline("~~gone~~ and ||the **end**||"),
            vec![
                Node::Strike(vec![t("gone")]),
                t(" and "),
                Node::Spoiler(vec![t("the "), Node::Bold(vec![t("end")])]),
            ]
        );
        assert_eq!(
            inline("**a ||b|| c**"),
            vec![Node::Bold(vec![
                t("a "),
                Node::Spoiler(vec![t("b")]),
                t(" c")
            ])]
        );
        for typed in ["a || b || c", "||", "||||", "x ||unclosed", "a ~ b ~~ c"] {
            assert_eq!(inline(typed), vec![t(typed)], "{typed}");
        }
    }

    #[test]
    fn markdown_hides_a_spoiler_among_its_siblings() {
        assert_eq!(
            markdown("see ||the **end**|| now"),
            vec![Node::Paragraph(vec![
                t("see "),
                Node::Spoiler(vec![t("the "), Node::Bold(vec![t("end")])]),
                t(" now"),
            ])]
        );
        assert_eq!(
            markdown("**a ||b|| c**"),
            vec![Node::Paragraph(vec![Node::Bold(vec![
                t("a "),
                Node::Spoiler(vec![t("b")]),
                t(" c"),
            ])])]
        );
        for typed in ["a || b || c", "x ||unclosed", "||||"] {
            let flat: String = match &markdown(typed)[..] {
                [Node::Paragraph(children)] => children
                    .iter()
                    .map(|n| match n {
                        Node::Text(text) => text.clone(),
                        other => panic!("{typed}: {other:?}"),
                    })
                    .collect(),
                other => panic!("{typed}: {other:?}"),
            };
            assert_eq!(flat, typed);
        }
        assert_eq!(
            markdown("`||code||`"),
            vec![Node::Paragraph(vec![Node::Code("||code||".into())])]
        );
    }

    #[test]
    fn a_plain_line_drops_marks_and_never_says_what_a_spoiler_hides() {
        assert_eq!(
            plain("**well** ||he *dies*|| at the end"),
            "well ▒▒▒▒▒ at the end"
        );
        assert_eq!(plain("a || b || c"), "a || b || c");
        assert_eq!(plain("snake_case stays"), "snake_case stays");
    }

    #[test]
    fn what_is_not_emphasis_stays_as_typed() {
        for plain in [
            "2 * 3 * 4",
            "snake_case_name",
            "****",
            "**unclosed",
            "a ** b **",
            "https://example.com/a_b_c",
        ] {
            assert_eq!(inline(plain), vec![t(plain)], "{plain}");
        }
    }

    #[test]
    fn marks_span_lines_and_survive_wide_characters() {
        assert_eq!(
            inline("**one\ntwo** 🙂 _é_"),
            vec![
                Node::Bold(vec![t("one\ntwo")]),
                t(" 🙂 "),
                Node::Italic(vec![t("é")]),
            ]
        );
    }

    #[test]
    fn a_hostile_message_is_parsed_quickly_and_shallowly() {
        let wall = "*_".repeat(1000);
        let started = std::time::Instant::now();
        inline(&wall);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));

        let deep = format!("{}x{}", "*_".repeat(20), "_*".repeat(20));
        fn depth(nodes: &[Node]) -> usize {
            nodes
                .iter()
                .map(|n| match n {
                    Node::Bold(c) | Node::Italic(c) | Node::Underline(c) => 1 + depth(c),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }
        assert!(depth(&inline(&deep)) <= MAX_DEPTH);
    }

    #[test]
    fn markdown_builds_blocks_and_keeps_underline() {
        let nodes = markdown("# Title\n\n- one\n- **two**\n\n> __quoted__\n\n```\nlet x = 1;\n```");
        assert_eq!(
            nodes,
            vec![
                Node::Heading(1, vec![t("Title")]),
                Node::List(
                    None,
                    vec![
                        Node::Item(vec![t("one")]),
                        Node::Item(vec![Node::Bold(vec![t("two")])]),
                    ]
                ),
                Node::Quote(vec![Node::Paragraph(vec![Node::Underline(vec![t(
                    "quoted"
                )])])]),
                Node::CodeBlock("let x = 1;".into()),
            ]
        );
    }

    #[test]
    fn markdown_never_runs_html_or_loads_an_image_or_opens_a_script() {
        let nodes =
            markdown("<b>hi</b> ![pic](https://example.com/a.png) [x](javascript:alert(1))");
        let Node::Paragraph(inner) = &nodes[0] else {
            panic!("{nodes:?}")
        };
        assert_eq!(inner[0], t("<b>hi</b> "));
        assert_eq!(
            inner[1],
            Node::Link(Some("https://example.com/a.png".into()), vec![t("pic")])
        );
        assert_eq!(inner[3], Node::Link(None, vec![t("x")]));
    }

    #[test]
    fn a_single_newline_in_markdown_is_still_a_line_break() {
        assert_eq!(
            markdown("one\ntwo"),
            vec![Node::Paragraph(vec![t("one\ntwo")])]
        );
    }
}
