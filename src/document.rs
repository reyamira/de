use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::parsing::{
    ParseState, Scope, ScopeStack, SyntaxDefinition, SyntaxReference, SyntaxSet,
};

use crate::Palette;

/// A document prepared for display. Loading and parsing happen once, not during drawing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    pub lines: Vec<Line<'static>>,
    pub source_lines: Option<Vec<Line<'static>>>,
    pub markdown_source: Option<Arc<str>>,
    pub supports_reading: bool,
    pub notice: Option<String>,
}

// Bound both input and the number of independently allocated display lines.
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_LINES: usize = 20_000;

/// Load a regular UTF-8 file for display without interpreting executable content.
pub fn load(path: &Path, palette: &Palette) -> Result<Document, String> {
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("Cannot read file metadata: {error}"))?;
    if !metadata.is_file() {
        return Err("Preview is available only for regular files.".into());
    }
    let mut file = File::open(path).map_err(|error| format!("Cannot open file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Cannot read file metadata: {error}"))?;
    if !metadata.is_file() {
        return Err("Preview is available only for regular files.".into());
    }
    if metadata.len() > MAX_BYTES {
        return Err(format!(
            "File is too large to preview (limit: {MAX_BYTES} bytes)."
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read file: {error}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!(
            "File is too large to preview (limit: {MAX_BYTES} bytes)."
        ));
    }
    if bytes.contains(&0) {
        return Err("Binary files cannot be previewed as text.".into());
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "File is not valid UTF-8 text and cannot be previewed.".to_string())?;
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let is_markdown =
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown");
    if text.is_empty() {
        return Ok(Document {
            lines: vec![Line::default()],
            source_lines: is_markdown.then(|| vec![Line::default()]),
            notice: Some("Empty file.".into()),
            markdown_source: is_markdown.then(|| Arc::from(text)),
            supports_reading: !is_markdown
                && !extension.eq_ignore_ascii_case("json")
                && syntax_for(&SYNTAX_ASSETS, path, text)
                    .is_none_or(|syntax| syntax.name == "Plain Text"),
        });
    }
    if extension.eq_ignore_ascii_case("json") && !is_jsonc_config(path) {
        return Ok(json_document(text, palette));
    }
    if is_markdown {
        Ok(markdown_document(text, palette))
    } else {
        Ok(source_document(path, &sanitize(text), palette))
    }
}

// Never allow C0/C1 controls, including terminal escape sequences, through to the backend.
// Preserve line breaks and expand tabs so terminal tab stops cannot affect the layout.
fn sanitize(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                clean.push('\n');
            }
            '\n' => clean.push('\n'),
            '\t' => clean.push_str("    "),
            c if c.is_control() => clean.push('\u{fffd}'),
            c => clean.push(c),
        }
    }
    clean
}

struct Lines {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    truncated: bool,
}

impl Lines {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            truncated: false,
        }
    }

    fn push(&mut self, text: &str, style: Style) {
        if self.truncated {
            return;
        }
        let mut parts = text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                self.current.push(Span::styled(part.to_owned(), style));
            }
            if parts.peek().is_some() {
                self.newline();
            }
            if self.truncated {
                break;
            }
        }
    }

    fn newline(&mut self) {
        if self.lines.len() + 1 >= MAX_LINES {
            self.truncated = true;
        } else {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
    }

    fn end_block(&mut self) {
        if !self.current.is_empty() {
            self.newline();
        }
    }

    fn finish(mut self, notice: Option<String>) -> Document {
        if !self.current.is_empty() || self.lines.is_empty() {
            self.lines.push(Line::from(self.current));
        }
        let notice = if self.truncated {
            Some(match notice {
                Some(message) => format!("{message} Preview is limited to {MAX_LINES} lines."),
                None => format!("Preview is limited to {MAX_LINES} lines."),
            })
        } else {
            notice
        };
        Document {
            lines: self.lines,
            source_lines: None,
            markdown_source: None,
            supports_reading: false,
            notice,
        }
    }
}

struct SyntaxAssets {
    syntaxes: SyntaxSet,
    scopes: [(Scope, SyntaxColor); 10],
}

#[derive(Clone, Copy)]
enum SyntaxColor {
    Muted,
    Title,
    Accent,
    Symlink,
}

static SYNTAX_ASSETS: LazyLock<SyntaxAssets> = LazyLock::new(|| SyntaxAssets {
    syntaxes: {
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
        // https://github.com/sharkdp/bat/blob/73dc3258bec83bd7c66334f05b13c2d8221859f9/assets/syntaxes/02_Extra/TypeScript.sublime-syntax
        // Converted from Microsoft/TypeScript-Sublime-Plugin; license in assets/syntaxes.
        builder.add(
            SyntaxDefinition::load_from_str(
                include_str!("../assets/syntaxes/TypeScript.sublime-syntax"),
                true,
                None,
            )
            .expect("bundled TypeScript grammar must be valid"),
        );
        builder.add(
            SyntaxDefinition::load_from_str(
                include_str!("../assets/syntaxes/TypsecriptReact.sublime-syntax"),
                true,
                None,
            )
            .expect("bundled TypeScriptReact grammar must be valid"),
        );
        for (name, source) in [
            (
                "TOML",
                include_str!("../assets/syntaxes/TOML/TOML.sublime-syntax"),
            ),
            (
                "JSONC",
                include_str!("../assets/syntaxes/JSONC/JSONC.sublime-syntax"),
            ),
            (
                "JSON5",
                include_str!("../assets/syntaxes/JSON5/JSON5.sublime-syntax"),
            ),
            (
                "Dockerfile",
                include_str!("../assets/syntaxes/Dockerfile/Dockerfile.sublime-syntax"),
            ),
            (
                "GraphQL",
                include_str!("../assets/syntaxes/GraphQL/GraphQL.sublime-syntax"),
            ),
            (
                "JavaScript Regular Expressions",
                include_str!("../assets/syntaxes/web/JavaScript-RegExp.sublime-syntax"),
            ),
            (
                "JavaScript (Babel)",
                include_str!("../assets/syntaxes/web/JavaScript-Babel.sublime-syntax"),
            ),
            (
                "Vue Component",
                include_str!("../assets/syntaxes/web/Vue.sublime-syntax"),
            ),
            (
                "Svelte",
                include_str!("../assets/syntaxes/web/Svelte.sublime-syntax"),
            ),
            (
                "Prisma",
                include_str!("../assets/syntaxes/Prisma/Prisma.sublime-syntax"),
            ),
            (
                "Terraform",
                include_str!("../assets/syntaxes/Terraform/Terraform.sublime-syntax"),
            ),
        ] {
            builder.add(
                SyntaxDefinition::load_from_str(source, true, Some(name)).unwrap_or_else(|error| {
                    panic!("bundled {name} grammar must be valid: {error}")
                }),
            );
        }
        builder.build()
    },
    scopes: [
        (Scope::new("comment").unwrap(), SyntaxColor::Muted),
        (Scope::new("keyword").unwrap(), SyntaxColor::Title),
        (Scope::new("storage").unwrap(), SyntaxColor::Title),
        (Scope::new("entity").unwrap(), SyntaxColor::Title),
        (Scope::new("string").unwrap(), SyntaxColor::Accent),
        (Scope::new("markup.heading").unwrap(), SyntaxColor::Title),
        (Scope::new("markup.bold").unwrap(), SyntaxColor::Title),
        (Scope::new("markup.italic").unwrap(), SyntaxColor::Accent),
        (Scope::new("constant").unwrap(), SyntaxColor::Symlink),
        (Scope::new("support").unwrap(), SyntaxColor::Symlink),
    ],
});
/// List the names and file associations of the loaded syntax definitions.
pub fn syntax_definitions() -> impl Iterator<Item = (&'static str, &'static [String])> {
    SYNTAX_ASSETS
        .syntaxes
        .syntaxes()
        .iter()
        .map(|syntax| (syntax.name.as_str(), syntax.file_extensions.as_slice()))
}

fn is_jsonc_config(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    ["tsconfig", "jsconfig"].iter().any(|prefix| {
        name == format!("{prefix}.json")
            || (name.starts_with(&format!("{prefix}.")) && name.ends_with(".json"))
    })
}

fn syntax_for<'a>(
    assets: &'a SyntaxAssets,
    path: &Path,
    text: &str,
) -> Option<&'a SyntaxReference> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    if is_jsonc_config(path) {
        return assets.syntaxes.find_syntax_by_extension("jsonc");
    }
    if name.eq_ignore_ascii_case("Dockerfile")
        || name
            .get(..11)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Dockerfile."))
    {
        return assets.syntaxes.find_syntax_by_extension("Dockerfile");
    }
    if name.eq_ignore_ascii_case("Cargo.lock") {
        return assets.syntaxes.find_syntax_by_extension("toml");
    }
    assets
        .syntaxes
        .find_syntax_by_extension(name)
        .or_else(|| assets.syntaxes.find_syntax_by_extension(extension))
        .or_else(|| {
            assets
                .syntaxes
                .find_syntax_by_first_line(text.lines().next().unwrap_or(""))
        })
}

fn syntax_style(stack: &ScopeStack, assets: &SyntaxAssets, palette: &Palette) -> Style {
    let color = stack.as_slice().iter().rev().find_map(|scope| {
        assets
            .scopes
            .iter()
            .find_map(|(prefix, color)| prefix.is_prefix_of(*scope).then_some(color))
    });
    Style::default().fg(match color {
        Some(SyntaxColor::Muted) => palette.muted,
        Some(SyntaxColor::Title) => palette.title,
        Some(SyntaxColor::Accent) => palette.accent,
        Some(SyntaxColor::Symlink) => palette.symlink,
        None => palette.text,
    })
}

// The input has already been sanitized. Parse once per document, retaining lexer
// state across lines; unknown syntaxes and parser errors remain readable as text.
fn source_document(path: &Path, text: &str, palette: &Palette) -> Document {
    let assets = &*SYNTAX_ASSETS;
    let syntax = syntax_for(assets, path, text);
    let supports_reading = syntax.is_none_or(|syntax| syntax.name == "Plain Text");
    let mut parser = syntax.map(ParseState::new);
    let mut stack = ScopeStack::new();
    let mut lines = Vec::new();
    let mut offset = 0;
    let mut parts = text.split('\n');
    for line in parts.by_ref().take(MAX_LINES) {
        let end = offset + line.len();
        let input_end = end + usize::from(text.as_bytes().get(end) == Some(&b'\n'));
        let input = &text[offset..input_end];
        let mut spans = Vec::new();
        let operations = parser
            .as_mut()
            .and_then(|state| state.parse_line(input, &assets.syntaxes).ok());
        if let Some(operations) = operations {
            let mut position = 0;
            let mut valid = true;
            for (index, operation) in operations {
                let visible_end = index.min(line.len());
                if position < visible_end {
                    spans.push(Span::styled(
                        line[position..visible_end].to_owned(),
                        syntax_style(&stack, assets, palette),
                    ));
                }
                position = visible_end;
                if stack.apply(&operation).is_err() {
                    valid = false;
                    break;
                }
            }
            if valid {
                if position < line.len() {
                    spans.push(Span::styled(
                        line[position..].to_owned(),
                        syntax_style(&stack, assets, palette),
                    ));
                }
            } else {
                parser = None;
                spans.clear();
                spans.push(Span::styled(
                    line.to_owned(),
                    Style::default().fg(palette.text),
                ));
            }
        } else {
            parser = None;
            spans.push(Span::styled(
                line.to_owned(),
                Style::default().fg(palette.text),
            ));
        }
        lines.push(Line::from(spans));
        offset = input_end;
    }
    Document {
        lines,
        source_lines: None,
        markdown_source: None,
        supports_reading,
        notice: parts
            .next()
            .map(|_| format!("Preview is limited to {MAX_LINES} lines.")),
    }
}

fn markdown_document(text: &str, palette: &Palette) -> Document {
    let original = Arc::from(text);
    let text = sanitize(text);
    let source = source_document(Path::new("source.md"), &text, palette);
    let source_truncated = source.notice.is_some();
    let source_lines = source.lines;
    let mut out = Lines::new();
    let mut style = Style::default().fg(palette.text);
    let mut styles = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(&text, options) {
        if out.truncated {
            break;
        }
        match event {
            Event::Start(tag) => {
                styles.push(style);
                match tag {
                    Tag::Heading { .. } => {
                        out.end_block();
                        style = style.fg(palette.title).add_modifier(Modifier::BOLD);
                    }
                    Tag::CodeBlock(_) => {
                        out.end_block();
                        style = style.fg(palette.accent);
                    }
                    Tag::BlockQuote(_) => {
                        out.end_block();
                        out.push("│ ", Style::default().fg(palette.muted));
                    }
                    Tag::List(start) => lists.push(start),
                    Tag::Item => {
                        out.end_block();
                        let indent = "  ".repeat(lists.len().saturating_sub(1));
                        let marker = match lists.last_mut() {
                            Some(Some(number)) => {
                                let result = format!("{number}. ");
                                *number = number.saturating_add(1);
                                result
                            }
                            _ => "• ".to_string(),
                        };
                        out.push(
                            &format!("{indent}{marker}"),
                            Style::default().fg(palette.accent),
                        );
                    }
                    Tag::Emphasis => style = style.add_modifier(Modifier::ITALIC),
                    Tag::Strong => style = style.add_modifier(Modifier::BOLD),
                    Tag::Strikethrough => style = style.add_modifier(Modifier::CROSSED_OUT),
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        links.push(sanitize(&dest_url));
                        style = style.fg(palette.symlink).add_modifier(Modifier::UNDERLINED);
                    }
                    _ => {}
                }
            }
            Event::End(tag) => {
                match tag {
                    TagEnd::Heading(_)
                    | TagEnd::Paragraph
                    | TagEnd::CodeBlock
                    | TagEnd::BlockQuote(_)
                    | TagEnd::Item
                    | TagEnd::HtmlBlock => out.end_block(),
                    TagEnd::List(_) => {
                        lists.pop();
                    }
                    TagEnd::Link | TagEnd::Image => {
                        if let Some(destination) = links.pop()
                            && !destination.is_empty()
                        {
                            out.push(
                                &format!(" ({destination})"),
                                Style::default().fg(palette.muted),
                            );
                        }
                    }
                    _ => {}
                }
                if let Some(previous) = styles.pop() {
                    style = previous;
                }
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                out.push(&sanitize(&text), style)
            }
            Event::Code(text) => out.push(&sanitize(&text), style.fg(palette.accent)),
            Event::SoftBreak => out.push(" ", style),
            Event::HardBreak => out.newline(),
            Event::Rule => {
                out.end_block();
                out.push("────────", Style::default().fg(palette.muted));
                out.newline();
            }
            Event::TaskListMarker(done) => out.push(if done { "[x] " } else { "[ ] " }, style),
            Event::FootnoteReference(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                out.push(&sanitize(&text), style)
            }
        }
    }
    let mut doc = out.finish(None);
    if source_truncated && doc.notice.is_none() {
        doc.notice = Some(format!("Preview is limited to {MAX_LINES} lines."));
    }
    doc.source_lines = Some(source_lines);
    doc.markdown_source = Some(original);
    doc
}

fn json_document(text: &str, palette: &Palette) -> Document {
    let json = text.strip_prefix('\u{feff}').unwrap_or(text);
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(_) => format_json(json, palette),
        Err(error) => {
            let mut doc = source_document(Path::new("source.json"), &sanitize(text), palette);
            doc.supports_reading = false;
            let message = format!("Invalid JSON: {error}");
            doc.notice = Some(match doc.notice {
                Some(notice) => format!("{message} {notice}"),
                None => message,
            });
            doc
        }
    }
}

// Format without deserializing values, preserving duplicate keys and number spelling.
fn format_json(text: &str, palette: &Palette) -> Document {
    let mut out = Lines::new();
    let base = Style::default().fg(palette.text);
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut depth: usize = 0;
    let mut previous = 0;
    while i < bytes.len() && !out.truncated {
        let byte = bytes[i];
        if byte.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match byte {
            b'{' | b'[' => {
                out.push(&text[i..i + 1], base);
                depth += 1;
                let next = bytes[i + 1..]
                    .iter()
                    .copied()
                    .find(|b| !b.is_ascii_whitespace());
                if !matches!(next, Some(b'}' | b']')) {
                    out.newline();
                    out.push(&"  ".repeat(depth), base);
                }
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if !matches!(previous, b'{' | b'[') {
                    out.newline();
                    out.push(&"  ".repeat(depth), base);
                }
                out.push(&text[i..i + 1], base);
                i += 1;
            }
            b',' => {
                out.push(",", base);
                out.newline();
                out.push(&"  ".repeat(depth), base);
                i += 1;
            }
            b':' => {
                out.push(": ", base);
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                    } else if bytes[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                out.push(&sanitize(&text[start..i]), base);
            }
            _ => {
                let start = i;
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && !matches!(bytes[i], b',' | b']' | b'}')
                {
                    i += 1;
                }
                out.push(&text[start..i], base);
            }
        }
        previous = byte;
    }
    let formatted = out.finish(None);
    let text = formatted
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut doc = source_document(Path::new("source.json"), &text, palette);
    doc.supports_reading = false;
    doc.notice = formatted.notice;
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn palette() -> Palette {
        Palette {
            accent: Color::Green,
            text: Color::White,
            muted: Color::DarkGray,
            title: Color::Yellow,
            error: Color::Red,
            symlink: Color::Cyan,
            emphasis_foreground: Color::Black,
            emphasis_background: Color::White,
            reverse_emphasis: false,
            dim_muted: false,
        }
    }
    fn text(doc: &Document) -> String {
        lines_text(&doc.lines)
    }

    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn markdown_retains_original_source_for_renderer() {
        let input = "# Heading\r\n\tText \u{1b}";
        let doc = markdown_document(input, &palette());
        assert_eq!(doc.markdown_source.as_deref(), Some(input));
        assert!(!lines_text(doc.source_lines.as_ref().unwrap()).contains('\u{1b}'));
    }

    #[test]
    fn markdown_parses_styles_and_preserves_link_destination() {
        let doc = markdown_document(
            "# Title\n\n- *word* and [site](https://example.org)\n\n```\ncode\n```",
            &palette(),
        );
        let rendered = text(&doc);
        assert!(rendered.contains("Title"));
        assert!(rendered.contains("• word and site (https://example.org)"));
        assert!(rendered.contains("code"));
        assert!(doc.lines.iter().flat_map(|line| &line.spans).any(
            |span| span.content == "word" && span.style.add_modifier.contains(Modifier::ITALIC)
        ));
    }

    #[test]
    fn markdown_reflows_soft_breaks_but_keeps_hard_and_block_breaks() {
        let doc = markdown_document(
            "first *styled\nwords* continue\nhere\n\nnext  \nhard\n\n```text\n  code\n  line\n```",
            &palette(),
        );
        assert_eq!(
            text(&doc),
            "first styled words continue here\nnext\nhard\n  code\n  line"
        );
        assert!(doc.lines[0].spans.iter().any(|span| {
            span.content.contains("words") && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
    }

    #[test]
    fn markdown_source_keeps_markup_newlines_and_sanitizes_controls() {
        let doc = markdown_document("# title\r\n*line*\t\x1b\n\n", &palette());
        assert_eq!(
            lines_text(doc.source_lines.as_ref().unwrap()),
            "# title\n*line*    �\n\n"
        );
        assert_eq!(text(&doc), "title\nline    �");
        assert!(json_document("{}", &palette()).source_lines.is_none());
    }

    #[test]
    fn markdown_source_truncation_is_reported_even_when_prose_reflows() {
        let doc = markdown_document(&"word\n".repeat(MAX_LINES + 1), &palette());
        assert_eq!(doc.source_lines.as_ref().unwrap().len(), MAX_LINES);
        assert_eq!(doc.lines.len(), 1);
        assert!(doc.notice.unwrap().contains("limited"));
    }

    #[test]
    fn markdown_decoded_entities_cannot_inject_terminal_controls() {
        let doc = markdown_document(
            "&#27;]52;c;payload&#7; [link](https://example.org/&#27;)",
            &palette(),
        );
        assert!(
            doc.lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.content.chars().all(|ch| !ch.is_control()))
        );
    }

    #[test]
    fn json_is_pretty_and_invalid_json_stays_readable() {
        let doc = json_document("{\"key\":1}", &palette());
        assert!(text(&doc).contains("\"key\": 1"));
        assert!(
            doc.lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg != Some(palette().text))
        );
        let invalid = json_document("{broken\u{1b}", &palette());
        assert!(text(&invalid).contains("{broken�"));
        assert!(invalid.notice.unwrap().starts_with("Invalid JSON:"));
    }

    #[test]
    fn json_preserves_order_duplicates_number_text_and_bom() {
        let doc = json_document(
            "\u{feff}{\"z\":1.2300e+900,\"a\":-0,\"z\":123456789012345678901234567890,\"empty\":[]}",
            &palette(),
        );
        assert!(doc.notice.is_none());
        assert_eq!(
            text(&doc),
            "{\n  \"z\": 1.2300e+900,\n  \"a\": -0,\n  \"z\": 123456789012345678901234567890,\n  \"empty\": []\n}"
        );
    }

    #[test]
    fn json_formatting_stops_at_line_limit() {
        let json = format!(
            "{}{}0{}",
            "[".repeat(100),
            "0,".repeat(MAX_LINES + 100),
            "]".repeat(100)
        );
        let doc = json_document(&json, &palette());
        assert_eq!(doc.lines.len(), MAX_LINES);
        assert!(doc.notice.unwrap().contains("limited"));
    }

    #[test]
    fn source_highlights_common_languages_and_shebangs() {
        let directory = tempfile::tempdir().unwrap();
        for (name, input) in [
            ("example.rs", "fn main() { let value = \"hello\"; }\n"),
            ("example.py", "def greet():\n    return \"hello\"\n"),
            ("example.yaml", "name: \"hello\"\n"),
            ("script", "#!/bin/sh\necho \"hello\"\n"),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, input).unwrap();
            let doc = load(&path, &palette()).unwrap();
            assert!(!doc.supports_reading, "{name}");
            assert_eq!(text(&doc), input, "{name}");
            assert!(
                doc.lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.style.fg != Some(palette().text)),
                "{name}"
            );
        }
    }

    #[test]
    fn additional_grammars_color_real_files_and_preserve_comment_state() {
        let directory = tempfile::tempdir().unwrap();
        for (name, input, colored, comment) in [
            (
                "Cargo.toml",
                "[package]\nname = \"demo\"\n# note\n",
                "demo",
                "note",
            ),
            (
                "Cargo.lock",
                "[[package]]\nname = \"demo\"\n# note\n",
                "demo",
                "note",
            ),
            (
                "settings.jsonc",
                "{\n // note\n \"name\": \"demo\",\n}\n",
                "demo",
                "note",
            ),
            (
                "settings.json5",
                "{\n // note\n name: 'demo',\n}\n",
                "demo",
                "note",
            ),
            (
                "component.jsx",
                "const view = <div>hello</div>;\n/* first\nsecond */\n",
                "div",
                "second",
            ),
            ("Dockerfile", "FROM alpine:3\n# note\n", "FROM", "note"),
            ("Dockerfile.dev", "FROM alpine:3\n# note\n", "FROM", "note"),
            (
                "component.vue",
                "<template><div>hello</div></template>\n<script>\n/* first\nsecond */\nconst value = \"demo\";\n</script>\n",
                "div",
                "second",
            ),
            (
                "component.svelte",
                "<script>\n/* first\nsecond */\nconst value = \"demo\";\n</script>\n<div>{value}</div>\n",
                "div",
                "second",
            ),
            (
                "schema.graphql",
                "type Query {\n  name: String\n}\n# note\n",
                "Query",
                "note",
            ),
            (
                "schema.gql",
                "type Query {\n  name: String\n}\n# note\n",
                "Query",
                "note",
            ),
            (
                "schema.prisma",
                "model User {\n  id Int @id\n}\n// note\n",
                "model",
                "note",
            ),
            (
                "main.tf",
                "resource \"thing\" \"demo\" {}\n# note\n",
                "resource",
                "note",
            ),
            ("values.tfvars", "name = \"demo\"\n# note\n", "demo", "note"),
            ("config.hcl", "name = \"demo\"\n# note\n", "demo", "note"),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, input).unwrap();
            let doc = load(&path, &palette()).unwrap();
            assert_eq!(text(&doc), input, "{name}");
            assert!(!doc.supports_reading, "{name}");
            assert!(doc.notice.is_none(), "{name}: {:?}", doc.notice);
            assert!(
                doc.lines.iter().flat_map(|line| &line.spans).any(|span| {
                    span.content.contains(colored) && span.style.fg != Some(palette().text)
                }),
                "{name}: missing colored {colored}"
            );
            assert!(
                doc.lines.iter().flat_map(|line| &line.spans).any(|span| {
                    span.content.contains(comment) && span.style.fg == Some(palette().muted)
                }),
                "{name}: missing comment {comment}"
            );
            if name.ends_with(".vue") || name.ends_with(".svelte") {
                assert!(
                    doc.lines.iter().flat_map(|line| &line.spans).any(|span| {
                        span.content.contains("demo") && span.style.fg == Some(palette().accent)
                    }),
                    "{name}: embedded JavaScript string"
                );
                assert!(
                    doc.lines.iter().flat_map(|line| &line.spans).any(|span| {
                        span.content.contains("const") && span.style.fg == Some(palette().title)
                    }),
                    "{name}: embedded JavaScript keyword"
                );
            }
        }
    }

    #[test]
    fn jsonc_config_names_bypass_strict_json_formatting() {
        let directory = tempfile::tempdir().unwrap();
        let input = "{\n  // note\n  \"compilerOptions\": {},\n}\n";
        for name in [
            "tsconfig.json",
            "tsconfig.build.json",
            "jsconfig.json",
            "jsconfig.base.json",
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, input).unwrap();
            let doc = load(&path, &palette()).unwrap();
            assert_eq!(text(&doc), input, "{name}");
            assert!(doc.notice.is_none(), "{name}");
            assert!(
                doc.lines.iter().flat_map(|line| &line.spans).any(|span| {
                    span.content.contains("note") && span.style.fg == Some(palette().muted)
                }),
                "{name}"
            );
        }
        let path = directory.path().join("ordinary.json");
        std::fs::write(&path, input).unwrap();
        assert!(
            load(&path, &palette())
                .unwrap()
                .notice
                .unwrap()
                .starts_with("Invalid JSON:")
        );
    }

    #[test]
    fn typescript_highlights_types_and_keeps_multiline_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("example.ts");
        let input = "interface Box<T> {\n  value: string;\n  /* first\n     second */\n}\n";
        std::fs::write(&path, input).unwrap();
        let doc = load(&path, &palette()).unwrap();
        assert!(!doc.supports_reading);
        assert_eq!(text(&doc), input);
        assert!(doc.lines[0].spans.iter().any(|span| {
            span.content.contains("interface") && span.style.fg == Some(palette().title)
        }));
        assert!(doc.lines[1].spans.iter().any(|span| {
            span.content.contains("string") && span.style.fg == Some(palette().symlink)
        }));
        assert!(doc.lines[3].spans.iter().any(|span| {
            span.content.contains("second") && span.style.fg == Some(palette().muted)
        }));
    }

    #[test]
    fn typescript_react_highlights_jsx_and_types() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("example.tsx");
        let input = "interface Props { title: string }\nconst view = <div>{title}</div>;\n";
        std::fs::write(&path, input).unwrap();
        let doc = load(&path, &palette()).unwrap();
        assert!(!doc.supports_reading);
        assert_eq!(text(&doc), input);
        assert!(doc.lines[0].spans.iter().any(|span| {
            span.content.contains("interface") && span.style.fg == Some(palette().title)
        }));
        assert!(doc.lines[1].spans.iter().any(|span| {
            span.content.contains("div") && span.style.fg == Some(palette().title)
        }));
    }

    #[test]
    fn source_tracks_multiline_comment_and_preserves_unknown_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("example.c");
        std::fs::write(&path, "int x;\n/* start\nstill comment\nend */\nint y;\n").unwrap();
        let doc = load(&path, &palette()).unwrap();
        let comment = &doc.lines[2].spans;
        assert!(
            comment
                .iter()
                .any(|span| span.content.contains("still comment")
                    && span.style.fg == Some(palette().muted))
        );
        assert!(
            doc.lines[4]
                .spans
                .iter()
                .any(|span| span.style.fg != Some(palette().muted))
        );

        let path = directory.path().join("unknown.unrecognizedextension");
        std::fs::write(&path, "  alpha\r\n\tbeta\u{1b}\n\n").unwrap();
        let doc = load(&path, &palette()).unwrap();
        assert!(doc.supports_reading);
        assert_eq!(text(&doc), "  alpha\n    beta�\n\n");
        assert!(
            doc.lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.fg == Some(palette().text))
        );
    }

    #[test]
    fn markdown_source_is_highlighted_and_source_limit_is_enforced() {
        let doc = markdown_document("# Heading\n\n**bold**\n", &palette());
        assert!(!doc.supports_reading);
        let source = doc.source_lines.unwrap();
        assert_eq!(lines_text(&source), "# Heading\n\n**bold**\n");
        assert!(
            source
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg != Some(palette().text))
        );

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.rs");
        std::fs::write(&path, "let x = 1;\n".repeat(MAX_LINES + 1)).unwrap();
        let doc = load(&path, &palette()).unwrap();
        assert_eq!(doc.lines.len(), MAX_LINES);
        assert!(doc.notice.unwrap().contains("limited"));
    }

    #[test]
    fn controls_are_safe_and_line_count_is_bounded() {
        assert_eq!(sanitize("a\x1b[31m\r\nb\t\u{85}"), "a�[31m\nb    �");
        let mut output = Lines::new();
        output.push(&"x\n".repeat(MAX_LINES + 1), Style::default());
        let doc = output.finish(None);
        assert_eq!(doc.lines.len(), MAX_LINES);
        assert!(doc.notice.unwrap().contains("limited"));
    }

    #[test]
    fn empty_markdown_retains_source_mode() {
        let path =
            std::env::temp_dir().join(format!("de-empty-document-test-{}.MD", std::process::id()));
        std::fs::write(&path, []).unwrap();
        let doc = load(&path, &palette()).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(lines_text(doc.source_lines.as_ref().unwrap()), "");
        assert_eq!(doc.notice.as_deref(), Some("Empty file."));
    }

    #[test]
    fn load_checks_file_content_and_size() {
        let path = std::env::temp_dir().join(format!("de-document-test-{}", std::process::id()));
        std::fs::write(&path, []).unwrap();
        assert_eq!(
            load(&path, &palette()).unwrap().notice.as_deref(),
            Some("Empty file.")
        );
        std::fs::write(&path, [b'a', 0, b'b']).unwrap();
        assert!(load(&path, &palette()).unwrap_err().contains("Binary"));
        std::fs::write(&path, [0xff]).unwrap();
        assert!(load(&path, &palette()).unwrap_err().contains("UTF-8"));
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert!(load(&path, &palette()).unwrap_err().contains("too large"));
        std::fs::remove_file(path).unwrap();
        assert!(
            load(&std::env::temp_dir(), &palette())
                .unwrap_err()
                .contains("regular files")
        );
    }
}
