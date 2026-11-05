//! Minimal, lenient XHTML DOM with lxml-like text/tail semantics.
//! Namespaces are stripped (local names only), entities are decoded.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

pub const NIL: usize = usize::MAX;

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<usize>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub parent: usize,
}

#[derive(Clone, Debug)]
pub struct Doc {
    pub nodes: Vec<Node>,
    pub root: usize,
}

/// Tags to which kindlegen adds the aid attribute.
pub const AID_ABLE: &[&str] = &[
    "a", "abbr", "address", "article", "aside", "audio", "b", "bdo", "blockquote", "body", "button", "cite",
    "code", "dd", "del", "details", "dfn", "div", "dl", "dt", "em", "fieldset", "figcaption", "figure",
    "footer", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hgroup", "i", "ins", "kbd", "label", "legend",
    "li", "map", "mark", "meter", "nav", "ol", "output", "p", "pre", "progress", "q", "rp", "rt", "samp",
    "section", "select", "small", "span", "strong", "sub", "summary", "sup", "textarea", "time", "ul", "var",
    "video",
];

pub fn is_aid_able(name: &str) -> bool {
    AID_ABLE.contains(&name)
}

fn needs_close_tag(name: &str) -> bool {
    is_aid_able(name) || matches!(name, "script" | "style" | "title" | "head")
}

fn local<T: AsRef<[u8]> + ?Sized>(name: &T) -> String {
    let s = String::from_utf8_lossy(name.as_ref());
    match s.rfind(':') {
        Some(i) => s[i + 1..].to_string(),
        None => s.into_owned(),
    }
}

pub fn decode_entities(raw: &str) -> String {
    if raw.contains('&') {
        html_escape::decode_html_entities(raw).into_owned()
    } else {
        raw.to_string()
    }
}

impl Doc {
    pub fn parse(data: &[u8]) -> Doc {
        let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
        let text = String::from_utf8_lossy(data);
        let mut r = Reader::from_str(&text);
        {
            let c = r.config_mut();
            c.check_end_names = false;
            c.allow_unmatched_ends = true;
            c.allow_dangling_amp = true;
            c.check_comments = false;
        }
        let mut doc = Doc { nodes: Vec::new(), root: NIL };
        let mut stack: Vec<usize> = Vec::new();
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) => {
                    let id = doc.new_from(&e);
                    if doc.attach(&stack, id) {
                        stack.push(id);
                    }
                }
                Ok(Event::Empty(e)) => {
                    let id = doc.new_from(&e);
                    doc.attach(&stack, id);
                }
                Ok(Event::End(e)) => {
                    let name = local(e.name().as_ref());
                    if let Some(pos) = stack.iter().rposition(|&i| doc.nodes[i].name == name) {
                        stack.truncate(pos);
                    }
                }
                Ok(Event::Text(t)) => {
                    let raw = String::from_utf8_lossy(AsRef::<[u8]>::as_ref(&*t));
                    doc.append_text(&stack, &decode_entities(&raw));
                }
                Ok(Event::GeneralRef(r)) => {
                    let raw = format!("&{};", String::from_utf8_lossy(AsRef::<[u8]>::as_ref(&*r)));
                    let dec = decode_entities(&raw);
                    doc.append_text(&stack, &dec);
                }
                Ok(Event::CData(t)) => {
                    let raw = String::from_utf8_lossy(AsRef::<[u8]>::as_ref(&*t)).into_owned();
                    doc.append_text(&stack, &raw);
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if doc.root == NIL {
            doc.root = doc.create("html", vec![]);
        }
        doc
    }

    fn new_from(&mut self, e: &BytesStart) -> usize {
        let name = local(e.name().as_ref());
        let mut attrs: Vec<(String, String)> = Vec::new();
        for a in e.html_attributes().with_checks(false).flatten() {
            let key: &[u8] = AsRef::<[u8]>::as_ref(a.key.as_ref());
            if key == b"xmlns" || key.starts_with(b"xmlns:") {
                continue;
            }
            let k = local(key);
            let v = decode_entities(&String::from_utf8_lossy(AsRef::<[u8]>::as_ref(&*a.value)));
            if let Some(slot) = attrs.iter_mut().find(|(ek, _)| *ek == k) {
                slot.1 = v;
            } else {
                attrs.push((k, v));
            }
        }
        self.create(&name, attrs)
    }

    pub fn create(&mut self, name: &str, attrs: Vec<(String, String)>) -> usize {
        self.nodes.push(Node { name: name.to_string(), attrs, parent: NIL, ..Default::default() });
        self.nodes.len() - 1
    }

    fn attach(&mut self, stack: &[usize], id: usize) -> bool {
        if let Some(&p) = stack.last() {
            self.nodes[id].parent = p;
            self.nodes[p].children.push(id);
            true
        } else if self.root == NIL {
            self.root = id;
            true
        } else {
            false
        }
    }

    fn append_text(&mut self, stack: &[usize], s: &str) {
        if s.is_empty() {
            return;
        }
        let Some(&p) = stack.last() else { return };
        let target = match self.nodes[p].children.last() {
            Some(&last) => &mut self.nodes[last].tail,
            None => &mut self.nodes[p].text,
        };
        match target {
            Some(t) => t.push_str(s),
            None => *target = Some(s.to_string()),
        }
    }

    pub fn attr(&self, id: usize, k: &str) -> Option<&str> {
        self.nodes[id].attrs.iter().find(|(ak, _)| ak == k).map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, id: usize, k: &str, v: String) {
        let attrs = &mut self.nodes[id].attrs;
        if let Some(slot) = attrs.iter_mut().find(|(ak, _)| ak == k) {
            slot.1 = v;
        } else {
            attrs.push((k.to_string(), v));
        }
    }

    pub fn remove_attr(&mut self, id: usize, k: &str) -> Option<String> {
        let attrs = &mut self.nodes[id].attrs;
        let pos = attrs.iter().position(|(ak, _)| ak == k)?;
        Some(attrs.remove(pos).1)
    }

    /// Remove `child` from its parent. Like lxml, the tail goes with it.
    pub fn remove(&mut self, child: usize) {
        let p = self.nodes[child].parent;
        if p != NIL {
            self.nodes[p].children.retain(|&c| c != child);
        }
        self.nodes[child].parent = NIL;
    }

    /// Remove `child` but keep its tail text in the document (lxml `extract`).
    pub fn extract(&mut self, child: usize) {
        let p = self.nodes[child].parent;
        if p == NIL {
            return;
        }
        let tail = self.nodes[child].tail.take();
        let idx = self.nodes[p].children.iter().position(|&c| c == child).unwrap();
        if let Some(t) = tail {
            if idx == 0 {
                self.nodes[p].text.get_or_insert_with(String::new).push_str(&t);
            } else {
                let prev = self.nodes[p].children[idx - 1];
                self.nodes[prev].tail.get_or_insert_with(String::new).push_str(&t);
            }
        }
        self.remove(child);
    }

    /// Replace `old` with `new` (new takes over old's position and tail).
    pub fn replace(&mut self, old: usize, new: usize) {
        let p = self.nodes[old].parent;
        if p == NIL {
            return;
        }
        let idx = self.nodes[p].children.iter().position(|&c| c == old).unwrap();
        self.nodes[p].children[idx] = new;
        self.nodes[new].parent = p;
        self.nodes[new].tail = self.nodes[old].tail.take();
        self.nodes[old].parent = NIL;
    }

    pub fn append_child(&mut self, parent: usize, child: usize) {
        self.nodes[child].parent = parent;
        self.nodes[parent].children.push(child);
    }

    /// Pre-order list of descendants (excluding `id`).
    pub fn descendants(&self, id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.nodes[id].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev().copied());
        }
        out
    }

    pub fn find_first(&self, id: usize, name: &str) -> Option<usize> {
        if self.nodes[id].name == name {
            return Some(id);
        }
        self.descendants(id).into_iter().find(|&n| self.nodes[n].name == name)
    }

    /// Concatenated text content.
    pub fn text_content(&self, id: usize) -> String {
        let mut s = String::new();
        fn rec(d: &Doc, id: usize, s: &mut String) {
            if let Some(t) = &d.nodes[id].text {
                s.push_str(t);
            }
            for &c in &d.nodes[id].children {
                rec(d, c, s);
                if let Some(t) = &d.nodes[c].tail {
                    s.push_str(t);
                }
            }
        }
        rec(self, id, &mut s);
        s
    }

    pub fn serialize(&self, id: usize, out: &mut Vec<u8>, with_tail: bool) {
        self.ser(id, out, false);
        if with_tail {
            if let Some(t) = &self.nodes[id].tail {
                escape_text(t, out);
            }
        }
    }

    pub fn serialize_root(&self, out: &mut Vec<u8>) {
        self.ser(self.root, out, true);
        if let Some(t) = &self.nodes[self.root].tail {
            escape_text(t, out);
        }
    }

    pub fn to_bytes(&self, id: usize, with_tail: bool) -> Vec<u8> {
        let mut v = Vec::new();
        self.serialize(id, &mut v, with_tail);
        v
    }

    fn ser(&self, id: usize, out: &mut Vec<u8>, xmlns: bool) {
        let n = &self.nodes[id];
        out.push(b'<');
        out.extend_from_slice(n.name.as_bytes());
        if xmlns {
            out.extend_from_slice(b" xmlns=\"http://www.w3.org/1999/xhtml\"");
        }
        for (k, v) in &n.attrs {
            out.push(b' ');
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(b"=\"");
            escape_attr(v, out);
            out.push(b'"');
        }
        let empty_text = n.text.as_deref().is_none_or(|t| t.is_empty());
        if n.children.is_empty() && empty_text {
            if needs_close_tag(&n.name) {
                out.extend_from_slice(b"></");
                out.extend_from_slice(n.name.as_bytes());
                out.push(b'>');
            } else {
                out.extend_from_slice(b"/>");
            }
            return;
        }
        out.push(b'>');
        if let Some(t) = &n.text {
            escape_text(t, out);
        }
        for &c in &n.children {
            self.ser(c, out, false);
            if let Some(t) = &self.nodes[c].tail {
                escape_text(t, out);
            }
        }
        out.extend_from_slice(b"</");
        out.extend_from_slice(n.name.as_bytes());
        out.push(b'>');
    }
}

pub fn escape_text(s: &str, out: &mut Vec<u8>) {
    let b = s.as_bytes();
    let mut last = 0;
    for (i, &c) in b.iter().enumerate() {
        let rep: &[u8] = match c {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            _ => continue,
        };
        out.extend_from_slice(&b[last..i]);
        out.extend_from_slice(rep);
        last = i + 1;
    }
    out.extend_from_slice(&b[last..]);
}

pub fn escape_attr(s: &str, out: &mut Vec<u8>) {
    let b = s.as_bytes();
    let mut last = 0;
    for (i, &c) in b.iter().enumerate() {
        let rep: &[u8] = match c {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b'"' => b"&quot;",
            b'\n' => b"&#10;",
            b'\r' => b"&#13;",
            b'\t' => b"&#9;",
            _ => continue,
        };
        out.extend_from_slice(&b[last..i]);
        out.extend_from_slice(rep);
        last = i + 1;
    }
    out.extend_from_slice(&b[last..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_serialize() {
        let src = br#"<?xml version="1.0"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en"><head><title>T</title></head><body><p class="a">Hi &amp; &nbsp;x<br/>tail</p><svg:svg xmlns:svg="x"/></body></html>"#;
        let d = Doc::parse(src);
        assert_eq!(d.nodes[d.root].name, "html");
        assert_eq!(d.attr(d.root, "lang"), Some("en"));
        let mut out = Vec::new();
        d.serialize(d.root, &mut out, false);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s,
            "<html lang=\"en\"><head><title>T</title></head><body><p class=\"a\">Hi &amp; \u{a0}x<br/>tail</p><svg/></body></html>"
        );
    }
}
