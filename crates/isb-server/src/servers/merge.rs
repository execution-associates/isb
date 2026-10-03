//! Cross-org reads on a control plane: its own answer plus each server's,
//! merged into one. Every item from a server carries `"server": NAME`; a
//! server that did not answer is listed under `unreachable` rather than
//! failing the call.

use serde_json::{Value, json};

/// The tools whose answers are merged across servers.
pub const FAN_OUT: &[&str] = &["overview", "stack_list", "ingress_status", "org_list"];

fn tagged(items: Option<&Value>, server: &str) -> Vec<Value> {
    items
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .cloned()
                .map(|mut v| {
                    if let Some(o) = v.as_object_mut() {
                        o.insert("server".into(), json!(server));
                    }
                    v
                })
                .collect()
        })
        .unwrap_or_default()
}

fn append(local: &mut Value, key: &str, more: Vec<Value>) {
    if more.is_empty() {
        return;
    }
    match local.get_mut(key).and_then(Value::as_array_mut) {
        Some(a) => a.extend(more),
        None => local[key] = Value::Array(more),
    }
}

/// `local` with each server's answer (`Err`: why it could not be had).
pub fn merge(tool: &str, mut local: Value, remote: Vec<(String, Result<Value, String>)>) -> Value {
    if tool == "org_list" {
        if let Some(a) = local.get_mut("orgs").and_then(Value::as_array_mut) {
            for o in a.iter_mut().filter_map(Value::as_object_mut) {
                o.entry("server").or_insert(json!("local"));
            }
        }
    }
    let mut down = Vec::new();
    let mut servers = serde_json::Map::new();
    for (name, r) in remote {
        let v = match r {
            Ok(v) => v,
            Err(e) => {
                down.push(json!({"server": name, "error": e}));
                continue;
            }
        };
        match tool {
            "stack_list" => append(&mut local, "stacks", tagged(v.get("stacks"), &name)),
            "org_list" => append(&mut local, "orgs", tagged(v.get("orgs"), &name)),
            "overview" => {
                append(&mut local, "stacks", tagged(v.get("stacks"), &name));
                append(&mut local, "sandboxes", tagged(v.get("sandboxes"), &name));
                servers.insert(
                    name.clone(),
                    json!({"host": v.get("host"), "isb": v.get("isb"), "sampled_at": v.get("sampled_at")}),
                );
            }
            _ => {
                servers.insert(name.clone(), v);
            }
        }
    }
    if !servers.is_empty() {
        local["servers"] = Value::Object(servers);
    }
    if !down.is_empty() {
        local["unreachable"] = Value::Array(down);
    }
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_concatenate_with_the_server_named() {
        let local = json!({"stacks": [{"name": "a", "org": "x"}]});
        let m = merge(
            "stack_list",
            local,
            vec![
                (
                    "box".into(),
                    Ok(json!({"stacks": [{"name": "b", "org": "y"}]})),
                ),
                ("gone".into(), Err("timeout".into())),
            ],
        );
        let s = m["stacks"].as_array().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].get("server"), None, "local items stay as they were");
        assert_eq!(s[1]["server"], "box");
        assert_eq!(m["unreachable"][0]["server"], "gone");
    }

    #[test]
    fn overview_keeps_local_host_and_cursor_and_adds_servers() {
        let local = json!({"host": {"cpu": 1}, "events_seq": 9, "stacks": [], "sandboxes": []});
        let m = merge(
            "overview",
            local,
            vec![(
                "box".into(),
                Ok(
                    json!({"host": {"cpu": 2}, "events_seq": 100, "isb": "0.7.0", "stacks": [{"name": "s"}], "sandboxes": [{"name": "i"}]}),
                ),
            )],
        );
        assert_eq!(
            m["events_seq"], 9,
            "events are mirrored: the local cursor is the feed's"
        );
        assert_eq!(m["host"]["cpu"], 1);
        assert_eq!(m["servers"]["box"]["host"]["cpu"], 2);
        assert_eq!(m["stacks"][0]["server"], "box");
        assert_eq!(m["sandboxes"][0]["server"], "box");
    }

    #[test]
    fn org_list_marks_local_orgs() {
        let m = merge(
            "org_list",
            json!({"orgs": [{"name": "a"}]}),
            vec![("box".into(), Ok(json!({"orgs": [{"name": "b"}]})))],
        );
        assert_eq!(m["orgs"][0]["server"], "local");
        assert_eq!(m["orgs"][1]["server"], "box");
        let m = merge(
            "ingress_status",
            json!({"enabled": false}),
            vec![("box".into(), Ok(json!({"enabled": true})))],
        );
        assert_eq!(m["servers"]["box"]["enabled"], true);
    }
}
