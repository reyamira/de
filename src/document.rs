use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, LazyLock};

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
    /// Original UTF-8 bytes, before terminal sanitization or JSON formatting.
    pub source: Arc<str>,
    pub markdown_source: Option<Arc<str>>,
    pub supports_reading: bool,
    pub notice: Option<String>,
    /// True only when display rows retain original source line boundaries.
    pub source_lines: bool,
}

// Bound both input and the number of independently allocated display lines.
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_LINES: usize = 20_000;

/// Load a regular UTF-8 file for display without interpreting executable content.
pub fn load(path: &Path, palette: &Palette) -> Result<Document, String> {
    load_mode(path, palette, false)
}

/// Load original source layout, including JSON, for source-line search targets.
pub fn load_source(path: &Path, palette: &Palette) -> Result<Document, String> {
    load_mode(path, palette, true)
}

/// Build source rows from the same loaded snapshot, without reading the file again.
pub(crate) fn source_from_text(path: &Path, text: &Arc<str>, palette: &Palette) -> Document {
    let mut document = source_document(path, &sanitize(text), palette);
    document.source = Arc::clone(text);
    document
}

fn load_mode(path: &Path, palette: &Palette, raw: bool) -> Result<Document, String> {
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
    let is_markdown = ["md", "markdown", "mdown", "markdn"]
        .iter()
        .any(|ext| extension.eq_ignore_ascii_case(ext));
    if text.is_empty() {
        return Ok(Document {
            lines: vec![Line::default()],
            source: Arc::from(text),
            notice: Some("Empty file.".into()),
            source_lines: true,
            markdown_source: is_markdown.then(|| Arc::from(text)),
            supports_reading: !is_markdown
                && !extension.eq_ignore_ascii_case("json")
                && syntax_for(&SYNTAX_ASSETS, path, text)
                    .is_none_or(|syntax| syntax.name == "Plain Text"),
        });
    }
    let mut document = if !raw && extension.eq_ignore_ascii_case("json") && !is_jsonc_config(path) {
        json_document(text, palette)
    } else if is_markdown {
        markdown_document(text, palette)
    } else {
        source_document(path, &sanitize(text), palette)
    };
    document.source = Arc::from(text);
    Ok(document)
}

/// Borrow source lines with the display policy: CRLF, LF, and CR each break a
/// line; a trailing break leaves an empty final line.
pub(crate) fn logical_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut remaining = Some(text);
    std::iter::from_fn(move || {
        let text = remaining.take()?;
        match text.bytes().position(|byte| byte == b'\r' || byte == b'\n') {
            Some(end) => {
                let delimiter = 1 + usize::from(
                    text.as_bytes()[end] == b'\r' && text.as_bytes().get(end + 1) == Some(&b'\n'),
                );
                remaining = Some(&text[end + delimiter..]);
                Some(&text[..end])
            }
            None => Some(text),
        }
    })
}

// Never allow C0/C1 controls, including terminal escape sequences, through to the backend.
// Preserve line breaks and expand tabs so terminal tab stops cannot affect the layout.
fn sanitize(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    for (index, line) in logical_lines(text).enumerate() {
        if index != 0 {
            clean.push('\n');
        }
        for ch in line.chars() {
            match ch {
                '\t' => clean.push_str("    "),
                c if c.is_control() => clean.push('\u{fffd}'),
                c => clean.push(c),
            }
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
            source: Arc::from(""),
            markdown_source: None,
            supports_reading: false,
            source_lines: false,
            notice,
        }
    }
}

struct SyntaxAssets {
    syntaxes: SyntaxSet,
    scopes: [(Scope, SyntaxColor); 11],
    markdown_scopes: [Scope; 13],
}

#[derive(Clone, Copy)]
enum SyntaxColor {
    Comment,
    Keyword,
    Entity,
    String,
    Number,
    Constant,
    Support,
}

// Generate Sublime embed rules from the same inventory used for source files.
// Exact spellings win before case-insensitive matches, so extensions such as
// C/c and M/m can retain their distinct grammars.
fn markdown_syntax(syntaxes: &[SyntaxDefinition]) -> String {
    let mut labels = BTreeMap::new();
    for syntax in syntaxes {
        let scope = syntax.scope.to_string();
        if scope.starts_with("text.html.markdown") {
            continue;
        }
        for label in syntax
            .file_extensions
            .iter()
            .chain(std::iter::once(&syntax.name))
        {
            if !label.is_empty() && !label.chars().any(char::is_whitespace) {
                labels.insert(label.clone(), scope.clone());
            }
        }
    }
    for (alias, extension) in [
        ("shell", "sh"),
        ("node", "js"),
        ("python3", "py"),
        ("docker", "Dockerfile"),
    ] {
        if let Some(scope) = labels.get(extension).cloned() {
            labels.insert(alias.to_owned(), scope);
        }
    }
    let mut folded: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (label, scope) in &labels {
        folded
            .entry(label.to_ascii_lowercase())
            .or_default()
            .insert(scope.clone());
    }
    let mut rules = String::new();
    let mut add_rule = |label: &str, scope: &str, insensitive: bool| {
        let mut pattern = String::new();
        for ch in label.chars() {
            if !ch.is_ascii_alphanumeric() && ch != '_' {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        let pattern = pattern.replace('\'', "''");
        let pattern = if insensitive {
            format!("(?i:{pattern})")
        } else {
            pattern
        };
        writeln!(rules, "    - match: '{{{{fenced_code_block_start}}}}[ \\t]*({pattern})(?=[ \\t]|$)[^\\n]*$'\n      captures:\n        1: punctuation.definition.raw.code-fence.begin.markdown\n        2: constant.other.language-name.markdown\n      embed: scope:{scope}\n      embed_scope: markup.raw.code-fence.markdown\n      escape: '^[ ]{{0,3}}(\\1(?:`(?<=``)|~(?<=~~))*)[ \\t]*$'\n      escape_captures:\n        1: punctuation.definition.raw.code-fence.end.markdown").unwrap();
    };
    for (label, scope) in &labels {
        add_rule(label, scope, false);
    }
    for (label, scopes) in &folded {
        if scopes.len() == 1 {
            add_rule(label, scopes.iter().next().unwrap(), true);
        }
    }
    include_str!("../assets/syntaxes/Markdown.sublime-syntax").replacen(
        "  fenced-code-block:\n",
        &format!("  fenced-code-block:\n{rules}"),
        1,
    )
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
        let markdown = markdown_syntax(builder.syntaxes());
        builder.add(
            SyntaxDefinition::load_from_str(&markdown, true, None)
                .expect("generated Markdown grammar must be valid"),
        );
        builder.build()
    },
    scopes: [
        (Scope::new("comment").unwrap(), SyntaxColor::Comment),
        (Scope::new("keyword").unwrap(), SyntaxColor::Keyword),
        (Scope::new("storage").unwrap(), SyntaxColor::Keyword),
        (Scope::new("entity").unwrap(), SyntaxColor::Entity),
        (Scope::new("string").unwrap(), SyntaxColor::String),
        (Scope::new("markup.heading").unwrap(), SyntaxColor::Entity),
        (Scope::new("markup.bold").unwrap(), SyntaxColor::Entity),
        (Scope::new("markup.italic").unwrap(), SyntaxColor::String),
        (Scope::new("constant.numeric").unwrap(), SyntaxColor::Number),
        (Scope::new("constant").unwrap(), SyntaxColor::Constant),
        (Scope::new("support").unwrap(), SyntaxColor::Support),
    ],
    markdown_scopes: [
        "text.html.markdown",
        "markup.heading",
        "markup.bold",
        "markup.italic",
        "meta.link.inline.description",
        "meta.image.inline.description",
        "markup.underline.link",
        "markup.raw.inline",
        "punctuation.definition",
        "punctuation.definition.list_item",
        "punctuation.definition.blockquote",
        "markup.list.numbered.bullet",
        "markup.raw.code-fence",
    ]
    .map(|name| Scope::new(name).unwrap()),
});
/// List the names and file associations of the loaded syntax definitions.
pub fn syntax_definitions() -> impl Iterator<Item = (&'static str, &'static [String])> {
    let syntaxes = SYNTAX_ASSETS.syntaxes.syntaxes();
    let markdown = syntaxes
        .iter()
        .rposition(|syntax| syntax.name == "Markdown");
    syntaxes
        .iter()
        .enumerate()
        // The defaults retain their linked contexts. Only expose the replacement.
        .filter(move |(index, syntax)| syntax.name != "Markdown" || Some(*index) == markdown)
        .map(|(_, syntax)| (syntax.name.as_str(), syntax.file_extensions.as_slice()))
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
    if extension.eq_ignore_ascii_case("svg") {
        return assets.syntaxes.find_syntax_by_extension("xml");
    }
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
    let scopes = stack.as_slice();
    let [
        root,
        heading,
        bold,
        italic,
        link_label,
        image_label,
        link_url,
        inline_code,
        punctuation,
        list_marker,
        quote_marker,
        numbered_marker,
        fence,
    ] = &assets.markdown_scopes;
    let markdown = scopes
        .first()
        .is_some_and(|scope| root.is_prefix_of(*scope));
    let has = |prefix: &Scope| scopes.iter().any(|scope| prefix.is_prefix_of(*scope));
    let color = scopes.iter().rev().find_map(|scope| {
        assets
            .scopes
            .iter()
            .find_map(|(prefix, color)| prefix.is_prefix_of(*scope).then_some(color))
    });
    let mut style = Style::default().fg(match color {
        Some(SyntaxColor::Comment) => palette.syntax.comment,
        Some(SyntaxColor::Keyword) => palette.syntax.keyword,
        Some(SyntaxColor::Entity) => palette.syntax.entity,
        Some(SyntaxColor::String) => palette.syntax.string,
        Some(SyntaxColor::Number) => palette.syntax.number,
        Some(SyntaxColor::Constant) => palette.syntax.constant,
        Some(SyntaxColor::Support) => palette.syntax.support,
        None if markdown && !has(fence) => palette.markdown.text,
        None => palette.syntax.text,
    });
    if markdown && !has(fence) {
        style = style.fg(palette.markdown.text);
        if has(list_marker) || has(quote_marker) || has(numbered_marker) {
            style = style.fg(palette.markdown.marker);
        } else if has(punctuation) {
            style = style.fg(palette.markdown.punctuation);
        } else if has(inline_code) {
            style = style.fg(palette.markdown.code);
        } else if has(link_label) || has(image_label) {
            style = style
                .fg(palette.markdown.link)
                .add_modifier(Modifier::UNDERLINED);
        } else if has(link_url) {
            style = style.fg(palette.markdown.link_url);
        } else if has(heading) {
            style = style.fg(palette.markdown.heading);
        }
        if !has(punctuation) {
            if has(heading) || has(bold) {
                style = style.add_modifier(Modifier::BOLD);
            }
            if has(italic) {
                style = style.add_modifier(Modifier::ITALIC);
            }
        }
    }
    style
}

/// Grammar identity shared by source previews and the live editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SourceSyntax(Option<usize>);

impl SourceSyntax {
    pub(crate) fn for_path(path: &Path, first_line: &str) -> Self {
        let assets = &*SYNTAX_ASSETS;
        Self(syntax_for(assets, path, first_line).map(|selected| {
            assets
                .syntaxes
                .syntaxes()
                .iter()
                .position(|syntax| std::ptr::eq(syntax, selected))
                .expect("selected syntax belongs to the shared syntax set")
        }))
    }

    pub(crate) fn name(self) -> Option<&'static str> {
        self.0
            .map(|index| SYNTAX_ASSETS.syntaxes.syntaxes()[index].name.as_str())
    }

    pub(crate) fn state(self) -> SourceState {
        SourceState {
            parser: self
                .0
                .map(|index| ParseState::new(&SYNTAX_ASSETS.syntaxes.syntaxes()[index])),
            stack: ScopeStack::new(),
        }
    }

    fn supports_reading(self) -> bool {
        self.0
            .is_none_or(|index| SYNTAX_ASSETS.syntaxes.syntaxes()[index].name == "Plain Text")
    }
}

/// The outgoing lexical boundary of a line, including embedded grammar state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceState {
    parser: Option<ParseState>,
    stack: ScopeStack,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SourceStyleRange {
    pub(crate) bytes: std::ops::Range<usize>,
    pub(crate) style: Style,
}

impl SourceState {
    /// `input` includes its real line break; ranges exclude that line break.
    /// The caller retains the original bytes and owns any display substitution.
    pub(crate) fn line(
        &mut self,
        input: &str,
        visible_len: usize,
        palette: &Palette,
    ) -> Vec<SourceStyleRange> {
        let assets = &*SYNTAX_ASSETS;
        let operations = self
            .parser
            .as_mut()
            .and_then(|parser| parser.parse_line(input, &assets.syntaxes).ok());
        let mut ranges = Vec::new();
        if let Some(operations) = operations {
            let mut position = 0;
            let mut valid = true;
            for (index, operation) in operations {
                let end = index.min(visible_len);
                if position < end {
                    ranges.push(SourceStyleRange {
                        bytes: position..end,
                        style: syntax_style(&self.stack, assets, palette),
                    });
                }
                position = end;
                if self.stack.apply(&operation).is_err() {
                    valid = false;
                    break;
                }
            }
            if valid {
                if position < visible_len {
                    ranges.push(SourceStyleRange {
                        bytes: position..visible_len,
                        style: syntax_style(&self.stack, assets, palette),
                    });
                }
                return ranges;
            }
        }
        // Retain the source preview's readable fallback after a parser failure.
        self.parser = None;
        ranges.clear();
        ranges.push(SourceStyleRange {
            bytes: 0..visible_len,
            style: Style::default().fg(palette.syntax.text),
        });
        ranges
    }
}

// The input has already been sanitized. Parse once per document, retaining lexer
// state across lines; unknown syntaxes and parser errors remain readable as text.
fn source_document(path: &Path, text: &str, palette: &Palette) -> Document {
    let syntax = SourceSyntax::for_path(path, text.lines().next().unwrap_or(""));
    let supports_reading = syntax.supports_reading();
    let mut state = syntax.state();
    let mut lines = Vec::new();
    let mut offset = 0;
    let mut parts = text.split('\n');
    for line in parts.by_ref().take(MAX_LINES) {
        let end = offset + line.len();
        let input_end = end + usize::from(text.as_bytes().get(end) == Some(&b'\n'));
        let input = &text[offset..input_end];
        let spans = state
            .line(input, line.len(), palette)
            .into_iter()
            .map(|range| Span::styled(line[range.bytes].to_owned(), range.style))
            .collect::<Vec<_>>();
        lines.push(Line::from(spans));
        offset = input_end;
    }
    Document {
        lines,
        source: Arc::from(""),
        markdown_source: None,
        supports_reading,
        source_lines: true,
        notice: parts
            .next()
            .map(|_| format!("Preview is limited to {MAX_LINES} lines.")),
    }
}

fn markdown_document(text: &str, palette: &Palette) -> Document {
    let mut doc = source_document(Path::new("source.md"), &sanitize(text), palette);
    doc.markdown_source = Some(Arc::from(text));
    doc.source = Arc::from(text);
    doc.supports_reading = false;
    doc
}

/// Create the Markdown sample used by the theme picker.
pub fn markdown_sample(palette: &Palette) -> Document {
    markdown_document(
        r#"# Heading level 1

Body text with **bold**, *italic*, ***bold italic***, ~~strikethrough~~,
a [link label](https://example.com/path) and `inline code`.

## Heading level 2
### Heading level 3
#### Heading level 4
##### Heading level 5
###### Heading level 6

> A quoted passage shows the quote text and bar.

- Bullet item
  - Nested bullet
1. Numbered item
   1. Nested number

| Table | Example |
| --- | --- |
| First | Normal row |
| Second | Striped row |
| Third | Another row |

```jsx
// Syntax colors
function sample(value) {
  const message = "hello";
  return true ? Math.max(value, 42) : null;
}
```

```mermaid
flowchart LR
    A[Start] --> B[Finish]
```
"#,
        palette,
    )
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
    doc.source_lines = false;
    doc.notice = formatted.notice;
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn palette() -> Palette {
        let mut p = crate::Theme::auto().palette();
        p.accent = Color::Green;
        p.text = Color::White;
        p.muted = Color::DarkGray;
        p.title = Color::Yellow;
        p.error = Color::Red;
        p.symlink = Color::Cyan;
        p.emphasis_foreground = Color::Black;
        p.emphasis_background = Color::White;
        p.reverse_emphasis = false;
        p.syntax.text = p.text;
        p.syntax.comment = p.muted;
        p.syntax.keyword = p.title;
        p.syntax.entity = p.title;
        p.syntax.string = p.accent;
        p.syntax.number = p.symlink;
        p.syntax.constant = p.symlink;
        p.syntax.support = p.symlink;
        p.markdown.text = p.text;
        p.markdown.heading = p.title;
        p.markdown.link = p.symlink;
        p.markdown.link_url = p.muted;
        p.markdown.code = p.accent;
        p.markdown.punctuation = p.muted;
        p.markdown.marker = p.accent;
        p
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
    fn source_and_markdown_roles_control_their_own_spans() {
        let mut p = palette();
        p.syntax.keyword = Color::Red;
        p.syntax.string = Color::Blue;
        p.syntax.number = Color::Magenta;
        p.syntax.comment = Color::Cyan;
        p.markdown.heading = Color::Green;
        p.markdown.link = Color::Yellow;
        p.markdown.code = Color::Red;
        let source = source_document(
            Path::new("sample.rs"),
            "let x = \"hello\"; let n = 42; // note",
            &p,
        );
        let source_has = |fragment: &str, color| {
            source
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains(fragment) && span.style.fg == Some(color))
        };
        assert!(source_has("let", Color::Red));
        assert!(source_has("hello", Color::Blue));
        assert!(source_has("42", Color::Magenta));
        assert!(source_has("note", Color::Cyan));
        let markdown = markdown_document("# Heading\n\n[site](url) and `code`", &p);
        let markdown_has = |fragment: &str, color| {
            markdown
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains(fragment) && span.style.fg == Some(color))
        };
        assert!(markdown_has("Heading", Color::Green));
        assert!(markdown_has("site", Color::Yellow));
        assert!(markdown_has("code", Color::Red));
    }

    #[test]
    fn markdown_retains_original_source_for_renderer() {
        let input = "# Heading\r\n\tText \u{1b}";
        let doc = markdown_document(input, &palette());
        assert_eq!(doc.markdown_source.as_deref(), Some(input));
        assert_eq!(text(&doc), "# Heading\n    Text �");
    }

    #[test]
    fn markdown_source_styles_literal_markup() {
        let input = "# Heading [site](https://example.org)\n\n- **bold** and *italic* and `code`\n\n> quote\n";
        let doc = markdown_document(input, &palette());
        assert_eq!(text(&doc), input);
        let span = |line: usize, content: &str| {
            doc.lines[line]
                .spans
                .iter()
                .find(|span| span.content == content)
                .unwrap()
                .style
        };
        assert!(span(0, "Heading ").add_modifier.contains(Modifier::BOLD));
        assert_eq!(span(0, "Heading ").fg, Some(palette().title));
        assert_eq!(span(0, "[").fg, Some(palette().muted));
        assert_eq!(span(0, "site").fg, Some(palette().symlink));
        assert!(span(0, "site").add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(span(0, "https://example.org").fg, Some(palette().muted));
        assert_eq!(span(2, "-").fg, Some(palette().accent));
        assert_eq!(span(2, "**").fg, Some(palette().muted));
        assert!(span(2, "bold").add_modifier.contains(Modifier::BOLD));
        assert!(span(2, "italic").add_modifier.contains(Modifier::ITALIC));
        assert_eq!(span(2, "code").fg, Some(palette().accent));
        assert_eq!(span(4, ">").fg, Some(palette().accent));
    }

    #[test]
    fn markdown_fenced_languages_highlight_and_close_without_losing_source() {
        let input = "````rust\nfn main() { let value = \"hello\"; }\n```\nlet still_code = 1;\n````\n\n# After\n\n```unknown\nfn plain = \"text\";\n```\n";
        let doc = markdown_document(input, &palette());
        assert_eq!(text(&doc), input);
        assert!(
            doc.lines[1]
                .spans
                .iter()
                .any(|span| span.content.contains("fn") && span.style.fg == Some(palette().title))
        );
        assert!(
            doc.lines[1]
                .spans
                .iter()
                .any(|span| span.content.contains("hello")
                    && span.style.fg == Some(palette().accent))
        );
        assert!(
            doc.lines[3]
                .spans
                .iter()
                .any(|span| span.content.contains("let") && span.style.fg == Some(palette().title))
        );
        assert!(
            doc.lines[6]
                .spans
                .iter()
                .any(|span| span.content.contains("After")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            doc.lines[9]
                .spans
                .iter()
                .all(|span| span.style.fg == Some(palette().text))
        );
    }

    #[test]
    fn markdown_fenced_language_labels_style_code_and_preserve_source() {
        let cases = [
            ("bash", "echo \"hello\""),
            ("sh", "echo \"hello\""),
            ("json", "{\"key\": \"hello\"}"),
            ("go", "var message = \"hello\""),
            ("java", "String message = \"hello\";"),
            ("sql", "SELECT 'hello';"),
            ("yaml", "message: \"hello\""),
            ("shell", "echo \"hello\""),
            ("js", "const message = \"hello\";"),
            ("mjs", "export const message = \"hello\";"),
            ("cjs", "module.exports = \"hello\";"),
            ("c++", "const char* message = \"hello\";"),
            ("ts", "const message: string = \"hello\";"),
            ("jsx", "const element = <div title=\"hello\" />;"),
            ("tsx", "const element = <div title=\"hello\" />;"),
        ];
        for (label, code) in cases {
            let input = format!("```{label}\n{code}\n```\n");
            let doc = markdown_document(&input, &palette());
            assert_eq!(text(&doc), input, "label: {label}");
            assert_eq!(doc.markdown_source.as_deref(), Some(input.as_str()));
            assert!(
                doc.lines[1].spans.iter().any(|span| {
                    span.content.contains("hello") && span.style.fg == Some(palette().accent)
                }),
                "string should be styled for {label}: {:?}",
                doc.lines[1].spans
            );
        }
    }

    #[test]
    fn markdown_unknown_and_unlabeled_fences_remain_plain() {
        for label in ["unknown-language", ""] {
            let input = format!("```{label}\nfn value = \"hello\";\n# Not a heading\n```\n");
            let doc = markdown_document(&input, &palette());
            assert_eq!(text(&doc), input, "label: {label}");
            assert_eq!(doc.markdown_source.as_deref(), Some(input.as_str()));
            for line in &doc.lines[1..=2] {
                assert!(
                    line.spans.iter().all(|span| {
                        span.style.fg == Some(palette().text)
                            && !span.style.add_modifier.contains(Modifier::BOLD)
                    }),
                    "fence should be plain for {label}: {:?}",
                    line.spans
                );
            }
        }
    }

    #[test]
    fn markdown_fence_retains_multiline_state_and_restores_markdown() {
        let input = "```rust\n/* first\nstill comment */\nlet value = \"hello\";\n```\n# After\n";
        let doc = markdown_document(input, &palette());
        assert_eq!(text(&doc), input);
        assert_eq!(doc.markdown_source.as_deref(), Some(input));
        assert!(doc.lines[2].spans.iter().any(|span| {
            span.content.contains("still comment") && span.style.fg == Some(palette().muted)
        }));
        assert!(doc.lines[3].spans.iter().any(|span| {
            span.content.contains("let") && span.style.fg == Some(palette().title)
        }));
        assert!(doc.lines[5].spans.iter().any(|span| {
            span.content.contains("After") && span.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn markdown_source_keeps_markup_newlines_and_sanitizes_controls() {
        let doc = markdown_document("# title\r\n*line*\t\x1b\n\n", &palette());
        assert_eq!(text(&doc), "# title\n*line*    �\n\n");
        assert!(json_document("{}", &palette()).markdown_source.is_none());
        let doc = markdown_document(&"word\n".repeat(MAX_LINES + 1), &palette());
        assert_eq!(doc.lines.len(), MAX_LINES);
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
            (
                "module.mjs",
                "export const message = \"demo\";\n/* first\nsecond */\n",
                "demo",
                "second",
            ),
            (
                "module.cjs",
                "module.exports = \"demo\";\n/* first\nsecond */\n",
                "demo",
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
        assert_eq!(text(&doc), "# Heading\n\n**bold**\n");
        assert!(
            doc.lines
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
    fn markdown_aliases_retain_rendered_and_source_modes() {
        let directory = tempfile::tempdir().unwrap();
        for extension in ["md", "MD", "markdown", "mdown", "markdn", "MARKDN"] {
            let path = directory.path().join(format!("document.{extension}"));
            for input in ["", "# Heading\n\n**bold**\n"] {
                std::fs::write(&path, input).unwrap();
                let doc = load(&path, &palette()).unwrap();
                assert_eq!(doc.markdown_source.as_deref(), Some(input), "{extension}");
                assert!(!doc.supports_reading, "{extension}");
                assert_eq!(text(&doc), input, "{extension}");
                if input.is_empty() {
                    assert_eq!(doc.notice.as_deref(), Some("Empty file."));
                } else {
                    assert!(doc.lines[0].spans.iter().any(|span| {
                        span.content.contains("Heading")
                            && span.style.add_modifier.contains(Modifier::BOLD)
                    }));
                }
            }
        }
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
