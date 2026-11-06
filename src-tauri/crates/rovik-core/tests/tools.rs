//! The tools a request offers: what the project's memory can be asked, and
//! when it is offered at all.
//!
//! `tools_for` is the one place an offered set is decided, so the memory tools
//! are checked through it: their schemas, and the two conditions that keep
//! them out of a prompt — no project to have a memory about, and a project
//! that turned its memory off.

use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::tools::tools_for;
use serde_json::{json, Value};

/// The names of the tools in a list, as the model sees them.
fn names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// One tool's schema, by name.
fn schema(tools: &[Value], name: &str) -> Value {
    tools
        .iter()
        .find(|t| t.pointer("/function/name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("{name} isn't offered: {:?}", names(tools)))
        .clone()
}

/// The offered set of an ordinary agent turn: no plan mode, no CLM, not
/// restricted to context tools.
fn offered(has_project: bool, memory: bool, no_tools: bool) -> Vec<Value> {
    tools_for(
        has_project,
        memory,
        false,
        no_tools,
        &Permissions::default(),
        false,
        false,
        false,
    )
}

fn requires(tool: &Value, key: &str) -> bool {
    tool.pointer("/function/parameters/required")
        .and_then(Value::as_array)
        .map(|required| required.iter().any(|r| r.as_str() == Some(key)))
        .unwrap_or(false)
}

// The two tools the project's memory can be asked through (PR 3).

#[test]
fn the_memory_tools_are_offered_to_a_project_whose_memory_is_on() {
    let tools = offered(true, true, false);
    let names = names(&tools);
    assert!(
        names.contains(&"memory_search".to_string()),
        "a project with a memory is offered a way to search it: {names:?}"
    );
    assert!(
        names.contains(&"memory_get".to_string()),
        "a project with a memory is offered a way to read one record of it: {names:?}"
    );
}

#[test]
fn a_project_that_turned_memory_off_is_offered_no_memory_tool() {
    let tools = offered(true, false, false);
    let names = names(&tools);
    assert!(
        !names.contains(&"memory_search".to_string()),
        "memory that is off is not searched: {names:?}"
    );
    assert!(
        !names.contains(&"memory_get".to_string()),
        "memory that is off is not read: {names:?}"
    );
    assert!(
        names.contains(&"read".to_string()),
        "the rest of the tools are still offered: {names:?}"
    );
}

#[test]
fn without_a_project_no_memory_tool_is_offered_even_with_memory_on() {
    let tools = offered(false, true, false);
    let names = names(&tools);
    assert!(
        !names.contains(&"memory_search".to_string()),
        "there is no project, so there is no memory to search: {names:?}"
    );
    assert!(
        !names.contains(&"memory_get".to_string()),
        "there is no project, so there is no record to read: {names:?}"
    );
}

#[test]
fn a_model_that_takes_no_tools_is_offered_no_memory_tool() {
    let tools = offered(true, true, true);
    let names = names(&tools);
    assert!(
        !names.contains(&"memory_search".to_string()),
        "a model that takes no tools is offered no memory tool: {names:?}"
    );
    assert!(
        !names.contains(&"memory_get".to_string()),
        "a model that takes no tools is offered no memory tool: {names:?}"
    );
}

#[test]
fn memory_search_takes_a_query_and_may_be_narrowed_by_files_and_a_limit() {
    let tools = offered(true, true, false);
    let search = schema(&tools, "memory_search");

    assert!(
        requires(&search, "query"),
        "the one thing a search cannot do without is what to search for: {search}"
    );
    assert_eq!(
        search.pointer("/function/parameters/properties/query/type"),
        Some(&json!("string")),
        "a search takes a query as text: {search}"
    );
    assert!(
        !requires(&search, "files"),
        "the files to look around are a narrowing, not a demand: {search}"
    );
    assert_eq!(
        search.pointer("/function/parameters/properties/files/type"),
        Some(&json!("array")),
        "files come as a list: {search}"
    );
    assert_eq!(
        search.pointer("/function/parameters/properties/files/items/type"),
        Some(&json!("string")),
        "each file is a path: {search}"
    );
    assert!(
        !requires(&search, "limit"),
        "a limit is a choice, not a demand: {search}"
    );
    assert_eq!(
        search.pointer("/function/parameters/properties/limit/type"),
        Some(&json!("integer")),
        "the limit counts records: {search}"
    );
    assert_eq!(
        search.pointer("/function/parameters/properties/limit/maximum"),
        Some(&json!(8)),
        "a search never asks for more than eight records: {search}"
    );
    assert_eq!(
        search.pointer("/function/parameters/properties/limit/default"),
        Some(&json!(4)),
        "a search that says nothing else is answered with four records: {search}"
    );
}

#[test]
fn memory_get_takes_one_id() {
    let tools = offered(true, true, false);
    let get = schema(&tools, "memory_get");

    assert!(
        requires(&get, "id"),
        "a record is read by its id: {get}"
    );
    assert_eq!(
        get.pointer("/function/parameters/properties/id/type"),
        Some(&json!("string")),
        "the id is text, as the packet shows it: {get}"
    );
    assert!(
        get.pointer("/function/parameters/properties/query").is_none(),
        "reading one record is not a search: {get}"
    );
}
