//! Printing results: JSON, aligned tables, label lists.

use isb::sandbox::SandboxInfo;
use serde::Serialize;

pub(crate) fn print_json<T: Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).expect("serializable"));
}

pub(crate) fn table(rows: Vec<Vec<String>>) {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut w = vec![0; n];
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i + 1 == r.len() {
                    c.clone()
                } else {
                    format!("{c:<width$}", width = w[i])
                }
            })
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

pub(crate) fn labels_brief(i: &SandboxInfo) -> String {
    i.labels
        .iter()
        .filter(|(k, _)| !k.starts_with("isb."))
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}
