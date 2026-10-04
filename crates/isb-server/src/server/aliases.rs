//! Argument spellings a newcomer reaches for, mapped to the canonical ones
//! before a call is authorized. The schemas and docs show only the canonical
//! names; the aliases make a guess work instead of failing on an unknown
//! field.

use serde_json::{Value, json};

use super::Tool;

/// `app` for an `app_*` tool's `name`, and `command` (a string, run as
/// `sh -c`, or an array) for an exec tool's `argv`. The canonical name wins
/// when both are given.
pub fn alias_args(tool: &Tool, mut args: Value) -> Value {
    let props = tool.input_schema.get("properties");
    let has = |k: &str| props.is_some_and(|p| p.get(k).is_some());
    let Some(m) = args.as_object_mut() else {
        return args;
    };
    if tool.name.starts_with("app_") && has("name") && !has("app") && !m.contains_key("name") {
        if let Some(v) = m.remove("app") {
            m.insert("name".into(), v);
        }
    }
    if has("argv") && !has("command") && !m.contains_key("argv") {
        if let Some(v) = m.remove("command") {
            let v = match v {
                Value::String(s) => json!(["sh", "-c", s]),
                other => other,
            };
            m.insert("argv".into(), v);
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Tool {
        let schema = json!({"type": "object", "properties": {"name": {}, "argv": {}}});
        Tool::new(name, "x", schema, |a, _| Ok(a))
    }

    #[test]
    fn newcomer_names_map_to_the_canonical_ones() {
        let a = alias_args(&tool("app_exec"), json!({"app": "web", "command": "ls -l"}));
        assert_eq!(a, json!({"name": "web", "argv": ["sh", "-c", "ls -l"]}));
        let a = alias_args(
            &tool("app_exec"),
            json!({"app": "web", "command": ["ls", "-l"]}),
        );
        assert_eq!(a, json!({"name": "web", "argv": ["ls", "-l"]}));
    }

    #[test]
    fn the_canonical_name_wins_and_app_means_name_only_on_app_tools() {
        let a = alias_args(&tool("app_logs"), json!({"app": "x", "name": "web"}));
        assert_eq!(a, json!({"app": "x", "name": "web"}));
        let a = alias_args(
            &tool("instance_exec"),
            json!({"app": "web", "command": ["id"]}),
        );
        assert_eq!(a, json!({"app": "web", "argv": ["id"]}));
        let a = alias_args(&tool("app_top"), json!("not an object"));
        assert_eq!(a, json!("not an object"));
    }
}
