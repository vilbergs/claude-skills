//! Markdown -> Notion blocks.
//!
//! Supported: paragraphs, `#`/`##`/`###` headings, `-`/`*` bullets, `1.` numbered,
//! `- [ ]`/`- [x]` to-dos (nesting by indentation), `> ` quotes, `> [!💡] text` callouts,
//! ``` fences, `---` dividers, pipe tables, `![alt](url)` images,
//! `<details><summary>Title</summary> ... </details>` toggles (`## Title` in the
//! summary makes a toggle heading), and inline `**bold**`, `*italic*`, `~~strike~~`,
//! `` `code` ``, `[text](url)` and bare URLs.

use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

static INLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\*\*(.+?)\*\*",
        r"|~~(.+?)~~",
        r"|`([^`]+)`",
        r"|\[([^\]]+)\]\((https?://[^)\s]+)\)",
        r"|(https?://[^\s)>\]]+)",
        r"|\*([^*\s][^*]*?)\*",
    ))
    .unwrap()
});

/// Notion caps each text object at 2000 characters.
fn push_text(out: &mut Vec<Value>, text: &str, link: Option<&str>, ann: &[&str]) {
    let chars: Vec<char> = text.chars().collect();
    for piece in chars.chunks(2000) {
        let mut t = json!({ "type": "text", "text": { "content": piece.iter().collect::<String>() } });
        if let Some(url) = link {
            t["text"]["link"] = json!({ "url": url });
        }
        if !ann.is_empty() {
            let a: Map<String, Value> = ann.iter().map(|k| (k.to_string(), json!(true))).collect();
            t["annotations"] = Value::Object(a);
        }
        out.push(t);
    }
}

pub fn rich_text(s: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pos = 0;
    for c in INLINE.captures_iter(s) {
        let m = c.get(0).unwrap();
        if m.start() > pos {
            push_text(&mut out, &s[pos..m.start()], None, &[]);
        }
        if let Some(x) = c.get(1) {
            push_text(&mut out, x.as_str(), None, &["bold"]);
        } else if let Some(x) = c.get(2) {
            push_text(&mut out, x.as_str(), None, &["strikethrough"]);
        } else if let Some(x) = c.get(3) {
            push_text(&mut out, x.as_str(), None, &["code"]);
        } else if let (Some(t), Some(u)) = (c.get(4), c.get(5)) {
            push_text(&mut out, t.as_str(), Some(u.as_str()), &[]);
        } else if let Some(u) = c.get(6) {
            push_text(&mut out, u.as_str(), Some(u.as_str()), &[]);
        } else if let Some(x) = c.get(7) {
            push_text(&mut out, x.as_str(), None, &["italic"]);
        }
        pos = m.end();
    }
    if pos < s.len() {
        push_text(&mut out, &s[pos..], None, &[]);
    }
    out
}

pub fn block(kind: &str, text: &str) -> Value {
    json!({ "object": "block", "type": kind, kind: { "rich_text": rich_text(text) } })
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

static TODO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-*+] \[([ xX])\] (.*)$").unwrap());
static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-*+] (.*)$").unwrap());
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+[.)] (.*)$").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6}) (.*)$").unwrap());
static CALLOUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[!([^\]]+)\]\s*(.*)$").unwrap());
static IMAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^!\[([^\]]*)\]\((\S+)\)$").unwrap());
static SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<details>\s*<summary>(.*?)</summary>\s*$").unwrap());

fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_table_sep(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn heading(level: usize, text: &str) -> Value {
    block(&format!("heading_{}", level.min(3)), text)
}

/// Parse markdown into a list of Notion block objects (children nested inline).
pub fn to_blocks(md: &str) -> Vec<Value> {
    let lines: Vec<&str> = md.trim_matches('\n').lines().collect();
    let mut i = 0;
    parse(&lines, &mut i, None)
}

/// Parse until end of input, or until `</details>` when `in_details`.
fn parse(lines: &[&str], i: &mut usize, stop: Option<&str>) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    // Stack of (indent, path into `blocks`) for nesting list items.
    let mut list_stack: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut para: Vec<String> = Vec::new();

    fn flush(blocks: &mut Vec<Value>, para: &mut Vec<String>) {
        if !para.is_empty() {
            blocks.push(block("paragraph", &para.join("\n")));
            para.clear();
        }
    }

    fn container<'a>(blocks: &'a mut Vec<Value>, path: &[usize]) -> &'a mut Vec<Value> {
        let mut cur = blocks;
        for &p in path {
            let b = &mut cur[p];
            let kind = b["type"].as_str().unwrap().to_string();
            let inner = b[&kind].as_object_mut().unwrap();
            cur = inner.entry("children").or_insert_with(|| json!([])).as_array_mut().unwrap();
        }
        cur
    }

    while *i < lines.len() {
        let line = lines[*i];
        let s = line.trim();
        if let Some(stop) = stop
            && s == stop {
                *i += 1;
                break;
            }
        let ind = indent_of(line);
        let list_item = TODO.captures(s).map(|c| {
            let mut b = block("to_do", &c[2]);
            b["to_do"]["checked"] = json!(&c[1] != " ");
            b
        })
        .or_else(|| BULLET.captures(s).map(|c| block("bulleted_list_item", &c[1])))
        .or_else(|| NUMBER.captures(s).map(|c| block("numbered_list_item", &c[1])));

        if let Some(item) = list_item {
            flush(&mut blocks, &mut para);
            while list_stack.last().is_some_and(|(d, _)| *d >= ind) {
                list_stack.pop();
            }
            let parent_path = list_stack.last().map(|(_, p)| p.clone()).unwrap_or_default();
            let target = container(&mut blocks, &parent_path);
            target.push(item);
            let mut path = parent_path;
            path.push(target.len() - 1);
            list_stack.push((ind, path));
            *i += 1;
            continue;
        }

        // An indented plain line right after a list item continues that item's text.
        if ind > 0 && !s.is_empty() && !list_stack.is_empty() && para.is_empty() && !s.starts_with("```") {
            let path = list_stack.last().unwrap().1.clone();
            let (last, parent) = path.split_last().unwrap();
            let b = &mut container(&mut blocks, parent)[*last];
            let kind = b["type"].as_str().unwrap().to_string();
            let rt = b[&kind]["rich_text"].as_array_mut().unwrap();
            rt.extend(rich_text(&format!("\n{s}")));
            *i += 1;
            continue;
        }
        list_stack.clear();

        if s.starts_with("```") {
            flush(&mut blocks, &mut para);
            let lang = s[3..].trim();
            let lang = if lang.is_empty() { "plain text" } else { lang };
            let mut code = Vec::new();
            *i += 1;
            while *i < lines.len() && !lines[*i].trim().starts_with("```") {
                code.push(lines[*i]);
                *i += 1;
            }
            let mut rt = Vec::new();
            push_text(&mut rt, &code.join("\n"), None, &[]);
            blocks.push(json!({ "object": "block", "type": "code", "code": { "rich_text": rt, "language": lang } }));
        } else if s.is_empty() {
            flush(&mut blocks, &mut para);
        } else if let Some(c) = SUMMARY.captures(s) {
            flush(&mut blocks, &mut para);
            let title = c[1].trim().to_string();
            *i += 1;
            let kids = parse(lines, i, Some("</details>"));
            let mut b = match HEADING.captures(&title) {
                Some(h) => {
                    let mut b = heading(h[1].len(), &h[2]);
                    let k = b["type"].as_str().unwrap().to_string();
                    b[&k]["is_toggleable"] = json!(true);
                    b
                }
                None => block("toggle", &title),
            };
            let k = b["type"].as_str().unwrap().to_string();
            if !kids.is_empty() {
                b[&k]["children"] = json!(kids);
            }
            blocks.push(b);
            continue;
        } else if let Some(c) = HEADING.captures(s) {
            flush(&mut blocks, &mut para);
            blocks.push(heading(c[1].len(), &c[2]));
        } else if s == "---" || s == "***" {
            flush(&mut blocks, &mut para);
            blocks.push(json!({ "object": "block", "type": "divider", "divider": {} }));
        } else if s.starts_with('>') {
            flush(&mut blocks, &mut para);
            let mut q = Vec::new();
            while *i < lines.len() && lines[*i].trim().starts_with('>') {
                q.push(lines[*i].trim().trim_start_matches('>').trim_start().to_string());
                *i += 1;
            }
            let text = q.join("\n");
            match CALLOUT.captures(&text) {
                Some(c) => {
                    let mut b = block("callout", &c[2]);
                    b["callout"]["icon"] = json!({ "type": "emoji", "emoji": &c[1] });
                    blocks.push(b);
                }
                None => blocks.push(block("quote", &text)),
            }
            continue;
        } else if s.starts_with('|') {
            flush(&mut blocks, &mut para);
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut header = false;
            while *i < lines.len() && lines[*i].trim().starts_with('|') {
                if is_table_sep(lines[*i]) {
                    header = rows.len() == 1;
                } else {
                    rows.push(table_cells(lines[*i]));
                }
                *i += 1;
            }
            let width = rows.iter().map(Vec::len).max().unwrap_or(1);
            let rows: Vec<Value> = rows
                .into_iter()
                .map(|mut r| {
                    r.resize(width, String::new());
                    let cells: Vec<Value> = r.iter().map(|c| json!(rich_text(c))).collect();
                    json!({ "type": "table_row", "table_row": { "cells": cells } })
                })
                .collect();
            blocks.push(json!({ "object": "block", "type": "table", "table": {
                "table_width": width, "has_column_header": header, "has_row_header": false, "children": rows } }));
            continue;
        } else if let Some(c) = IMAGE.captures(s) {
            flush(&mut blocks, &mut para);
            let mut img = json!({ "type": "external", "external": { "url": &c[2] } });
            if !c[1].is_empty() {
                img["caption"] = json!(rich_text(&c[1]));
            }
            blocks.push(json!({ "object": "block", "type": "image", "image": img }));
        } else {
            para.push(s.to_string());
        }
        *i += 1;
    }
    flush(&mut blocks, &mut para);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_lists() {
        let b = to_blocks("- a\n  - b\n    - c\n- d");
        assert_eq!(b.len(), 2);
        assert_eq!(b[0]["bulleted_list_item"]["children"][0]["bulleted_list_item"]["children"][0]["type"], "bulleted_list_item");
    }

    #[test]
    fn inline() {
        let r = rich_text("a **b** `c` [d](https://x.y) *e* https://z.q");
        let kinds: Vec<_> = r.iter().map(|t| t["text"]["content"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, ["a ", "b", " ", "c", " ", "d", " ", "e", " ", "https://z.q"]);
        assert_eq!(r[1]["annotations"]["bold"], true);
        assert_eq!(r[7]["annotations"]["italic"], true);
    }

    #[test]
    fn table_toggle_callout() {
        let b = to_blocks("| a | b |\n|---|---|\n| 1 | 2 |\n\n<details><summary>## More</summary>\n\ntext\n</details>\n\n> [!💡] note");
        assert_eq!(b[0]["table"]["has_column_header"], true);
        assert_eq!(b[0]["table"]["children"].as_array().unwrap().len(), 2);
        assert_eq!(b[1]["heading_2"]["is_toggleable"], true);
        assert_eq!(b[1]["heading_2"]["children"][0]["type"], "paragraph");
        assert_eq!(b[2]["callout"]["icon"]["emoji"], "💡");
    }

    #[test]
    fn todo_and_numbers() {
        let b = to_blocks("1. one\n2. two\n- [x] done\n- [ ] open");
        assert_eq!(b[0]["type"], "numbered_list_item");
        assert_eq!(b[2]["to_do"]["checked"], true);
        assert_eq!(b[3]["to_do"]["checked"], false);
    }
}
