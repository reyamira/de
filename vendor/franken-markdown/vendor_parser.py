#!/usr/bin/env python3
"""Reproduce the source-aware parser from the pinned crates.io parser.

Usage: python vendor_parser.py /path/to/franken_markdown-0.7.0/src
The generated Rust is shipped in the package; this is not a build dependency.
"""
from pathlib import Path
import hashlib
import sys
import re
from pygments.lexers import RustLexer
from pygments.token import Punctuation


def item_end(source, start):
    depth = 0
    started = False
    attributes = 0
    for offset, token, value in RustLexer().get_tokens_unprocessed(source[start:]):
        if token not in Punctuation:
            continue
        for index, character in enumerate(value):
            if character == '[':
                attributes += 1
            elif character == ']':
                attributes -= 1
            elif not attributes:
                if character == '{':
                    started = True
                    depth += 1
                elif character == '}':
                    depth -= 1
                    if started and depth == 0:
                        end = start + offset + index + 1
                        following = end
                        while following < len(source) and source[following].isspace():
                            following += 1
                        # Brace-group imports and macro invocations also own
                        # their terminating semicolon.
                        return following + 1 if source[following:following+1] == ';' else end
                elif character == ';' and not started:
                    return start + offset + index + 1
    raise ValueError('Unterminated Rust item')

def item_start(source, start):
    # Documentation and outer attributes belong to the removed item.
    while start:
        previous = source.rfind('\n', 0, start-1) + 1
        line = source[previous:start].strip()
        if line.startswith('#[') or line.startswith('///'):
            start = previous
        else:
            break
    return start


def remove_tests(source):
    matches = list(re.finditer(r'(?m)^[ \t]*#\[cfg\(test\)\]', source))
    cuts = []
    end = 0
    for match in matches:
        if match.start() < end:
            continue
        end = item_end(source, match.start())
        cuts.append((item_start(source, match.start()), end))
    for start, end in reversed(cuts):
        source = source[:start] + source[end:]
    return source

def remove_cli(source):
    matches = list(re.finditer(r'(?m)^[ \t]*#\[cfg\(feature = "cli"\)\]', source))
    for match in reversed(matches):
        end = item_end(source, match.start())
        source = source[:item_start(source, match.start())] + source[end:]
    return source


def remove_items(source, names):
    for name in names:
        match = re.search(r'(?m)^(?:pub(?:\([^)]*\))? )?(?:struct|fn) '+name+r'\b', source)
        if not match:
            continue
        start = item_start(source, match.start())
        end = item_end(source, match.start())
        source = source[:start] + source[end:]
    return source


def generate(root):
    root = Path(root)
    names = ['mod.rs', 'destinations.rs', 'entities.rs', 'frontmatter.rs', 'html_blocks.rs', 'reference_labels.rs', 'unicode_casefold.rs', 'unicode_punct.rs']
    files = {name: (root / 'parse' / name).read_text() for name in names}
    # This pins the grammar as well as the crate version. Refuse a registry edit.
    assert hashlib.sha256(files['mod.rs'].encode()).hexdigest() == '41a66c0e1b6d6f49a88d013c6973fa934851d4096336c9b6dd3658e7824ef665'
    files['safe_html.rs'] = (root / 'safe_html.rs').read_text()
    files['footnotes.rs'] = (root / 'footnotes.rs').read_text()
    files['toc.rs'] = (root / 'toc.rs').read_text()
    source_names = ['parse/'+name for name in names] + ['safe_html.rs', 'footnotes.rs', 'toc.rs']
    digest = hashlib.sha256(b''.join(name.encode()+b'\0'+(root/name).read_bytes() for name in source_names)).hexdigest()
    assert digest == '8c7ab55460e055c65e49ef595ec4d262d644c52ae2a78a62105598411eddf969'
    def subset(name, ranges):
        lines = files[name].splitlines(keepends=True)
        files[name] = ''.join(''.join(lines[start-1:end]) for start, end in ranges)
    # Only the URL tokenizer is private and needed by the parser's tracker.
    subset('safe_html.rs', [(146, 170), (201, 383), (509, 570), (1340, 1344)])
    # Native assets already made raw HTML inert. Keep the exact endnote pass,
    # not HTML lowering, page-note layout, heading metadata, or their tests.
    subset('footnotes.rs', [(1, 11), (26, 28), (50, 127), (194, 425)])
    subset('toc.rs', [(1, 25)])
    for name in files:
        files[name] = files[name].replace('crate::', 'franken_markdown::').replace('use crate::{', 'use franken_markdown::{')
    s = files['mod.rs']
    s = s.replace('use std::collections::{BTreeMap, HashMap};', '#![allow(dead_code, unexpected_cfgs, unused_imports)]\nuse std::collections::{BTreeMap, HashMap};\nmod provenance;\nmod safe_html;\nmod footnotes;\nmod toc;\npub(crate) use provenance::{Provenance, parse as parse_sourced, prepare_endnotes, with_assets, image_fallback};')
    s = s.replace('pub(crate) use destinations::container_fence_ranges;', '')
    def change(old, new):
        nonlocal s
        assert old in s, old
        s = s.replace(old, new)
    change('if !scan.needs_full_parse {', 'if !scan.needs_full_parse && profiler.destinations.is_none() {')
    change('vec![Inline::Text(text.to_string())]', '{ let output = text.to_string(); provenance::register(&output, provenance::text_origins(text, profiler)); vec![Inline::Text(output)] }')
    change('vec![Inline::Text(chars.iter().collect())]', '{ let output: String = chars.iter().collect(); provenance::register(&output, provenance::char_origins(&output, chars, profiler)); vec![Inline::Text(output)] }')
    change('let mut buf = String::new();', 'let mut buf = provenance::Buffer::default();')
    change('let flush = |buf: &mut String, els: &mut Vec<InlineEl>| {', 'let flush = |buf: &mut provenance::Buffer, els: &mut Vec<InlineEl>| {')
    change('InlineEl::Text(std::mem::take(buf))', 'InlineEl::Text(std::mem::take(buf).finish())')
    change('        let c = bytes[i];\n        match c {', '        let c = bytes[i];\n        let previous = buf.len();\n        let consumed_start = i;\n        match c {')
    change('        }\n    }\n    profiler.inline_work.depth -= 1;', '        }\n        buf.record(previous, bytes, consumed_start..i, profiler);\n    }\n    profiler.inline_work.depth -= 1;')
    # Register exact normalization before the parser moves opaque text atoms.
    change('                    els.push(InlineEl::Node(Inline::Code(code)));', '                    provenance::register(&code, code_origins(&code, &bytes[i + n..end], profiler));\n                    els.push(InlineEl::Node(Inline::Code(code)));')
    change('                    els.push(InlineEl::Node(Inline::Html(html)));', '                    provenance::register(&html, provenance::char_origins(&html, &bytes[i..next], profiler));\n                    els.push(InlineEl::Node(Inline::Html(html)));')
    change('                    els.push(InlineEl::Node(Inline::DisplayMath(inner)));', '                    provenance::register(&inner, provenance::char_origins(&inner, &bytes[i + 2..next - 2], profiler));\n                    els.push(InlineEl::Node(Inline::DisplayMath(inner)));')
    change('                    els.push(InlineEl::Node(Inline::Math(inner)));', '                    provenance::register(&inner, provenance::char_origins(&inner, &bytes[i + 1..next - 1], profiler));\n                    els.push(InlineEl::Node(Inline::Math(inner)));')
    change('                    Inline::SoftBreak\n', '                    provenance::soft_break(bytes, i, profiler)\n')
    change('                out.push_str(t)\n', '                provenance::append(out, t)\n')
    change('Inline::Image { alt, .. } => out.push_str(alt),', 'Inline::Image { alt, .. } => provenance::append(out, alt),')
    change('Inline::Html(h) => out.push_str(h),', 'Inline::Html(h) => provenance::append(out, h),')
    change("Inline::SoftBreak | Inline::HardBreak => out.push(' '),", "Inline::SoftBreak | Inline::HardBreak => provenance::append(out, \" \"),")
    change('Inline::FootnoteRef { id } => out.push_str(&format!("[^{id}]")),', 'Inline::FootnoteRef { id } => provenance::append(out, &provenance::visible_footnote(id)),')
    change('                        els.push(InlineEl::Node(Inline::FootnoteRef { id }));', '                        provenance::register(&id, provenance::char_origins(&id, &bytes[i+2..next-1], profiler));\n                        els.push(InlineEl::Node(Inline::FootnoteRef { id }));')
    change('                        content: vec![Inline::Text(label)],', '                        content: {\n                            let start = if bytes[i] == \'<\' { i + 1 } else { i };\n                            provenance::register(&label, provenance::char_origins(&label, &bytes[start..start + label.chars().count()], profiler));\n                            vec![Inline::Text(label)]\n                        },')
    change('                        orig: n,', '                        orig: n,\n                        origins: provenance::origins_for_chars(&bytes[i..i+n], profiler),')
    change('        orig: usize,\n        can_open:', '        orig: usize,\n        origins: Vec<provenance::Origin>,\n        can_open:')
    change('InlineEl::Delim { ch, count, .. } => {', 'InlineEl::Delim { ch, count, origins, .. } => {')
    change('            push_inline_text(out, &s);', '            provenance::register(&s, origins);\n            push_inline_text(out, &s);')
    change('        t.push_str(s);', '        provenance::append(t, s);')
    change('        out.push(Inline::Text(s.to_string()));', '        out.push(Inline::Text(provenance::copy(s)));')
    change('                t.push_str(&s);', '                provenance::append(t, &s);')
    change('if let InlineEl::Delim { count, .. } = &mut els[o] {\n            *count -= use_delims;', 'if let InlineEl::Delim { count, origins, .. } = &mut els[o] {\n            *count -= use_delims;\n            origins.retain(|origin| origin.bytes.start < *count);')
    change('if let InlineEl::Delim { count, .. } = &mut els[c] {\n            *count -= use_delims;', 'if let InlineEl::Delim { count, origins, .. } = &mut els[c] {\n            *count -= use_delims;\n            origins.retain_mut(|origin| { if origin.bytes.end <= use_delims { return false; } origin.bytes.start -= use_delims; origin.bytes.end -= use_delims; true });')
    change(r"code.trim_end_matches('\n').to_string()", r"provenance::copy(code.trim_end_matches('\n'))")
    # Code block text is assembled at the actual grammar's indentation sites.
    change('let mut code = String::with_capacity(body_bytes);', 'let mut code = String::with_capacity(body_bytes);\n            let mut code_parts = Vec::new();')
    change('            let mut code = match start {', '            let mut math_parts = Vec::new();\n            let mut code = match start {')
    change('MathBlockStart::SingleLine(content) => content.to_owned(),', 'MathBlockStart::SingleLine(content) => { let text = content.to_owned(); provenance::register(&text, provenance::text_origins(content, profiler)); text },')
    change('                        code.push_str(initial);', '                        math_parts.push((code.len(), initial));\n                        code.push_str(initial);')
    change('                                code.push_str(inner);', '                                math_parts.push((code.len(), inner));\n                                code.push_str(inner);')
    change('                            code.push_str(line);', '                            math_parts.push((code.len(), *line));\n                            code.push_str(line);')
    change('            if multiline {\n                code.truncate', '            if multiline {\n                register_parts(&code, &math_parts, profiler);\n                code.truncate')
    change('                code.push_str(strip_fence_indent(lines[i], fence_indent));', '                let part = strip_fence_indent(lines[i], fence_indent);\n                code_parts.push((code.len(), part));\n                code.push_str(part);')
    change('            if lang.as_deref() == Some("math") {', '            register_parts(&code, &code_parts, profiler);\n            if lang.as_deref() == Some("math") {')
    change('let (code, used) = parse_indented_code(&lines[i..]);', 'let (code, used) = parse_indented_code(&lines[i..], profiler);')
    change('fn parse_indented_code(lines: &[&str]) -> (String, usize) {', 'fn parse_indented_code(lines: &[&str], profiler: &ParseProfiler) -> (String, usize) {')
    change('        code.push_str(strip_n(line, 4));', '        let part = strip_n(line, 4);\n        code_parts.push((code.len(), part));\n        code.push_str(part);')
    change('    (code, used)\n}', '    register_parts(&code, &code_parts, profiler);\n    (code, used)\n}')
    change('            let html = lines[start..i].join("\\n");', '            let html = lines[start..i].join("\\n");\n            let mut offset = 0;\n            let parts: Vec<_> = lines[start..i].iter().map(|line| { let part = (offset, *line); offset += line.len()+1; part }).collect();\n            register_parts(&html, &parts, profiler);')
    # Upstream private helper tests need the additional tracing argument.
    s = s.replace('parse_indented_code(&lines)', 'parse_indented_code(&lines, &ParseProfiler::disabled())')
    files['mod.rs'] = s + '\n' + HELPERS
    d = files['destinations.rs']
    d = d.replace('    fn new(source: &str)', '    pub(super) fn new(source: &str)').replace('    fn text_range(', '    pub(super) fn text_range(').replace('    fn char_range(', '    pub(super) fn char_range(')
    d = d.replace('franken_markdown::safe_html::', 'super::safe_html::')
    d = d.replace('use super::*;', 'use super::*;\n#[path = "source_prefix.rs"]\nmod source_prefix;', 1)
    d = d.replace('        let start = run.original.start + offset - run.copied.start;', '        if run.original.len() != run.copied.len() { return Some(run.original.clone()); }\n        let start = run.original.start + offset - run.copied.start;')
    d = d.replace('            && last.copied.end == copied.start\n            && last.original.end == original.start', '            && last.copied.end == copied.start\n            && last.original.end == original.start\n            && last.copied.len() == last.original.len()\n            && copied.len() == original.len()')
    d = d.replace('            self.map_parts(output, &[]);\n        }\n    }', '            self.map_parts(output, &[]);\n        }\n        self.map_generated_prefix(output, original, common);\n    }')
    d = d.replace('if token.is_empty() || dest.is_empty() {', 'if token.is_empty() || dest.is_empty() || (!self.html && self.labels.is_none()) {')
    d = d.replace('    source_len: usize,', '    source_len: usize,\n    line_endings: Vec<Range<usize>>,')
    d = d.replace('            source_len: source.len(),', '            source_len: source.len(),\n            line_endings: { let mut offset = 0; logical_lines_inclusive(source).filter_map(|line| { let start = offset + logical_line_content(line).len(); offset += line.len(); (start < offset).then_some(start..offset) }).collect() },')
    d = d.replace('                map.push(1, None);', '                let ending = self.line_ending(lines[index - 1]);\n                map.push(ending.as_ref().map_or(1, Range::len), ending);')
    d = d.replace('    fn append_chars(', '    pub(super) fn line_ending(&self, text: &str) -> Option<Range<usize>> {\n        let width = text.chars().next_back()?.len_utf8();\n        let end = self.text_range(&text[text.len()-width..])?.end;\n        self.line_endings.get(self.line_endings.partition_point(|range| range.start < end)).filter(|range| range.start == end).cloned()\n    }\n\n    fn append_chars(')
    files['destinations.rs'] = d
    files['safe_html.rs'] = files['safe_html.rs'].replace('franken_markdown::parse::entities::', 'super::entities::')
    f = files['footnotes.rs'].replace('franken_markdown::toc::', 'super::toc::')
    f = f.replace('fn endnotes_for_pdf(', 'pub(super) fn endnotes_for_pdf(')
    f = f.replace('let mut list = list.clone();', 'let mut list = super::provenance::clone_list(list);')
    f = f.replace('let mut table = table.clone();', 'let mut table = super::provenance::clone_table(table);')
    f = f.replace('let mut items = items.clone();', 'let mut items = super::provenance::clone_definitions(items);')
    f = f.replace('            other => other.clone(),', '            other => super::provenance::clone_block(other),', 1)
    f = f.replace('            other => other.clone(),', '            other => super::provenance::clone_inline(other),', 1)
    f = f.replace('Inline::Text("Notes".to_string())', 'Inline::Text(super::provenance::synthetic("Notes".to_string()))')
    f = f.replace('Inline::Text(format!("{label} "))', 'Inline::Text(super::provenance::synthetic(format!("{label} ")))')
    f = f.replace('Inline::Text(format!("[^{id}]"))', 'Inline::Text(super::provenance::visible_footnote(id))')
    f = f.replace('    number\n        .to_string()', '    super::provenance::synthetic(number\n        .to_string()')
    f = f.replace(".map(|digit| DIGITS[usize::from(digit - b'0')])\n        .collect()", ".map(|digit| DIGITS[usize::from(digit - b'0')])\n        .collect())")
    files['footnotes.rs'] = f
    runtime = {name: remove_cli(remove_tests(source)) for name, source in files.items()}
    runtime['mod.rs'] = remove_items(runtime['mod.rs'], [
        'destination_spans', 'reference_label_spans', 'ParseProfile', 'SpannedParseProfile',
        'parse_document_profiled', 'parse_document_spanned_profiled',
    ])
    runtime['destinations.rs'] = remove_items(runtime['destinations.rs'], [
        'destination_spans', 'reference_label_spans', 'resource_destination_spans',
        'tracked_destinations', 'container_fence_ranges',
    ])
    return runtime


HELPERS = r'''
fn register_parts(output: &str, parts: &[(usize, &str)], profiler: &ParseProfiler) {
    let mut origins = Vec::new();
    for (offset, part) in parts {
        origins.extend(provenance::text_origins(part, profiler).into_iter().map(|mut origin| {
            origin.bytes.start += offset;
            origin.bytes.end += offset;
            origin
        }));
        let newline = offset + part.len();
        if newline < output.len()
            && output.as_bytes()[newline] == b'\n'
            && let Some(source) = profiler.destinations.as_ref().and_then(|tracker| tracker.line_ending(part))
        {
            origins.push(provenance::Origin { bytes: newline..newline+1, source });
        }
    }
    provenance::register(output, origins);
}

fn code_origins(output: &str, chars: &[char], profiler: &ParseProfiler) -> Vec<provenance::Origin> {
    let strip = chars.len() >= 2 && matches!(chars[0], ' ' | '\n')
        && matches!(chars[chars.len()-1], ' ' | '\n') && chars.iter().any(|c| !c.is_whitespace());
    let body = if strip { &chars[1..chars.len()-1] } else { chars };
    let mut origins = Vec::new();
    let mut offset = 0;
    let mut index = 0;
    while index < body.len() {
        let escaped_pipe = profiler.table_cell_code_escapes && body[index] == '\\' && body.get(index+1) == Some(&'|');
        let count = if escaped_pipe { 2 } else { 1 };
        let width = if escaped_pipe || body[index] == '\n' { 1 } else { body[index].len_utf8() };
        if let Some(tracker) = &profiler.destinations {
            for scalar in index..index+count {
                if let Some(source) = tracker.char_range(body, scalar..scalar+1) {
                    origins.push(provenance::Origin { bytes: offset..offset+width, source });
                }
            }
        }
        offset += width;
        index += count;
    }
    debug_assert_eq!(offset, output.len());
    origins
}
'''

if __name__ == '__main__':
    for name, source in generate(sys.argv[1]).items():
        (Path(__file__).parent / 'src' / 'parse' / name).write_text(source)
