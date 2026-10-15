use serde_json::{Map, Value, json};

pub fn plain(rich: &Value) -> String {
    rich.as_array().into_iter().flatten().filter_map(|t| t["plain_text"].as_str()).collect()
}

/// Flatten a property value to something readable: strings, numbers, lists of names/ids.
pub fn value(p: &Value) -> Value {
    let t = p["type"].as_str().unwrap_or("");
    let v = &p[t];
    let names = |arr: &Value, key: &str| -> Value {
        json!(arr.as_array().into_iter().flatten().map(|x| x[key].clone()).collect::<Vec<_>>())
    };
    match t {
        "title" | "rich_text" => json!(plain(v)),
        "select" | "status" => v["name"].clone(),
        "multi_select" => names(v, "name"),
        "people" => json!(v.as_array().into_iter().flatten()
            .map(|u| u.get("name").filter(|n| !n.is_null()).unwrap_or(&u["id"]).clone()).collect::<Vec<_>>()),
        "relation" => names(v, "id"),
        "date" => v["start"].clone(),
        "formula" | "rollup" => {
            let inner = v["type"].as_str().unwrap_or("");
            match (t, inner) {
                ("rollup", "array") => json!(v["array"].as_array().into_iter().flatten().map(value).collect::<Vec<_>>()),
                _ => v[inner].clone(),
            }
        }
        "unique_id" => json!(format!("{}{}", v["prefix"].as_str().map(|p| format!("{p}-")).unwrap_or_default(), v["number"])),
        "files" => json!(v.as_array().into_iter().flatten()
            .map(|f| f["external"]["url"].as_str().or(f["file"]["url"].as_str()).unwrap_or("").to_string()).collect::<Vec<_>>()),
        "created_by" | "last_edited_by" => v.get("name").cloned().unwrap_or(v["id"].clone()),
        _ => v.clone(),
    }
}

/// `{_id, <property>: <flattened value>, ...}` for a page object.
pub fn flatten(page: &Value) -> Value {
    let mut m = Map::new();
    m.insert("_id".into(), page["id"].clone());
    for (k, p) in page["properties"].as_object().into_iter().flatten() {
        m.insert(k.clone(), value(p));
    }
    Value::Object(m)
}

pub fn is_set(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => true,
    }
}

/// Title text of a page or database object.
pub fn title_of(obj: &Value) -> String {
    if obj["object"] == "database" || obj["object"] == "data_source" {
        return plain(&obj["title"]);
    }
    obj["properties"].as_object().into_iter().flatten()
        .find(|(_, p)| p["type"] == "title")
        .map(|(_, p)| plain(&p["title"]))
        .unwrap_or_default()
}
