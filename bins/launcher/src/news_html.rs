// SPDX-License-Identifier: GPL-3.0-or-later
//! A small reader for the `bodyHtml` of a site news post. The site sanitizes the body to the tags
//! `p strong em a ul ol li code br`, with links to http, https and mailto only. This reader knows
//! exactly those tags and turns them into blocks of styled text; any other tag shows as its own
//! text, and a link to another scheme shows as plain text.

use reqwest::Url;

/// The tags this reader knows.
const PARAGRAPH: &str = "p";
const STRONG: &str = "strong";
const EMPHASIS: &str = "em";
const LINK: &str = "a";
const BULLET_LIST: &str = "ul";
const NUMBERED_LIST: &str = "ol";
const LIST_ITEM: &str = "li";
const CODE: &str = "code";
const LINE_BREAK: &str = "br";
/// The tags that change the look of text inside a block.
const INLINE_TAGS: [&str; 4] = [STRONG, EMPHASIS, LINK, CODE];
/// The link target attribute of `a`.
const HREF: &str = "href";
/// The link schemes that open.
const LINK_SCHEMES: [&str; 3] = ["http", "https", "mailto"];
/// The marker of a bullet list item.
pub const BULLET: &str = "\u{2022}";
/// The number of the first item of a numbered list.
const FIRST_NUMBER: usize = 1;

/// How a run of text looks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpanStyle {
    /// Inside `strong`.
    pub strong: bool,
    /// Inside `em`.
    pub emphasis: bool,
    /// Inside `code`.
    pub code: bool,
}

/// A run of text with one style and one link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The text; `\n` for a `br`.
    pub text: String,
    /// How it looks.
    pub style: SpanStyle,
    /// The link it opens, when it is inside an `a` with a safe link.
    pub link: Option<String>,
}

/// What a block is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// A paragraph.
    Paragraph,
    /// A list item: its nesting depth (0 for a top list) and its marker (a bullet or `1.`).
    Item {
        /// Number of lists around the list of this item.
        depth: usize,
        /// The bullet or the number.
        marker: String,
    },
}

/// A paragraph or a list item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Paragraph or list item.
    pub kind: BlockKind,
    /// Its text runs.
    pub spans: Vec<Span>,
}

/// The link of `href` when it is an http, https or mailto address.
pub fn safe_link(href: &str) -> Option<String> {
    let url = Url::parse(href.trim()).ok()?;
    LINK_SCHEMES
        .contains(&url.scheme())
        .then(|| url.to_string())
}

/// One piece of the body source.
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    /// Text between tags, with its entities.
    Text(&'a str),
    /// A start tag: its lower-case name, its attribute text and its source.
    Open {
        name: String,
        attributes: &'a str,
        source: &'a str,
    },
    /// An end tag: its lower-case name and its source.
    Close { name: String, source: &'a str },
}

/// The tag name at the start of `text` and the rest, or `None` when `text` does not start with a
/// letter.
fn tag_name(text: &str) -> Option<(String, &str)> {
    let end = text
        .find(|character: char| !character.is_ascii_alphanumeric())
        .unwrap_or(text.len());
    let name = &text[..end];
    name.starts_with(|character: char| character.is_ascii_alphabetic())
        .then(|| (name.to_ascii_lowercase(), &text[end..]))
}

/// The tokens of `source`. A `<` with no tag name after it, or with no `>` after it, is text.
fn tokens(source: &str) -> Vec<Token<'_>> {
    let mut found = Vec::new();
    let mut rest = source;
    while !rest.is_empty() {
        let Some(start) = rest.find('<') else {
            found.push(Token::Text(rest));
            break;
        };
        let Some(length) = rest[start..].find('>') else {
            found.push(Token::Text(rest));
            break;
        };
        let end = start + length + 1;
        let tag = &rest[start..end];
        let inner = &tag[1..tag.len() - 1];
        let token = match inner.strip_prefix('/') {
            Some(closing) => tag_name(closing).map(|(name, _)| Token::Close { name, source: tag }),
            None => tag_name(inner).map(|(name, attributes)| Token::Open {
                name,
                attributes: attributes.trim_end_matches('/'),
                source: tag,
            }),
        };
        match token {
            Some(token) => {
                if start > 0 {
                    found.push(Token::Text(&rest[..start]));
                }
                found.push(token);
                rest = &rest[end..];
            }
            // Not a tag: the `<` is text, and the search goes on after it.
            None => {
                found.push(Token::Text(&rest[..=start]));
                rest = &rest[start + 1..];
            }
        }
    }
    found
}

/// The value of attribute `name` in the attribute text of a start tag, without its quotes.
fn attribute<'a>(attributes: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = attributes;
    loop {
        rest = rest.trim_start();
        let key_end = rest.find(|character: char| character == '=' || character.is_whitespace())?;
        let key = &rest[..key_end];
        rest = rest[key_end..].trim_start();
        let Some(after_equals) = rest.strip_prefix('=') else {
            continue;
        };
        let value_text = after_equals.trim_start();
        let (value, after) = match value_text.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let body = &value_text[1..];
                let close = body.find(quote)?;
                (&body[..close], &body[close + 1..])
            }
            _ => {
                let end = value_text
                    .find(char::is_whitespace)
                    .unwrap_or(value_text.len());
                (&value_text[..end], &value_text[end..])
            }
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
        rest = after;
    }
}

/// The character of one entity name or number (the text between `&` and `;`).
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Longest entity name this reader looks for after a `&`.
const MAX_ENTITY_LENGTH: usize = 10;

/// `text` with its character entities decoded; an unknown entity stays as it is.
pub fn decode_entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let found = after
            .find(';')
            .filter(|end| *end <= MAX_ENTITY_LENGTH)
            .and_then(|end| entity(&after[..end]).map(|character| (character, end)));
        match found {
            Some((character, end)) => {
                decoded.push(character);
                rest = &after[end + 1..];
            }
            None => {
                decoded.push('&');
                rest = after;
            }
        }
    }
    decoded.push_str(rest);
    decoded
}

/// One open list.
struct OpenList {
    ordered: bool,
    next: usize,
}

/// Builds the blocks from the tokens.
#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    current: Option<Block>,
    strong: usize,
    emphasis: usize,
    code: usize,
    links: Vec<Option<String>>,
    lists: Vec<OpenList>,
    /// White space was seen after the last text of the current block.
    space: bool,
}

impl Builder {
    fn style(&self) -> SpanStyle {
        SpanStyle {
            strong: self.strong > 0,
            emphasis: self.emphasis > 0,
            code: self.code > 0,
        }
    }

    fn link(&self) -> Option<String> {
        self.links.last().cloned().flatten()
    }

    fn finish_block(&mut self) {
        if let Some(mut block) = self.current.take() {
            if let Some(last) = block.spans.last_mut() {
                let kept = last.text.trim_end_matches([' ', '\n']).len();
                last.text.truncate(kept);
            }
            block.spans.retain(|span| !span.text.is_empty());
            if !block.spans.is_empty() {
                self.blocks.push(block);
            }
        }
        self.space = false;
    }

    fn start_block(&mut self, kind: BlockKind) {
        self.finish_block();
        self.current = Some(Block {
            kind,
            spans: Vec::new(),
        });
    }

    /// Adds one character to the current block (a paragraph is started for text outside a
    /// block).
    fn push_char(&mut self, character: char) {
        let style = self.style();
        let link = self.link();
        let block = self.current.get_or_insert_with(|| Block {
            kind: BlockKind::Paragraph,
            spans: Vec::new(),
        });
        match block.spans.last_mut() {
            Some(span) if span.style == style && span.link == link => span.text.push(character),
            _ => block.spans.push(Span {
                text: character.to_string(),
                style,
                link,
            }),
        }
    }

    fn block_has_text(&self) -> bool {
        self.current.as_ref().is_some_and(|block| {
            block
                .spans
                .last()
                .is_some_and(|span| !span.text.ends_with('\n'))
        })
    }

    /// Adds the white space seen so far as one space in the style before a style change, so a
    /// space next to a tag stays outside the styled text (as a browser shows it).
    fn flush_space(&mut self) {
        if self.space {
            self.push_char(' ');
            self.space = false;
        }
    }

    /// Adds text; runs of white space become one space, and white space at a block start or
    /// after a line break is dropped.
    fn push_text(&mut self, text: &str) {
        for character in text.chars() {
            if character.is_whitespace() {
                self.space = self.block_has_text();
            } else {
                self.flush_space();
                self.push_char(character);
            }
        }
    }

    fn line_break(&mut self) {
        if self.current.is_some() {
            self.push_char('\n');
        }
        self.space = false;
    }

    fn open(&mut self, name: &str, attributes: &str, source: &str) {
        if INLINE_TAGS.contains(&name) {
            self.flush_space();
        }
        match name {
            PARAGRAPH => {
                let empty_item = matches!(
                    &self.current,
                    Some(Block { kind: BlockKind::Item { .. }, spans }) if spans.is_empty()
                );
                if !empty_item {
                    self.start_block(BlockKind::Paragraph);
                }
            }
            STRONG => self.strong += 1,
            EMPHASIS => self.emphasis += 1,
            CODE => self.code += 1,
            LINK => self.links.push(
                attribute(attributes, HREF).and_then(|href| safe_link(&decode_entities(href))),
            ),
            BULLET_LIST | NUMBERED_LIST => {
                self.finish_block();
                self.lists.push(OpenList {
                    ordered: name == NUMBERED_LIST,
                    next: FIRST_NUMBER,
                });
            }
            LIST_ITEM => {
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(list) if list.ordered => {
                        list.next += 1;
                        format!("{}.", list.next - 1)
                    }
                    _ => BULLET.to_owned(),
                };
                self.start_block(BlockKind::Item { depth, marker });
            }
            LINE_BREAK => self.line_break(),
            _ => self.push_text(source),
        }
    }

    fn close(&mut self, name: &str, source: &str) {
        if INLINE_TAGS.contains(&name) {
            self.flush_space();
        }
        match name {
            PARAGRAPH | LIST_ITEM => self.finish_block(),
            STRONG => self.strong = self.strong.saturating_sub(1),
            EMPHASIS => self.emphasis = self.emphasis.saturating_sub(1),
            CODE => self.code = self.code.saturating_sub(1),
            LINK => {
                self.links.pop();
            }
            BULLET_LIST | NUMBERED_LIST => {
                self.finish_block();
                self.lists.pop();
            }
            LINE_BREAK => self.line_break(),
            _ => self.push_text(source),
        }
    }
}

/// The blocks of a sanitized post body.
pub fn render(body_html: &str) -> Vec<Block> {
    let mut builder = Builder::default();
    for token in tokens(body_html) {
        match token {
            Token::Text(text) => builder.push_text(&decode_entities(text)),
            Token::Open {
                name,
                attributes,
                source,
            } => builder.open(&name, attributes, source),
            Token::Close { name, source } => builder.close(&name, source),
        }
    }
    builder.finish_block();
    builder.blocks
}

/// The blocks as one line of plain text, for a short preview: blocks joined by a space, list
/// items with their marker.
pub fn plain_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|block| {
            let text: String = block
                .spans
                .iter()
                .map(|span| span.text.replace('\n', " "))
                .collect();
            match &block.kind {
                BlockKind::Paragraph => text,
                BlockKind::Item { marker, .. } => format!("{marker} {text}"),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether any block has a link.
pub fn has_link(blocks: &[Block]) -> bool {
    blocks
        .iter()
        .flat_map(|block| &block.spans)
        .any(|span| span.link.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, style: SpanStyle, link: Option<&str>) -> Span {
        Span {
            text: text.to_owned(),
            style,
            link: link.map(str::to_owned),
        }
    }

    fn plain(text: &str) -> Span {
        span(text, SpanStyle::default(), None)
    }

    fn paragraph(spans: Vec<Span>) -> Block {
        Block {
            kind: BlockKind::Paragraph,
            spans,
        }
    }

    fn item(depth: usize, marker: &str, spans: Vec<Span>) -> Block {
        Block {
            kind: BlockKind::Item {
                depth,
                marker: marker.to_owned(),
            },
            spans,
        }
    }

    const STRONG_STYLE: SpanStyle = SpanStyle {
        strong: true,
        emphasis: false,
        code: false,
    };
    const EMPHASIS_STYLE: SpanStyle = SpanStyle {
        strong: false,
        emphasis: true,
        code: false,
    };
    const CODE_STYLE: SpanStyle = SpanStyle {
        strong: false,
        emphasis: false,
        code: true,
    };

    #[test]
    fn a_site_body_becomes_styled_paragraphs_and_items() {
        let body = "<p>Join us for an <em>open test</em> this <strong>weekend</strong>.</p>\n<ul>\n\
            <li>Fresh characters</li>\n<li>Report bugs on <a target=\"_blank\" \
            href=\"https://discord.gg/example\" rel=\"noopener nofollow\">Discord</a></li>\n</ul>\n\
            <p>Run <code>!level 65</code><br>then play.</p>\n";
        assert_eq!(
            render(body),
            [
                paragraph(vec![
                    plain("Join us for an "),
                    span("open test", EMPHASIS_STYLE, None),
                    plain(" this "),
                    span("weekend", STRONG_STYLE, None),
                    plain("."),
                ]),
                item(0, BULLET, vec![plain("Fresh characters")]),
                item(
                    0,
                    BULLET,
                    vec![
                        plain("Report bugs on "),
                        span(
                            "Discord",
                            SpanStyle::default(),
                            Some("https://discord.gg/example")
                        ),
                    ]
                ),
                paragraph(vec![
                    plain("Run "),
                    span("!level 65", CODE_STYLE, None),
                    plain("\nthen play."),
                ]),
            ]
        );
    }

    #[test]
    fn numbered_and_nested_lists_get_numbers_and_depth() {
        let body = "<ol><li><p>One</p></li><li>Two<ul><li>Inner</li></ul></li></ol>";
        assert_eq!(
            render(body),
            [
                item(0, "1.", vec![plain("One")]),
                item(0, "2.", vec![plain("Two")]),
                item(1, BULLET, vec![plain("Inner")]),
            ]
        );
    }

    #[test]
    fn other_tags_show_as_text_and_entities_are_decoded() {
        assert_eq!(
            render("<p>a <script>x</script> &lt;b&gt; &amp; &#39;c&#x27; &unknown; 1 < 2</p>"),
            [paragraph(vec![plain(
                "a <script>x</script> <b> & 'c' &unknown; 1 < 2"
            )])]
        );
        assert_eq!(
            render("<img src=x onerror=alert(1)>"),
            [paragraph(vec![plain("<img src=x onerror=alert(1)>")])]
        );
    }

    #[test]
    fn unsafe_links_are_plain_text() {
        for href in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "",
        ] {
            let body = format!("<p><a href=\"{href}\">x</a></p>");
            assert_eq!(render(&body), [paragraph(vec![plain("x")])], "{href}");
        }
        assert_eq!(
            safe_link("mailto:staff@example.org").as_deref(),
            Some("mailto:staff@example.org")
        );
        assert_eq!(
            safe_link(" http://example.org/a?b=1 ").as_deref(),
            Some("http://example.org/a?b=1")
        );
    }

    #[test]
    fn link_attributes_are_read_in_any_quote_style() {
        assert_eq!(
            attribute(" target=_blank href='https://a.example.org' rel", HREF),
            Some("https://a.example.org")
        );
        assert_eq!(
            attribute(" HREF=https://b.example.org", HREF),
            Some("https://b.example.org")
        );
        assert_eq!(attribute(" rel=\"x\"", HREF), None);
        assert_eq!(attribute(" href=\"open", HREF), None);
    }

    #[test]
    fn broken_markup_is_kept_as_text() {
        assert_eq!(render("a <b"), [paragraph(vec![plain("a <b")])]);
        assert_eq!(render("x </ y"), [paragraph(vec![plain("x </ y")])]);
        assert_eq!(render("<p></p><p>  </p>"), Vec::<Block>::new());
        assert_eq!(
            render("</strong>text</em>"),
            [paragraph(vec![plain("text")])]
        );
    }

    #[test]
    fn the_preview_is_one_line_of_plain_text() {
        let blocks = render(
            "<p>One<br>two</p><ul><li>Item</li></ul><p><a href=\"https://x.example.org\">Link</a></p>",
        );
        assert_eq!(plain_text(&blocks), format!("One two {BULLET} Item Link"));
        assert!(has_link(&blocks));
        assert!(!has_link(&render("<p>none</p>")));
    }
}
