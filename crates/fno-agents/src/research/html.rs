//! HTML-to-text for fetched pages: a port of the stdlib `html.parser`
//! tokenizer as the Python leg drove it (`convert_charrefs=True`, one
//! `feed`, never `close`), plus `html.unescape`. Text after an unterminated
//! construct is dropped, exactly as the unflushed parser buffer dropped it.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::pyfmt;

const CDATA_CONTENT_ELEMENTS: &[&str] =
    &["script", "style", "xmp", "iframe", "noembed", "noframes"];
const RCDATA_CONTENT_ELEMENTS: &[&str] = &["textarea", "title"];

/// Strip tags and script/style bodies, collapse inline whitespace, squeeze
/// blank lines, and trim.
pub fn strip_html(body: &str) -> String {
    let text = extract_text(body);
    let collapsed = collapse_inline_ws(&text);
    let squeezed = squeeze_blank_lines(&collapsed);
    pyfmt::strip(&squeezed).to_string()
}

/// `[ \t\r\f\v]+` -> one space.
pub fn collapse_inline_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    for c in s.chars() {
        if matches!(c, ' ' | '\t' | '\r' | '\x0c' | '\x0b') {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            in_run = false;
            out.push(c);
        }
    }
    out
}

fn squeeze_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for c in s.chars() {
        if c == '\n' {
            run += 1;
            continue;
        }
        push_newlines(&mut out, run);
        run = 0;
        out.push(c);
    }
    push_newlines(&mut out, run);
    out
}

fn push_newlines(out: &mut String, run: usize) {
    let keep = if run >= 3 { 2 } else { run };
    for _ in 0..keep {
        out.push('\n');
    }
}

#[derive(Default)]
struct Extractor {
    parts: String,
    skip: bool,
}

impl Extractor {
    fn start(&mut self, tag: &str) {
        if tag == "script" || tag == "style" {
            self.skip = true;
        } else {
            self.parts.push(' ');
        }
    }

    fn end(&mut self, tag: &str) {
        if tag == "script" || tag == "style" {
            self.skip = false;
        } else {
            self.parts.push(' ');
        }
    }

    fn data(&mut self, s: &str) {
        if !self.skip {
            self.parts.push_str(s);
        }
    }
}

fn is_tag_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\x0c' | ' ')
}

fn starts_with(raw: &[char], i: usize, lit: &str) -> bool {
    let mut p = i;
    for c in lit.chars() {
        if p >= raw.len() || raw[p] != c {
            return false;
        }
        p += 1;
    }
    true
}

fn find_char(raw: &[char], c: char, from: usize) -> Option<usize> {
    (from..raw.len()).find(|&p| raw[p] == c)
}

fn find_str(raw: &[char], lit: &str, from: usize) -> Option<usize> {
    (from..raw.len()).find(|&p| starts_with(raw, p, lit))
}

fn collect(raw: &[char], from: usize, to: usize) -> String {
    raw[from..to].iter().collect()
}

struct Parser {
    raw: Vec<char>,
    cdata_elem: Option<String>,
    escapable: bool,
    out: Extractor,
}

/// One `feed` of the whole body with no `close`: whatever the tokenizer
/// cannot settle stays in its buffer and never reaches the text.
fn extract_text(body: &str) -> String {
    let mut p = Parser {
        raw: body.chars().collect(),
        cdata_elem: None,
        escapable: true,
        out: Extractor::default(),
    };
    p.goahead();
    p.out.parts
}

impl Parser {
    fn goahead(&mut self) {
        let n = self.raw.len();
        let mut i = 0usize;
        while i < n {
            let j = if self.cdata_elem.is_none() {
                match find_char(&self.raw, '<', i) {
                    Some(p) => p,
                    None => {
                        // A trailing `&...` with no terminator may be half a
                        // charref, so the parser waits for more text.
                        let lo = i.max(n.saturating_sub(34));
                        let amp = (lo..n).rev().find(|&p| self.raw[p] == '&');
                        if let Some(amp) = amp {
                            let terminated =
                                self.raw[amp..].iter().any(|&c| is_tag_ws(c) || c == ';');
                            if !terminated {
                                break;
                            }
                        }
                        n
                    }
                }
            } else {
                match self.cdata_end(i) {
                    Some(p) => p,
                    None => break,
                }
            };
            if i < j {
                let chunk = collect(&self.raw, i, j);
                if self.escapable {
                    self.out.data(&unescape(&chunk));
                } else {
                    self.out.data(&chunk);
                }
            }
            i = j;
            if i == n {
                break;
            }
            let k = if i + 1 < n && self.raw[i + 1].is_ascii_alphabetic() {
                self.parse_starttag(i)
            } else if starts_with(&self.raw, i, "</") {
                self.parse_endtag(i)
            } else if starts_with(&self.raw, i, "<!--") {
                self.parse_comment(i)
            } else if starts_with(&self.raw, i, "<?") {
                find_char(&self.raw, '>', i + 2).map(|p| p + 1)
            } else if starts_with(&self.raw, i, "<!") {
                self.parse_html_declaration(i)
            } else if i + 1 < n {
                self.out.data("<");
                Some(i + 1)
            } else {
                break;
            };
            match k {
                Some(k) => i = k,
                None => break,
            }
        }
    }

    /// The `</elem` (ASCII case-insensitive) that ends raw/rc text, when
    /// followed by a tag-name terminator.
    fn cdata_end(&self, from: usize) -> Option<usize> {
        let elem = self.cdata_elem.as_deref()?;
        let n = self.raw.len();
        if elem == "plaintext" {
            return Some(n);
        }
        let name: Vec<char> = elem.chars().collect();
        (from..n).find(|&p| {
            if !starts_with(&self.raw, p, "</") {
                return false;
            }
            let at = p + 2;
            if at + name.len() >= n {
                return false;
            }
            let same = name
                .iter()
                .zip(&self.raw[at..at + name.len()])
                .all(|(a, b)| a.eq_ignore_ascii_case(b));
            let next = self.raw[at + name.len()];
            same && (is_tag_ws(next) || next == '/' || next == '>')
        })
    }

    fn parse_starttag(&mut self, i: usize) -> Option<usize> {
        let endpos = locate_tag_end(&self.raw, i + 1);
        if self.raw[endpos - 1] != '>' {
            return None;
        }
        let (name_end, mut k) = tagfind(&self.raw, i + 1);
        let tag = collect(&self.raw, i + 1, name_end).to_lowercase();
        while k < endpos {
            match attrfind(&self.raw, k) {
                Some(e) => k = e,
                None => break,
            }
        }
        let rest = if k < endpos {
            collect(&self.raw, k, endpos)
        } else {
            String::new()
        };
        let end = pyfmt::strip(&rest);
        if end != ">" && end != "/>" {
            let text = collect(&self.raw, i, endpos);
            self.out.data(&text);
            return Some(endpos);
        }
        if end.ends_with("/>") {
            self.out.start(&tag);
            self.out.end(&tag);
        } else {
            self.out.start(&tag);
            if CDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) || tag == "plaintext" {
                self.cdata_elem = Some(tag);
                self.escapable = false;
            } else if RCDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) {
                self.cdata_elem = Some(tag);
                self.escapable = true;
            }
        }
        Some(endpos)
    }

    fn parse_endtag(&mut self, i: usize) -> Option<usize> {
        find_char(&self.raw, '>', i + 2)?;
        let letter = self.raw.get(i + 2).is_some_and(|c| c.is_ascii_alphabetic());
        if !letter {
            if self.raw.get(i + 2) == Some(&'>') {
                return Some(i + 3);
            }
            return find_char(&self.raw, '>', i + 2).map(|p| p + 1);
        }
        let j = locate_tag_end(&self.raw, i + 2);
        if self.raw[j - 1] != '>' {
            return None;
        }
        let (name_end, _) = tagfind(&self.raw, i + 2);
        let tag = collect(&self.raw, i + 2, name_end).to_lowercase();
        self.out.end(&tag);
        self.cdata_elem = None;
        self.escapable = true;
        Some(j)
    }

    fn parse_comment(&self, i: usize) -> Option<usize> {
        let n = self.raw.len();
        for p in (i + 4)..n {
            if starts_with(&self.raw, p, "-->") {
                return Some(p + 3);
            }
            if starts_with(&self.raw, p, "--!>") {
                return Some(p + 4);
            }
        }
        if starts_with(&self.raw, i + 4, ">") {
            return Some(i + 5);
        }
        if starts_with(&self.raw, i + 4, "->") {
            return Some(i + 6);
        }
        None
    }

    fn parse_html_declaration(&self, i: usize) -> Option<usize> {
        if starts_with(&self.raw, i, "<![CDATA[") {
            return find_str(&self.raw, "]]>", i + 9).map(|j| j + 3);
        }
        let head: String = self.raw.iter().skip(i).take(9).collect();
        if head.to_lowercase() == "<!doctype" {
            return find_char(&self.raw, '>', i + 9).map(|p| p + 1);
        }
        if starts_with(&self.raw, i, "<![") {
            return find_char(&self.raw, '>', i + 3).map(|p| p + 1);
        }
        find_char(&self.raw, '>', i + 2).map(|p| p + 1)
    }
}

/// `[ws]*=[ws]*` then a quoted or bare value, resolved in the regex
/// engine's backtracking order. `None` when no value group matches at `q`.
fn match_value(raw: &[char], q: usize) -> Option<usize> {
    let n = raw.len();
    let mut p = q;
    while p < n && is_tag_ws(raw[p]) {
        p += 1;
    }
    if p >= n || raw[p] != '=' {
        return None;
    }
    let after_eq = p + 1;
    let mut v = after_eq;
    while v < n && is_tag_ws(raw[v]) {
        v += 1;
    }
    if let Some(end) = value_at(raw, v) {
        return Some(end);
    }
    // Giving back one whitespace lets the bare form match empty there.
    if v > after_eq {
        return Some(v - 1);
    }
    None
}

fn value_at(raw: &[char], v: usize) -> Option<usize> {
    let n = raw.len();
    match raw.get(v) {
        Some(&q) if q == '\'' || q == '"' => find_char(raw, q, v + 1).map(|c| c + 1),
        _ => {
            let mut e = v;
            while e < n && !(is_tag_ws(raw[e]) || raw[e] == '>') {
                e += 1;
            }
            Some(e)
        }
    }
}

fn is_name_stop(c: char) -> bool {
    is_tag_ws(c) || c == '/' || c == '>'
}

fn is_attr_rest_stop(c: char) -> bool {
    is_name_stop(c) || c == '='
}

fn lookbehind_ok(raw: &[char], p: usize) -> bool {
    p > 0 && (matches!(raw[p - 1], '\'' | '"' | '/') || is_tag_ws(raw[p - 1]))
}

/// `locatetagend` matched at `start` (the first tag-name letter).
fn locate_tag_end(raw: &[char], start: usize) -> usize {
    let n = raw.len();
    let mut p = start + 1;
    while p < n && !is_name_stop(raw[p]) {
        p += 1;
    }
    while p < n && (is_tag_ws(raw[p]) || raw[p] == '/') {
        p += 1;
    }
    while p < n && lookbehind_ok(raw, p) && !is_name_stop(raw[p]) {
        let mut q = p + 1;
        while q < n && !is_attr_rest_stop(raw[q]) {
            q += 1;
        }
        if let Some(v) = match_value(raw, q) {
            q = v;
        }
        while q < n && (is_tag_ws(raw[q]) || raw[q] == '/') {
            q += 1;
        }
        p = q;
    }
    if p < n && raw[p] == '>' {
        p += 1;
    }
    p
}

/// `(?:[ws]|/(?!>))*` from `p`.
fn skip_tag_trailer(raw: &[char], mut p: usize) -> usize {
    let n = raw.len();
    while p < n {
        if is_tag_ws(raw[p]) || (raw[p] == '/' && raw.get(p + 1) != Some(&'>')) {
            p += 1;
        } else {
            break;
        }
    }
    p
}

/// `tagfind_tolerant` at `start`: (end of the name, end of the match).
fn tagfind(raw: &[char], start: usize) -> (usize, usize) {
    let n = raw.len();
    let mut p = start + 1;
    while p < n && !is_name_stop(raw[p]) {
        p += 1;
    }
    (p, skip_tag_trailer(raw, p))
}

/// `attrfind_tolerant` at `k`: the end of one attribute, or `None`.
fn attrfind(raw: &[char], k: usize) -> Option<usize> {
    let n = raw.len();
    if k >= n || !lookbehind_ok(raw, k) || is_name_stop(raw[k]) {
        return None;
    }
    let mut q = k + 1;
    while q < n && !is_attr_rest_stop(raw[q]) {
        q += 1;
    }
    if let Some(v) = match_value(raw, q) {
        q = v;
    }
    Some(skip_tag_trailer(raw, q))
}

fn entities() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for line in include_str!("html5_entities.tsv").lines() {
            let Some((name, points)) = line.split_once('\t') else {
                continue;
            };
            let value: String = points
                .split(' ')
                .filter_map(|h| u32::from_str_radix(h, 16).ok())
                .filter_map(char::from_u32)
                .collect();
            map.insert(name.to_string(), value);
        }
        map
    })
}

fn invalid_charref(num: u64) -> Option<char> {
    let c = match num {
        0x00 => '\u{fffd}',
        0x0d => '\r',
        0x80 => '\u{20ac}',
        0x81 => '\u{81}',
        0x82 => '\u{201a}',
        0x83 => '\u{0192}',
        0x84 => '\u{201e}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02c6}',
        0x89 => '\u{2030}',
        0x8a => '\u{0160}',
        0x8b => '\u{2039}',
        0x8c => '\u{0152}',
        0x8d => '\u{8d}',
        0x8e => '\u{017d}',
        0x8f => '\u{8f}',
        0x90 => '\u{90}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201c}',
        0x94 => '\u{201d}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02dc}',
        0x99 => '\u{2122}',
        0x9a => '\u{0161}',
        0x9b => '\u{203a}',
        0x9c => '\u{0153}',
        0x9d => '\u{9d}',
        0x9e => '\u{017e}',
        0x9f => '\u{0178}',
        _ => return None,
    };
    Some(c)
}

fn invalid_codepoint(num: u64) -> bool {
    (0x1..=0x8).contains(&num)
        || (0xe..=0x1f).contains(&num)
        || (0x7f..=0x9f).contains(&num)
        || (0xfdd0..=0xfdef).contains(&num)
        || num == 0xb
        || (num <= 0x10ffff && (num & 0xfffe) == 0xfffe)
}

fn numeric_charref(num: u64) -> String {
    if let Some(c) = invalid_charref(num) {
        return c.to_string();
    }
    if (0xd800..=0xdfff).contains(&num) || num > 0x10ffff {
        return '\u{fffd}'.to_string();
    }
    if invalid_codepoint(num) {
        return String::new();
    }
    u32::try_from(num)
        .ok()
        .and_then(char::from_u32)
        .map(|c| c.to_string())
        .unwrap_or_else(|| '\u{fffd}'.to_string())
}

fn named_charref(name: &[char]) -> String {
    let table = entities();
    let whole: String = name.iter().collect();
    if let Some(v) = table.get(&whole) {
        return v.clone();
    }
    // The longest known prefix wins, as the standard defines it.
    for x in (2..name.len()).rev() {
        let prefix: String = name[..x].iter().collect();
        if let Some(v) = table.get(&prefix) {
            let tail: String = name[x..].iter().collect();
            return format!("{v}{tail}");
        }
    }
    format!("&{whole}")
}

fn digits_value(digits: &[char], radix: u32) -> u64 {
    let mut num: u64 = 0;
    for c in digits {
        let d = c.to_digit(radix).unwrap_or(0) as u64;
        num = num.saturating_mul(radix as u64).saturating_add(d);
    }
    num
}

/// `html.unescape`: numeric and named character references per HTML5.
pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    let mut out = String::with_capacity(s.len());
    let mut p = 0usize;
    while p < n {
        if c[p] != '&' {
            out.push(c[p]);
            p += 1;
            continue;
        }
        if c.get(p + 1) == Some(&'#') {
            let hex_follows = c.get(p + 3).is_some_and(|h| h.is_ascii_hexdigit());
            let (start, radix) = match c.get(p + 2).copied() {
                Some(d) if d.is_ascii_digit() => (p + 2, 10),
                Some('x') | Some('X') if hex_follows => (p + 3, 16),
                _ => {
                    out.push('&');
                    p += 1;
                    continue;
                }
            };
            let mut q = start;
            while q < n && c[q].is_digit(radix) && c[q].is_ascii() {
                q += 1;
            }
            let num = digits_value(&c[start..q], radix);
            if q < n && c[q] == ';' {
                q += 1;
            }
            out.push_str(&numeric_charref(num));
            p = q;
            continue;
        }
        let mut q = p + 1;
        while q < n
            && q - (p + 1) < 32
            && !matches!(c[q], '\t' | '\n' | '\x0c' | ' ' | '<' | '&' | '#' | ';')
        {
            q += 1;
        }
        if q == p + 1 {
            out.push('&');
            p += 1;
            continue;
        }
        if q < n && c[q] == ';' {
            q += 1;
        }
        out.push_str(&named_charref(&c[p + 1..q]));
        p = q;
    }
    out
}
