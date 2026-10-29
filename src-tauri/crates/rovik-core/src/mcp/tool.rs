//! A server's tool as Rovik keeps it from `tools/list`: what the agent's
//! list shows of it, and the arguments a modern HTTP call mirrors into its
//! headers (`x-mcp-header`, the spec's Streamable HTTP "Custom Headers from
//! Tool Parameters").

use serde_json::Value;

/// One tool of a server.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// Its arguments' JSON Schema.
    pub input_schema: Value,
    /// Its annotations as the server gave them.
    pub annotations: Option<Value>,
    /// The server marks it as changing nothing (`readOnlyHint`): its claim
    /// (docs/design/mcp-plan.md, Q08).
    pub read_only: bool,
    /// The arguments a modern HTTP call mirrors into headers: each one's
    /// path of properties, and its header's name (`Mcp-Param-<name>`).
    pub(super) headers: Vec<(Vec<String>, String)>,
}

impl Tool {
    /// A tool as a server might list it (tests).
    #[cfg(test)]
    pub fn new(name: &str, description: &str, input_schema: Value, read_only: bool) -> Tool {
        Tool {
            name: name.into(),
            description: description.into(),
            input_schema,
            annotations: read_only.then(|| serde_json::json!({ "readOnlyHint": true })),
            read_only,
            headers: Vec::new(),
        }
    }
}

/// The tools of a `tools/list` result. With `checks_headers` (a modern
/// HTTP server's), a tool whose `x-mcp-header` breaks the spec's rules is
/// left out, as the spec says, with a warning in the log.
pub(super) fn tools_of(result: &Value, checks_headers: bool) -> Vec<Tool> {
    let Some(listed) = result.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    listed
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            let input_schema = tool
                .get("inputSchema")
                .filter(|s| s.is_object())
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
            let headers = if checks_headers {
                match header_paths(&input_schema) {
                    Ok(headers) => headers,
                    Err(why) => {
                        log::warn!("mcp: the tool {name} is left out: {why}");
                        return None;
                    }
                }
            } else {
                Vec::new()
            };
            let annotations = tool.get("annotations").filter(|a| a.is_object()).cloned();
            let read_only = annotations
                .as_ref()
                .and_then(|a| a.get("readOnlyHint"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(Tool {
                name: name.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                input_schema,
                annotations,
                read_only,
                headers,
            })
        })
        .collect()
}

/// The `x-mcp-header` properties of a schema: each one's path from the
/// root, through `properties` alone, and its header's name. An annotation
/// the rules refuse (its name, its type, a place reached through anything
/// but `properties`) makes the whole tool invalid.
fn header_paths(schema: &Value) -> Result<Vec<(Vec<String>, String)>, String> {
    let mut found = Vec::new();
    walk(schema, &mut Vec::new(), &mut found)?;
    let reached = found.len();
    if count_annotations(schema) != reached {
        return Err("an x-mcp-header outside the schema's properties".into());
    }
    let mut names: Vec<String> = found.iter().map(|(_, n)| n.to_ascii_lowercase()).collect();
    names.sort();
    names.dedup();
    if names.len() != reached {
        return Err("two x-mcp-header properties with the same name".into());
    }
    Ok(found)
}

/// The annotated properties under `schema`'s `properties`, depth first.
fn walk(
    schema: &Value,
    path: &mut Vec<String>,
    found: &mut Vec<(Vec<String>, String)>,
) -> Result<(), String> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    for (key, property) in properties {
        path.push(key.clone());
        if let Some(header) = property.get("x-mcp-header") {
            let name = header
                .as_str()
                .filter(|n| is_token(n))
                .ok_or_else(|| format!("x-mcp-header of {key} isn't a header name"))?;
            let primitive = property
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| matches!(t, "string" | "integer" | "boolean"));
            if !primitive {
                return Err(format!(
                    "x-mcp-header on {key}, which isn't a string, integer or boolean"
                ));
            }
            found.push((path.clone(), name.to_string()));
        }
        walk(property, path, found)?;
        path.pop();
    }
    Ok(())
}

/// How many `x-mcp-header` keys `value` has anywhere.
fn count_annotations(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, v)| usize::from(key == "x-mcp-header") + count_annotations(v))
            .sum(),
        Value::Array(items) => items.iter().map(count_annotations).sum(),
        _ => 0,
    }
}

/// An HTTP field name (RFC 9110's `1*tchar`).
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
}

/// The `Mcp-Param-…` headers of a call of `tool` with `arguments`: each
/// annotated argument that has a value, as text, encoded where it must be.
pub(super) fn header_values(tool: &Tool, arguments: &Value) -> Vec<(String, String)> {
    const SAFE: i64 = (1 << 53) - 1;
    tool.headers
        .iter()
        .filter_map(|(path, name)| {
            let value = path.iter().try_fold(arguments, |at, key| at.get(key))?;
            let text = match value {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n
                    .as_i64()
                    .filter(|n| (-SAFE..=SAFE).contains(n))?
                    .to_string(),
                _ => return None,
            };
            Some((
                format!("Mcp-Param-{name}"),
                super::http::header_value(&text),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_listed_tool_keeps_its_schema_and_its_read_only_mark() {
        let listed = tools_of(
            &json!({ "tools": [
                { "name": "search", "description": " Find things. ", "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } } }, "annotations": { "readOnlyHint": true } },
                { "name": "delete" },
                { "description": "no name" },
            ] }),
            false,
        );
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "search");
        assert_eq!(listed[0].description, "Find things.");
        assert!(listed[0].read_only);
        assert_eq!(listed[1].input_schema, json!({ "type": "object" }));
        assert!(!listed[1].read_only, "no mark: it may change things");
    }

    /// The spec's example: `region` goes as `Mcp-Param-Region`; a nested
    /// property's path is followed; a value that's absent sends nothing.
    #[test]
    fn annotated_arguments_become_headers() {
        let listed = tools_of(
            &json!({ "tools": [{ "name": "execute_sql", "inputSchema": { "type": "object", "properties": {
                "region": { "type": "string", "x-mcp-header": "Region" },
                "query": { "type": "string" },
                "opts": { "type": "object", "properties": { "limit": { "type": "integer", "x-mcp-header": "Limit" }, "dry": { "type": "boolean", "x-mcp-header": "Dry" } } },
            } } }] }),
            true,
        );
        assert_eq!(listed.len(), 1);
        let mut headers = header_values(
            &listed[0],
            &json!({ "region": "us-west1", "query": "SELECT 1", "opts": { "limit": 42 } }),
        );
        headers.sort();
        assert_eq!(
            headers,
            [
                ("Mcp-Param-Limit".to_string(), "42".to_string()),
                ("Mcp-Param-Region".to_string(), "us-west1".to_string()),
            ]
        );
        assert_eq!(
            header_values(&listed[0], &json!({ "region": "Zürich" })),
            [(
                "Mcp-Param-Region".to_string(),
                "=?base64?WsO8cmljaA==?=".to_string()
            )]
        );
    }

    #[test]
    fn a_tool_with_an_annotation_the_rules_refuse_is_left_out() {
        let tool = |properties: Value| json!({ "tools": [{ "name": "t", "inputSchema": { "type": "object", "properties": properties } }] });
        let refused = [
            json!({ "a": { "type": "number", "x-mcp-header": "A" } }),
            json!({ "a": { "type": "string", "x-mcp-header": "" } }),
            json!({ "a": { "type": "string", "x-mcp-header": "Bad Name" } }),
            json!({ "a": { "type": "string", "x-mcp-header": "Same" }, "b": { "type": "string", "x-mcp-header": "same" } }),
            json!({ "a": { "type": "array", "items": { "type": "string", "x-mcp-header": "A" } } }),
            json!({ "a": { "anyOf": [{ "type": "string", "x-mcp-header": "A" }] } }),
        ];
        for properties in refused {
            assert!(
                tools_of(&tool(properties.clone()), true).is_empty(),
                "{properties}"
            );
            assert_eq!(
                tools_of(&tool(properties.clone()), false).len(),
                1,
                "not checked off modern HTTP: {properties}"
            );
        }
    }
}
