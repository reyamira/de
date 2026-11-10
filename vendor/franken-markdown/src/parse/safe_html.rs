#[derive(Debug, Clone, PartialEq, Eq)]
struct Tag {
    /// ASCII-lowercased element name.
    name: String,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece<'a> {
    Text(&'a str),
    Tag(Tag, &'a str),
    /// Comment, declaration, CDATA or processing instruction: dropped.
    Ignorable,
}
fn markup_at(s: &str) -> Option<(Piece<'_>, usize)> {
    if let Some(rest) = s.strip_prefix("<!--") {
        let end = rest.find("-->")?;
        return Some((Piece::Ignorable, 4 + end + 3));
    }
    if let Some(rest) = s.strip_prefix("<![CDATA[") {
        let end = rest.find("]]>")?;
        return Some((Piece::Ignorable, 9 + end + 3));
    }
    if s.starts_with("<!") || s.starts_with("<?") {
        let end = s.find('>')?;
        return Some((Piece::Ignorable, end + 1));
    }
    let (tag, len) = parse_tag(s)?;
    Some((Piece::Tag(tag, &s[..len]), len))
}

fn parse_tag(s: &str) -> Option<(Tag, usize)> {
    parse_tag_with_destination(s).map(|(tag, len, _)| (tag, len))
}

/// Retain the first supported URL attribute's original value range for rebasing.
/// The range includes its quotes (if any), so an unquoted source can safely
/// become a quoted URL without changing any neighboring source bytes.
fn parse_tag_with_destination(s: &str) -> Option<(Tag, usize, Option<std::ops::Range<usize>>)> {
    let bytes = s.as_bytes();
    let mut i = 1usize;
    let closing = bytes.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    if !bytes.get(i)?.is_ascii_alphabetic() {
        return None;
    }
    while bytes
        .get(i)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-')
    {
        i += 1;
    }
    let name = s[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    let destination_name = match name.as_str() {
        "a" => "href",
        "img" => "src",
        _ => "",
    };
    let mut destination = None;
    let mut saw_destination = false;
    loop {
        let before_space = i;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match bytes.get(i)? {
            b'>' => {
                return Some((tag(name, closing, self_closing, attrs), i + 1, destination));
            }
            b'/' if bytes.get(i + 1) == Some(&b'>') && !closing => {
                self_closing = true;
                return Some((tag(name, closing, self_closing, attrs), i + 2, destination));
            }
            _ if closing => return None,
            // Attributes must be separated from the name and each other.
            _ if i == before_space => return None,
            _ => {}
        }
        let attr_start = i;
        if !bytes
            .get(i)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b':')
        {
            return None;
        }
        while bytes
            .get(i)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
        {
            i += 1;
        }
        let attr_name = s[attr_start..i].to_ascii_lowercase();
        let mut j = i;
        while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        let mut value = String::new();
        if bytes.get(j) == Some(&b'=') {
            j += 1;
            while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
                j += 1;
            }
            let value_start = j;
            match *bytes.get(j)? {
                quote @ (b'"' | b'\'') => {
                    let close = s[j + 1..].find(char::from(quote))?;
                    value = decode_entities(&s[j + 1..j + 1 + close]);
                    j += close + 2;
                }
                _ => {
                    let start = j;
                    while bytes.get(j).is_some_and(|b| {
                        !b.is_ascii_whitespace()
                            && !matches!(b, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
                    }) {
                        j += 1;
                    }
                    if start == j {
                        return None;
                    }
                    value = decode_entities(&s[start..j]);
                }
            }
            if attr_name == destination_name && !saw_destination {
                destination = Some(value_start..j);
            }
            i = j;
        }
        saw_destination |= attr_name == destination_name;
        attrs.push((attr_name, value));
    }
}

/// One supported HTML URL, with its complete original attribute-value token.
/// The first attribute wins, matching both safe lowering and HTML sanitizing.
pub(crate) struct UrlSpan {
    pub range: std::ops::Range<usize>,
    pub destination: String,
    pub kind: UrlKind,
}

impl UrlSpan {
    /// A quoted URL's meaningful source bytes, excluding its original quotes
    /// and surrounding literal whitespace. This lets native source edits keep
    /// line breaks and container prefixes that a Markdown HTML node omits.
    pub(crate) fn quoted_content_range(&self, html: &str) -> Option<std::ops::Range<usize>> {
        let raw = html.get(self.range.clone())?;
        let quote = *raw.as_bytes().first()?;
        if !matches!(quote, b'\'' | b'"') || raw.as_bytes().last() != Some(&quote) {
            return None;
        }
        let inner = raw.get(1..raw.len().checked_sub(1)?)?;
        let trimmed = inner.trim();
        if trimmed.is_empty() {
            return None;
        }
        let start = self.range.start + 1 + inner.len() - inner.trim_start().len();
        Some(start..start + trimmed.len())
    }
}

pub(crate) fn url_spans(html: &str) -> impl Iterator<Item = UrlSpan> + '_ {
    let mut cursor = 0;
    std::iter::from_fn(move || {
        while let Some(relative) = html[cursor..].find('<') {
            let start = cursor + relative;
            let Some((piece, len)) = markup_at(&html[start..]) else {
                cursor = start + 1;
                continue;
            };
            cursor = start + len;
            let Piece::Tag(tag, original) = piece else {
                continue;
            };
            let (attribute, kind) = match tag.name.as_str() {
                "a" if !tag.closing => ("href", UrlKind::Link),
                "img" if !tag.closing => ("src", UrlKind::Image),
                _ => continue,
            };
            if let Some(destination) = tag.attr(attribute).map(str::trim).filter(|s| !s.is_empty())
                && let Some((_, _, Some(range))) = parse_tag_with_destination(original)
            {
                return Some(UrlSpan {
                    range: start + range.start..start + range.end,
                    destination: destination.to_owned(),
                    kind,
                });
            }
        }
        None
    })
}
fn tag(name: String, closing: bool, self_closing: bool, attrs: Vec<(String, String)>) -> Tag {
    Tag {
        name,
        closing,
        self_closing,
        attrs,
    }
}

/// Decode HTML character references (named, decimal and hexadecimal).
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after
            .find(';')
            .filter(|&semi| semi > 0 && semi <= 32)
            .and_then(|semi| {
                let body = &after[..semi];
                let value = if let Some(hex) =
                    body.strip_prefix("#x").or_else(|| body.strip_prefix("#X"))
                {
                    numeric_reference(hex, 16)
                } else if let Some(dec) = body.strip_prefix('#') {
                    numeric_reference(dec, 10)
                } else {
                    super::entities::lookup(body).map(str::to_string)
                };
                value.map(|value| (value, semi + 1))
            });
        match decoded {
            Some((value, consumed)) => {
                out.push_str(&value);
                rest = &after[consumed..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn numeric_reference(digits: &str, radix: u32) -> Option<String> {
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    let value = u32::from_str_radix(digits, radix).ok()?;
    let ch = if value == 0 {
        '\u{FFFD}'
    } else {
        char::from_u32(value).unwrap_or('\u{FFFD}')
    };
    Some(ch.to_string())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlKind {
    Link,
    Image,
}
