//! Parse declarations without executing candidate code. Unsupported loading fails closed.
use crate::{require, Error, Result};
use serde::Serialize;
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct Import {
    pub source: String,
    pub module: String,
    /// Python's from-import member; it may name a submodule or an exported value.
    pub member: Option<String>,
    pub require: bool,
    /// Generated JavaScript to resolve at runtime; the source retains its graph identity.
    pub runtime_source: Option<String>,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str> {
    source
        .get(node.byte_range())
        .ok_or_else(|| Error::new("invalid import syntax range".into()))
}
fn string(node: Node<'_>, source: &str) -> Result<String> {
    require(
        node.kind() == "string",
        "computed imports require a separately registered architecture checker",
    )?;
    let value = text(node, source)?;
    require(
        value.len() >= 2 && !value.contains('\\'),
        "escaped import specifiers are unsupported",
    )?;
    let quote = value.as_bytes()[0];
    require(
        matches!(quote, b'\'' | b'"') && value.as_bytes().last() == Some(&quote),
        "unsupported import literal",
    )?;
    Ok(value[1..value.len() - 1].into())
}
fn named(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect()
}
fn field<'a>(node: Node<'a>, name: &str) -> Result<Node<'a>> {
    node.child_by_field_name(name)
        .ok_or_else(|| Error::new(format!("import syntax lacks {name}")))
}
fn imported_name<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str> {
    text(
        if node.kind() == "aliased_import" {
            field(node, "name")?
        } else {
            node
        },
        source,
    )
}

fn loading_capability(name: &str) -> bool {
    matches!(
        name,
        "eval"
            | "Function"
            | "Reflect"
            | "constructor"
            | "createRequire"
            | "getBuiltinModule"
            | "registerHooks"
            | "getOwnPropertyDescriptor"
            | "getOwnPropertyDescriptors"
            | "getPrototypeOf"
            | "__proto__"
            | "globalThis"
            | "global"
            | "window"
            | "self"
            | "require"
    )
}

fn property_value(
    node: Node<'_>,
    source: &str,
    bindings: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    match node.kind() {
        "string" => string(node, source).ok(),
        "identifier" => bindings.get(text(node, source).ok()?).cloned(),
        "parenthesized_expression" => property_value(*named(node).first()?, source, bindings),
        "binary_expression" if text(node.child_by_field_name("operator")?, source).ok()? == "+" => {
            let left = property_value(node.child_by_field_name("left")?, source, bindings)?;
            let right = property_value(node.child_by_field_name("right")?, source, bindings)?;
            (left.len() + right.len() <= 1024).then(|| left + &right)
        }
        _ => None,
    }
}

fn expression_has_computed_value(
    node: Node<'_>,
    source: &str,
    bindings: &std::collections::BTreeMap<String, String>,
    aliases: &std::collections::BTreeSet<String>,
) -> Result<bool> {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == "identifier" && aliases.contains(text(node, source)?) {
            return Ok(true);
        }
        if node.kind() == "subscript_expression"
            && property_value(field(node, "index")?, source, bindings).is_none()
        {
            return Ok(true);
        }
        pending.extend(named(node));
    }
    Ok(false)
}

fn javascript_capabilities(root: Node<'_>, source: &str) -> Result<()> {
    let mut nodes = vec![];
    let mut pending = vec![(root, 0)];
    while let Some((node, depth)) = pending.pop() {
        require(
            nodes.len() < 200_000 && depth < 256,
            "import syntax exceeds analysis limits",
        )?;
        nodes.push(node);
        pending.extend(named(node).into_iter().map(|child| (child, depth + 1)));
    }
    let mut bindings = std::collections::BTreeMap::new();
    let mut uncertain = std::collections::BTreeSet::new();
    let mut dangerous = std::collections::BTreeSet::new();
    let mut computed_values = std::collections::BTreeSet::new();
    // A name is never made safe by a later declaration or assignment. Scope
    // ambiguity is conservative; this is a bounded capability analysis, not JS
    // evaluation or a proof of arbitrary program dataflow.
    for _ in 0..32 {
        let before = (
            bindings.clone(),
            uncertain.clone(),
            dangerous.clone(),
            computed_values.clone(),
        );
        for node in &nodes {
            let fields = match node.kind() {
                "variable_declarator" => Some(("name", "value")),
                "assignment_expression" => Some(("left", "right")),
                _ => None,
            };
            if let Some((name, value)) = fields {
                let Some(value) = node.child_by_field_name(value) else {
                    continue;
                };
                let name = field(*node, name)?;
                if name.kind() != "identifier" {
                    continue;
                }
                let name = text(name, source)?.to_owned();
                if let Some(value) = property_value(value, source, &bindings) {
                    if loading_capability(&value) {
                        dangerous.insert(name.clone());
                    }
                    if bindings.get(&name).is_some_and(|old| old != &value) {
                        uncertain.insert(name.clone());
                    }
                    if !uncertain.contains(&name) {
                        bindings.insert(name.clone(), value);
                    }
                } else {
                    uncertain.insert(name.clone());
                }
                if expression_has_computed_value(value, source, &bindings, &computed_values)? {
                    computed_values.insert(name.clone());
                }
            }
        }
        for name in &uncertain {
            bindings.remove(name);
        }
        if before
            == (
                bindings.clone(),
                uncertain.clone(),
                dangerous.clone(),
                computed_values.clone(),
            )
        {
            break;
        }
    }
    for node in nodes {
        if node.kind() == "call_expression" || node.kind() == "new_expression" {
            let function = field(
                node,
                if node.kind() == "call_expression" {
                    "function"
                } else {
                    "constructor"
                },
            )?;
            require(!expression_has_computed_value(function, source, &bindings, &computed_values)?, "calling values extracted through unresolved computed properties needs a separately registered architecture checker")?;
        }
        if node.kind() == "identifier" && text(node, source)? == "Reflect" {
            return Err(Error::new(
                "JavaScript reflection needs a separately registered architecture checker".into(),
            ));
        }
        if node.kind() == "identifier"
            && matches!(
                text(node, source)?,
                "globalThis" | "global" | "window" | "self"
            )
        {
            require(
                node.parent().is_some_and(|parent| {
                    matches!(parent.kind(), "member_expression" | "subscript_expression")
                        && parent.child_by_field_name("object") == Some(node)
                }),
                "aliased global objects need a separately registered architecture checker",
            )?;
        }
        if node.kind() == "member_expression" {
            let property = text(field(node, "property")?, source)?;
            require(
                !loading_capability(property),
                "reflective JavaScript loading needs a separately registered architecture checker",
            )?;
        }
        if node.kind() == "subscript_expression" {
            let index = field(node, "index")?;
            let value = property_value(index, source, &bindings);
            require(
                !value.as_deref().is_some_and(loading_capability)
                    && !(index.kind() == "identifier" && dangerous.contains(text(index, source)?)),
                "reflective JavaScript loading needs a separately registered architecture checker",
            )?;
            require(
                index.kind() != "string" || value.is_some(),
                "escaped computed properties need a separately registered architecture checker",
            )?;
            let object = text(field(node, "object")?, source)?;
            require(
                !matches!(object, "globalThis" | "global" | "window" | "self") || value.is_some(),
                "computed global access needs a separately registered architecture checker",
            )?;
        }
        if matches!(
            node.kind(),
            "pair_pattern" | "shorthand_property_identifier_pattern"
        ) {
            let key = node.child_by_field_name("key").unwrap_or(node);
            if key.kind() == "computed_property_name" {
                let value = named(key)
                    .first()
                    .and_then(|key| property_value(*key, source, &bindings));
                require(value.as_deref().is_some_and(|name| !loading_capability(name)), "computed capability destructuring needs a separately registered architecture checker")?;
            }
            let name = text(key, source)?;
            require(!loading_capability(name), "destructured loading capabilities need a separately registered architecture checker")?;
        }
    }
    Ok(())
}

pub(super) fn parse(path: &str, source: &str, python: bool) -> Result<Vec<Import>> {
    let grammar = if python {
        tree_sitter_python::LANGUAGE.into()
    } else if path.ends_with(".tsx") {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else if [".ts", ".mts", ".cts"]
        .iter()
        .any(|ext| path.ends_with(ext))
    {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    } else {
        tree_sitter_javascript::LANGUAGE.into()
    };
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|e| Error::new(e.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::new("import parser did not complete".into()))?;
    require(
        !tree.root_node().has_error(),
        format!("invalid or unsupported syntax in {path}"),
    )?;
    if !python {
        javascript_capabilities(tree.root_node(), source)?;
    }
    let mut sys_aliases = std::collections::BTreeSet::new();
    if python {
        let mut pending = vec![tree.root_node()];
        let mut count = 0;
        while let Some(node) = pending.pop() {
            count += 1;
            require(count <= 200_000, "import syntax exceeds analysis limits")?;
            if node.kind() == "import_statement" {
                for item in named(node) {
                    if imported_name(item, source)? == "sys" {
                        sys_aliases.insert(if item.kind() == "aliased_import" {
                            text(field(item, "alias")?, source)?
                        } else {
                            "sys"
                        });
                    }
                }
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
    }
    let mut imports = Vec::new();
    let mut pending = vec![(tree.root_node(), 0)];
    let mut seen = 0;
    while let Some((node, depth)) = pending.pop() {
        seen += 1;
        require(
            depth < 256 && seen <= 200_000,
            "import syntax exceeds analysis limits",
        )?;
        let mut add = |module: String, member, require| {
            imports.push(Import {
                source: path.into(),
                module,
                member,
                require,
                runtime_source: None,
            })
        };
        if python {
            match node.kind() {
                "import_statement" => {
                    for item in named(node) {
                        add(imported_name(item, source)?.into(), None, false);
                    }
                }
                "import_from_statement" => {
                    let module = field(node, "module_name")?;
                    let mut cursor = node.walk();
                    for item in node.children_by_field_name("name", &mut cursor) {
                        add(
                            text(module, source)?.into(),
                            Some(imported_name(item, source)?.into()),
                            false,
                        );
                    }
                }
                "wildcard_import" => {
                    return Err(Error::new(format!(
                        "Python wildcard imports need an explicit module list: {path}"
                    )))
                }
                "identifier" => {
                    let value = text(node, source)?;
                    if sys_aliases.contains(value) {
                        let mut parent = node.parent();
                        let mut declaration = false;
                        while let Some(next) = parent {
                            if next.kind() == "import_statement" {
                                declaration = true;
                                break;
                            }
                            parent = next.parent();
                        }
                        if !declaration {
                            let parent = node
                                .parent()
                                .ok_or_else(|| Error::new("invalid sys reference".into()))?;
                            require(
                                parent.kind() == "attribute"
                                    && parent.child_by_field_name("object") == Some(node),
                                "aliased sys import machinery is unsupported",
                            )?;
                            let attribute = text(field(parent, "attribute")?, source)?;
                            require(
                                !matches!(
                                    attribute,
                                    "path"
                                        | "meta_path"
                                        | "path_hooks"
                                        | "path_importer_cache"
                                        | "modules"
                                        | "__dict__"
                                        | "__getattribute__"
                                ),
                                "custom Python import machinery is unsupported",
                            )?;
                        }
                    }
                    require(
                        !matches!(
                            text(node, source)?,
                            "__import__"
                                | "eval"
                                | "exec"
                                | "compile"
                                | "__builtins__"
                                | "__loader__"
                                | "__path__"
                                | "__package__"
                                | "__spec__"
                                | "getattr"
                        ),
                        format!("dynamic Python loading/reflection is unsupported in {path}"),
                    )?;
                }
                _ => {}
            }
        } else {
            if matches!(
                node.kind(),
                "import_statement" | "export_statement" | "import_require_clause"
            ) {
                if let Some(value) = node.child_by_field_name("source") {
                    add(
                        string(value, source)?,
                        None,
                        node.kind() == "import_require_clause",
                    );
                }
            }
            if node.kind() == "call_expression" {
                let function = field(node, "function")?;
                let name = text(function, source)?;
                if function.kind() == "import" || name == "require" {
                    let arguments = named(field(node, "arguments")?);
                    require(
                        arguments.len() == 1,
                        "import attributes and computed loading are unsupported",
                    )?;
                    add(string(arguments[0], source)?, None, name == "require");
                }
            }
            if matches!(node.kind(), "identifier" | "property_identifier") {
                let name = text(node, source)?;
                require(
                    !matches!(
                        name,
                        "eval"
                            | "Function"
                            | "createRequire"
                            | "getBuiltinModule"
                            | "registerHooks"
                            | "getOwnPropertyDescriptor"
                            | "getOwnPropertyDescriptors"
                            | "getPrototypeOf"
                            | "__proto__"
                    ),
                    format!("dynamic JavaScript loading is unsupported in {path}"),
                )?;
                if name == "require" {
                    require(
                        node.parent().is_some_and(|parent| {
                            parent.kind() == "call_expression"
                                && parent.child_by_field_name("function") == Some(node)
                        }),
                        "aliased or reflective require is unsupported",
                    )?;
                }
            }
            // Triple-slash references can introduce dependencies without import declarations.
            if node.kind() == "comment" && text(node, source)?.trim_start().starts_with("///") {
                require(
                    !text(node, source)?.contains("<reference"),
                    "TypeScript reference directives need a separately registered checker",
                )?;
            }
        }
        let mut cursor = node.walk();
        pending.extend(
            node.named_children(&mut cursor)
                .map(|child| (child, depth + 1)),
        );
    }
    for import in &imports {
        require(!import.module.is_empty(), "empty import specifier")?;
        if python {
            require(
                !(import.module == "sys"
                    && import.member.as_deref().is_some_and(|name| {
                        matches!(
                            name,
                            "path" | "meta_path" | "path_hooks" | "path_importer_cache" | "modules"
                        )
                    })),
                "custom Python import machinery is unsupported",
            )?;
            // These facilities can replace import resolution or load arbitrary paths.
            require(
                !matches!(
                    import.module.split('.').next(),
                    Some("importlib" | "runpy" | "builtins")
                ),
                format!(
                    "custom Python import machinery is unsupported: {}",
                    import.module
                ),
            )?;
        }
    }
    Ok(imports)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the Node.js CLI"]
    fn native_javascript_reflection_can_load_modules_but_cannot_claim_a_static_graph() {
        for source in [
            "const loaded = await globalThis['eval'](\"import('data:text/javascript,export default 42')\"); console.log(loaded.default)",
            "const load = (async()=>{}).constructor(\"return import('data:text/javascript,export default 42')\"); console.log((await load()).default)",
        ] {
            let result = std::process::Command::new("node").args(["--input-type=module", "-e", source]).output().unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), "42");
            assert!(parse("domain.mjs", source, false).is_err());
        }
    }
    #[test]
    fn reflected_loading_and_extracted_constructors_are_not_passing_empty_graphs() {
        for source in [
            "globalThis['eval'](\"import('./infra.mjs')\")",
            "globalThis['e'+'val'](\"import('./infra.mjs')\")",
            "const key = 'eval'; globalThis[key]('code')",
            "const globalAlias = globalThis; globalAlias['eval']('code')",
            "const load = (async()=>{}).constructor(\"return import('./infra.mjs')\"); load()",
            "const key = 'constructor'; const load = fn[key]; load('code')",
            "const {constructor: load} = fn; load('code')",
            "const {constructor} = fn; constructor('code')",
            "const {['constructor']: load} = fn; load('code')",
            "const {globalThis: g} = globalThis; g['eval']('code')",
            "const load = fn['con'+'structor']; load('code')",
            "const key='label'; {const key='constructor'; fn[key]('code');}",
            "let key='label'; key='constructor'; fn[key]('code')",
            "const key=readKey(); const load=fn[key]; const alias=load; alias('code')",
            "const key=readKey(); fn[key]('code')",
            "Reflect.get(fn, key)('code')",
        ] {
            assert!(parse("domain.mjs", source, false).is_err(), "{source}");
        }
        for source in [
            "class Model { constructor(value) { this.value=value; } }",
            "const key = 'label'; const value = data[key]; const row = rows[index];",
            "window.document.title = 'Ready'; globalThis['console'].log('Ready');",
        ] {
            assert!(parse("ordinary.mjs", source, false).is_ok(), "{source}");
        }
    }
    #[test]
    fn declarations_reexports_type_imports_and_literal_loading_are_dependencies() {
        let imports = parse("domain.ts", "import type {A} from './types.js'; export * from './barrel.js'; const x = import('./lazy.js'); const y = require('./legacy.cjs');", false).unwrap();
        for name in ["./types.js", "./barrel.js", "./lazy.js", "./legacy.cjs"] {
            assert!(imports.iter().any(|i| i.module == name));
        }
        assert!(
            imports
                .iter()
                .find(|i| i.module == "./legacy.cjs")
                .unwrap()
                .require
        );
        assert!(parse(
            "file.js",
            "// import './fake.js'\nconst text = \"import './fake.js'\";",
            false
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            parse(
                "module.py",
                "import package.one as one, package.two\nfrom .helpers import value as v\n",
                true
            )
            .unwrap()
            .len(),
            3
        );
        let types = parse("types.ts", "import { type A } from './inline.js'; type B = import('./query.js').B; import Legacy = require('./legacy.cjs');", false).unwrap();
        for name in ["./inline.js", "./query.js", "./legacy.cjs"] {
            assert!(types.iter().any(|i| i.module == name));
        }
        assert!(parse(
            "view.tsx",
            "import { View } from './view.js'; export const screen = <View/>;",
            false
        )
        .is_ok());
    }
    #[test]
    fn ordinary_sys_access_keeps_its_import_without_allowing_search_path_mutation() {
        let imports = parse(
            "a.py",
            "import sys as system\nversion = system.version\nfrom sys import exit\n",
            true,
        )
        .unwrap();
        assert_eq!(imports.len(), 2);
        assert!(imports.iter().all(|import| import.module == "sys"));
    }

    #[test]
    fn unsupported_loading_never_becomes_an_empty_passing_graph() {
        for source in [
            "import(name)",
            "const load = require;",
            "require.resolve('x')",
            "import('\\u0061')",
            "eval('code')",
            "/// <reference path='x.ts' />",
            "import('",
        ] {
            assert!(parse("a.ts", source, false).is_err(), "{source}");
        }
        for source in [
            "from a import *",
            "from importlib import import_module as load",
            "__import__('x')",
            "exec('code')",
            "import sys as s; s.path.append('infra')",
            "from sys import modules",
            "__path__ = ['../infrastructure']",
            "__package__ = 'infrastructure'",
            "__spec__.submodule_search_locations.append('../infrastructure')",
            "import (",
        ] {
            assert!(parse("a.py", source, true).is_err(), "{source}");
        }
    }
}
