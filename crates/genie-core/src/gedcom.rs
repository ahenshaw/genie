//! Lossless-ish GEDCOM reading and writing.
//!
//! The file is parsed into a generic tree of [`Node`]s so that every tag we
//! do not understand survives a load/save round trip untouched. `CONC`/`CONT`
//! continuation lines are folded into their parent's value on load (with
//! `\n` for `CONT`) and re-split on save.

use std::fmt::Write as _;

/// One GEDCOM line plus its subordinate lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub xref: Option<String>,
    pub tag: String,
    pub value: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(tag: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            xref: None,
            tag: tag.into(),
            value: value.into(),
            children: Vec::new(),
        }
    }

    pub fn record(xref: impl Into<String>, tag: impl Into<String>) -> Self {
        Self {
            xref: Some(xref.into()),
            tag: tag.into(),
            value: String::new(),
            children: Vec::new(),
        }
    }

    pub fn with_child(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    pub fn child(&self, tag: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.tag == tag)
    }

    pub fn children_with<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.tag == tag)
    }

    /// Value of the first child with `tag`, or `""`.
    pub fn child_value(&self, tag: &str) -> &str {
        self.child(tag).map(|c| c.value.as_str()).unwrap_or("")
    }

    /// Returns the child with `tag`, creating it if absent.
    pub fn ensure_child(&mut self, tag: &str) -> &mut Node {
        let idx = match self.children.iter().position(|c| c.tag == tag) {
            Some(i) => i,
            None => {
                self.children.push(Node::new(tag, ""));
                self.children.len() - 1
            }
        };
        &mut self.children[idx]
    }

    /// Sets (or clears, when `value` is empty) a simple `TAG value` child.
    pub fn set_child_value(&mut self, tag: &str, value: &str) {
        if value.trim().is_empty() {
            if let Some(i) = self.children.iter().position(|c| c.tag == tag) {
                if self.children[i].children.is_empty() {
                    self.children.remove(i);
                } else {
                    self.children[i].value.clear();
                }
            }
        } else {
            // Leave an equivalent value untouched, spacing and all.
            let child = self.ensure_child(tag);
            if child.value.trim() != value.trim() {
                child.value = value.trim().to_string();
            }
        }
    }

    /// If the value is a pointer (`@X1@`), returns the bare xref.
    pub fn pointer(&self) -> Option<&str> {
        as_pointer(&self.value)
    }
}

pub fn as_pointer(v: &str) -> Option<&str> {
    let v = v.trim();
    if v.len() > 2 && v.starts_with('@') && v.ends_with('@') && !v[1..v.len() - 1].contains('@') {
        Some(&v[1..v.len() - 1])
    } else {
        None
    }
}

pub fn pointer_to(xref: &str) -> String {
    format!("@{xref}@")
}

/// Result of decoding raw file bytes into text.
pub struct Decoded {
    pub text: String,
    /// Set when the file was not valid UTF-8/UTF-16 and a fallback was used.
    pub lossy_encoding: Option<&'static str>,
}

pub fn decode(bytes: &[u8]) -> Decoded {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Decoded {
            text: String::from_utf8_lossy(rest).into_owned(),
            lossy_encoding: None,
        };
    }
    let utf16 = |le: bool, data: &[u8]| {
        let units: Vec<u16> = data
            .as_chunks::<2>().0.iter()
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Decoded { text: utf16(true, rest), lossy_encoding: None };
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Decoded { text: utf16(false, rest), lossy_encoding: None };
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Decoded { text: s.to_string(), lossy_encoding: None },
        Err(_) => {
            // ANSEL / ANSI / Latin-1: decode as Latin-1 so nothing is lost
            // byte-wise for the common Western-European case.
            let text = bytes.iter().map(|&b| b as char).collect();
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_uppercase();
            let enc = if head.contains("CHAR ANSEL") { "ANSEL" } else { "Latin-1" };
            Decoded { text, lossy_encoding: Some(enc) }
        }
    }
}

#[derive(Debug)]
pub struct ParseWarning {
    pub line: usize,
    pub message: String,
}

/// Parses GEDCOM text into its top-level records.
pub fn parse(text: &str) -> (Vec<Node>, Vec<ParseWarning>) {
    let mut warnings = Vec::new();
    let mut roots: Vec<Node> = Vec::new();
    // Stack of (level, node) currently open.
    let mut stack: Vec<(usize, Node)> = Vec::new();

    fn close_to(stack: &mut Vec<(usize, Node)>, roots: &mut Vec<Node>, level: usize) {
        while let Some((l, _)) = stack.last() {
            if *l < level {
                break;
            }
            let (_, node) = stack.pop().unwrap();
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(node),
                None => roots.push(fold_continuations(node)),
            }
        }
    }

    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_start_matches('\u{feff}').trim_start();
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let (level_str, rest) = split_word(line);
        let Ok(mut level) = level_str.parse::<usize>() else {
            warnings.push(ParseWarning { line: i + 1, message: format!("Malformed line: {line}") });
            continue;
        };
        let (mut first, mut rest) = split_word(rest);
        let mut xref = None;
        if first.starts_with('@') && first.ends_with('@') && first.len() > 2 {
            xref = Some(first[1..first.len() - 1].to_string());
            (first, rest) = split_word(rest);
        }
        let tag = first.to_string();
        if tag.is_empty() {
            warnings.push(ParseWarning { line: i + 1, message: "Missing tag".into() });
            continue;
        }
        let value = rest.to_string();

        let depth = stack.last().map(|(l, _)| *l + 1).unwrap_or(0);
        if level > depth {
            warnings.push(ParseWarning {
                line: i + 1,
                message: format!("Level jumps from {} to {level}", depth.saturating_sub(1)),
            });
            level = depth;
        }
        close_to(&mut stack, &mut roots, level);
        stack.push((level, Node { xref, tag, value, children: Vec::new() }));
    }
    close_to(&mut stack, &mut roots, 0);
    (roots, warnings)
}

fn split_word(s: &str) -> (&str, &str) {
    // GEDCOM delimiter is a single space; values keep inner/trailing spaces.
    let s = s.trim_start_matches(' ');
    match s.find(' ') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    }
}

fn fold_continuations(mut node: Node) -> Node {
    let mut kept = Vec::with_capacity(node.children.len());
    for child in std::mem::take(&mut node.children) {
        match child.tag.as_str() {
            "CONC" => node.value.push_str(&child.value),
            "CONT" => {
                node.value.push('\n');
                node.value.push_str(&child.value);
            }
            _ => kept.push(fold_continuations(child)),
        }
    }
    node.children = kept;
    node
}

const MAX_LINE_VALUE: usize = 200;

pub fn write(records: &[Node]) -> String {
    let mut out = String::new();
    for r in records {
        write_node(&mut out, r, 0);
    }
    out
}

fn write_node(out: &mut String, node: &Node, level: usize) {
    let mut lines = node.value.split('\n');
    let first = lines.next().unwrap_or("");
    let chunks = split_conc(first);
    write_line(out, level, node.xref.as_deref(), &node.tag, chunks[0]);
    for c in &chunks[1..] {
        write_line(out, level + 1, None, "CONC", c);
    }
    for cont in lines {
        let chunks = split_conc(cont);
        write_line(out, level + 1, None, "CONT", chunks[0]);
        for c in &chunks[1..] {
            write_line(out, level + 1, None, "CONC", c);
        }
    }
    for c in &node.children {
        write_node(out, c, level + 1);
    }
}

fn write_line(out: &mut String, level: usize, xref: Option<&str>, tag: &str, value: &str) {
    let _ = write!(out, "{level}");
    if let Some(x) = xref {
        let _ = write!(out, " @{x}@");
    }
    let _ = write!(out, " {tag}");
    if !value.is_empty() {
        let _ = write!(out, " {value}");
    }
    out.push_str("\r\n");
}

/// Splits a long value into CONC-sized chunks, never splitting next to a
/// space (GEDCOM readers commonly trim those).
fn split_conc(s: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = s;
    while rest.len() > MAX_LINE_VALUE {
        let mut cut = MAX_LINE_VALUE;
        while cut > 1 && (!rest.is_char_boundary(cut) || at_space(rest, cut)) {
            cut -= 1;
        }
        if cut <= 1 {
            break;
        }
        chunks.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    chunks.push(rest);
    chunks
}

fn at_space(s: &str, i: usize) -> bool {
    s[..i].ends_with(' ') || s[i..].starts_with(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let src = "0 HEAD\r\n1 CHAR UTF-8\r\n0 @I1@ INDI\r\n1 NAME John /Smith/\r\n1 NOTE line one\r\n2 CONT line two\r\n0 TRLR\r\n";
        let (recs, w) = parse(src);
        assert!(w.is_empty());
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[1].xref.as_deref(), Some("I1"));
        assert_eq!(recs[1].child_value("NOTE"), "line one\nline two");
        assert_eq!(write(&recs), src);
    }

    #[test]
    fn long_values_use_conc() {
        let long = "word ".repeat(100);
        let n = Node::new("NOTE", long.trim_end());
        let text = write(std::slice::from_ref(&n));
        assert!(text.contains("CONC"));
        let (back, _) = parse(&text);
        assert_eq!(back[0].value, n.value);
    }
}
