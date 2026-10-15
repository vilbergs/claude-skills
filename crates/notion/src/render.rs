//! Notion blocks -> markdown, in the same dialect `md.rs` parses, so a page can be
//! read, edited and written back.

use crate::api::Notion;
use anyhow::Result;
use serde_json::Value;

pub struct Render<'a> {
    pub api: &'a Notion,
    pub max_depth: usize,
    pub ids: bool,
}

pub fn rich_to_md(rt: &Value) -> String {
    let mut out = String::new();
    for t in rt.as_array().into_iter().flatten() {
        let mut s = t["plain_text"].as_str().unwrap_or("").to_string();
        if s.is_empty() {
            continue;
        }
        let a = &t["annotations"];
        if a["code"] == true {
            s = format!("`{s}`");
        }
        if a["bold"] == true {
            s = format!("**{s}**");
        }
        if a["italic"] == true {
            s = format!("*{s}*");
        }
        if a["strikethrough"] == true {
            s = format!("~~{s}~~");
        }
        if let Some(href) = t["href"].as_str()
            && href != s {
                s = format!("[{s}]({href})");
            }
        out.push_str(&s);
    }
    out
}

fn notion_url(id: &str) -> String {
    format!("https://www.notion.so/{}", id.replace('-', ""))
}

fn file_url(v: &Value) -> &str {
    v["external"]["url"].as_str().or(v["file"]["url"].as_str()).unwrap_or("")
}

impl Render<'_> {
    pub fn page(&self, id: &str) -> Result<String> {
        let mut out = Vec::new();
        let blocks = self.api.children(id)?;
        self.blocks(&blocks, "", 0, &mut out)?;
        Ok(out.join("\n"))
    }

    fn tag(&self, b: &Value) -> String {
        if self.ids { format!(" <!-- {} -->", b["id"].as_str().unwrap_or("")) } else { String::new() }
    }

    fn kids(&self, b: &Value, depth: usize) -> Result<Vec<Value>> {
        if b["has_children"] == true && depth < self.max_depth {
            self.api.children(b["id"].as_str().unwrap_or(""))
        } else {
            Ok(Vec::new())
        }
    }

    fn blocks(&self, blocks: &[Value], ind: &str, depth: usize, out: &mut Vec<String>) -> Result<()> {
        let mut number = 0;
        let mut prev_list = false;
        for b in blocks {
            let t = b["type"].as_str().unwrap_or("");
            let v = &b[t];
            let text = rich_to_md(&v["rich_text"]);
            let tag = self.tag(b);
            let is_list = matches!(t, "bulleted_list_item" | "numbered_list_item" | "to_do");
            // Blank line between blocks, except inside a run of list items.
            if !out.is_empty() && !(is_list && prev_list) {
                out.push(String::new());
            }
            prev_list = is_list;
            if t == "numbered_list_item" { number += 1 } else { number = 0 }
            let multiline = |prefix: &str, s: &str| -> String {
                s.lines().map(|l| format!("{ind}{prefix}{l}")).collect::<Vec<_>>().join("\n")
            };
            match t {
                "paragraph" => out.push(format!("{}{tag}", multiline("", &text))),
                "heading_1" | "heading_2" | "heading_3" => {
                    let h = "#".repeat(t[8..].parse().unwrap_or(1));
                    if v["is_toggleable"] == true {
                        out.push(format!("{ind}<details><summary>{h} {text}</summary>{tag}"));
                        self.blocks(&self.kids(b, depth)?, ind, depth + 1, out)?;
                        out.push(format!("{ind}</details>"));
                        continue;
                    }
                    out.push(format!("{ind}{h} {text}{tag}"));
                }
                "bulleted_list_item" | "numbered_list_item" | "to_do" => {
                    let marker = match t {
                        "bulleted_list_item" => "- ".to_string(),
                        "numbered_list_item" => format!("{number}. "),
                        _ => if v["checked"] == true { "- [x] ".into() } else { "- [ ] ".into() },
                    };
                    let cont = format!("{ind}{}", " ".repeat(marker.len()));
                    let body = text.replace('\n', &format!("\n{cont}"));
                    out.push(format!("{ind}{marker}{body}{tag}"));
                    let kids = self.kids(b, depth)?;
                    if !kids.is_empty() {
                        let mut sub = Vec::new();
                        self.blocks(&kids, &format!("{ind}  "), depth + 1, &mut sub)?;
                        out.extend(sub);
                    }
                    continue;
                }
                "toggle" => {
                    out.push(format!("{ind}<details><summary>{text}</summary>{tag}"));
                    self.blocks(&self.kids(b, depth)?, ind, depth + 1, out)?;
                    out.push(format!("{ind}</details>"));
                    continue;
                }
                "quote" => out.push(format!("{}{tag}", multiline("> ", &text))),
                "callout" => {
                    let icon = v["icon"]["emoji"].as_str().unwrap_or("💡");
                    let body = multiline("> ", &format!("[!{icon}] {text}"));
                    out.push(format!("{body}{tag}"));
                }
                "code" => {
                    let lang = v["language"].as_str().unwrap_or("");
                    let code: String = v["rich_text"].as_array().into_iter().flatten()
                        .filter_map(|x| x["plain_text"].as_str()).collect();
                    out.push(format!("{ind}```{lang}{tag}"));
                    out.push(multiline("", &code));
                    out.push(format!("{ind}```"));
                }
                "divider" => out.push(format!("{ind}---{tag}")),
                "equation" => out.push(format!("{ind}$${}$${tag}", v["expression"].as_str().unwrap_or(""))),
                "table" => {
                    let rows = self.api.children(b["id"].as_str().unwrap_or(""))?;
                    for (n, r) in rows.iter().enumerate() {
                        let cells: Vec<String> = r["table_row"]["cells"].as_array().into_iter().flatten()
                            .map(|c| rich_to_md(c).replace('|', "\\|").replace('\n', " ")).collect();
                        out.push(format!("{ind}| {} |{}", cells.join(" | "), if n == 0 { tag.clone() } else { String::new() }));
                        if n == 0 && v["has_column_header"] == true {
                            out.push(format!("{ind}|{}", "---|".repeat(cells.len())));
                        }
                    }
                }
                "child_page" => out.push(format!("{ind}📄 [{}]({}){tag}", v["title"].as_str().unwrap_or(""), notion_url(b["id"].as_str().unwrap_or("")))),
                "child_database" => out.push(format!("{ind}🗃️ [{}]({}){tag}", v["title"].as_str().unwrap_or(""), notion_url(b["id"].as_str().unwrap_or("")))),
                "link_to_page" => {
                    let id = v["page_id"].as_str().or(v["database_id"].as_str()).unwrap_or("");
                    out.push(format!("{ind}↗ {}{tag}", notion_url(id)));
                }
                "bookmark" | "embed" | "link_preview" => {
                    let cap = rich_to_md(&v["caption"]);
                    let url = v["url"].as_str().unwrap_or("");
                    out.push(format!("{ind}[{}]({url}){tag}", if cap.is_empty() { url } else { &cap }));
                }
                "image" => out.push(format!("{ind}![{}]({}){tag}", rich_to_md(&v["caption"]), file_url(v))),
                "file" | "pdf" | "video" | "audio" => {
                    let name = v["name"].as_str().unwrap_or(t);
                    out.push(format!("{ind}[{name}]({}){tag}", file_url(v)));
                }
                "column_list" | "column" | "synced_block" => {
                    let kids = self.kids(b, depth)?;
                    if !out.is_empty() && out.last().is_some_and(|l| l.is_empty()) {
                        out.pop();
                    }
                    self.blocks(&kids, ind, depth + 1, out)?;
                    continue;
                }
                "table_of_contents" | "breadcrumb" => {
                    out.pop();
                    continue;
                }
                other => out.push(format!("{ind}<!-- {other} block{} -->", if self.ids { format!(" {}", b["id"].as_str().unwrap_or("")) } else { String::new() })),
            }
            // Children of any other block (e.g. paragraph, quote, callout) follow indented.
            let kids = self.kids(b, depth)?;
            if !kids.is_empty() && !matches!(t, "table" | "child_page" | "child_database") {
                let mut sub = Vec::new();
                self.blocks(&kids, &format!("{ind}  "), depth + 1, &mut sub)?;
                out.extend(sub);
            }
        }
        Ok(())
    }
}
