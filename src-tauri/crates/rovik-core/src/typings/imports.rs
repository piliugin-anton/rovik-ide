//! What a file names of other packages: the module specifiers of its
//! imports, as text, without parsing. A regex over the file is enough to
//! know which packages to load, and a specifier in a comment or a string
//! costs at most a package nothing imports. TypeScript resolves the real
//! imports itself once the files are there.
//!
//! Also the seeds a config and a manifest give: the packages a tsconfig's
//! `types` and `jsxImportSource` name, and a package.json's runtime
//! dependencies, which the code is about to import if it doesn't yet.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The imports of one file, for `load_dependencies`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ImportSite {
    /// The importing file's absolute path: resolution starts in its folder.
    pub from: String,
    pub specifiers: Vec<String>,
}

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `from 'x'` (import, export ... from), `import('x')`, `require('x')`,
        // `import 'x'` (for its side effects), `/// <reference types="x" />`.
        Regex::new(
            r#"(?x)
            \bfrom\s*['"]([^'"\n]+)['"]
            | \bimport\s*\(\s*['"]([^'"\n]+)['"]
            | \brequire\s*\(\s*['"]([^'"\n]+)['"]
            | \bimport\s+['"]([^'"\n]+)['"]
            | <reference\s+types\s*=\s*['"]([^'"\n]+)['"]
            "#,
        )
        .expect("a valid import pattern")
    })
}

/// The bare specifiers (packages, not relative or absolute paths) a
/// source or declaration file imports, each once, in order.
pub fn specifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for captures in pattern().captures_iter(text) {
        let Some(spec) = (1..=5).find_map(|i| captures.get(i)).map(|m| m.as_str()) else {
            continue;
        };
        if is_bare(spec) && !out.iter().any(|s| s == spec) {
            out.push(spec.to_string());
        }
    }
    out
}

/// A specifier that names a package rather than a file: not relative, not
/// absolute, not a URL or a bundler's virtual module, not a package.json
/// `imports` alias (`#x`).
pub fn is_bare(spec: &str) -> bool {
    let first = spec.chars().next();
    !(spec.is_empty()
        || matches!(
            first,
            Some('.') | Some('/') | Some('#') | Some('\\') | Some('\0')
        )
        || spec.contains("://")
        || spec.starts_with("virtual:")
        || spec.contains('*')
        || drive_letter(spec))
}

/// `C:\x` or `C:/x`.
fn drive_letter(spec: &str) -> bool {
    let bytes = spec.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// The package a bare specifier names: `@scope/name` for `@scope/name/x`,
/// `name` for `name/x`. None for a specifier no package can be found for
/// (`@/x`: an alias).
pub fn package_name(spec: &str) -> Option<String> {
    let mut parts = spec.split('/');
    let first = parts.next().filter(|p| !p.is_empty())?;
    if let Some(scope) = first.strip_prefix('@') {
        let name = parts.next().filter(|p| !p.is_empty())?;
        if scope.is_empty() {
            return None;
        }
        return Some(format!("@{scope}/{name}"));
    }
    Some(first.to_string())
}

/// The packages that may hold a specifier's declarations, in the order
/// TypeScript prefers them: the package itself, then its `@types`. A Node
/// built-in (`node:fs`, and `fs` itself) is typed by `@types/node`.
pub fn candidates(spec: &str) -> Vec<String> {
    if !is_bare(spec) {
        return Vec::new();
    }
    if let Some(rest) = spec.strip_prefix("node:") {
        return if rest.is_empty() {
            Vec::new()
        } else {
            vec!["@types/node".into()]
        };
    }
    let Some(name) = package_name(spec) else {
        return Vec::new();
    };
    let mut out = vec![name.clone()];
    if NODE_BUILTINS.contains(&name.as_str()) {
        out.push("@types/node".into());
    } else if let Some(types) = types_package(&name) {
        out.push(types);
    }
    out
}

/// Where DefinitelyTyped keeps a package's declarations: `@types/name`,
/// `@types/scope__name` for a scoped one.
pub fn types_package(name: &str) -> Option<String> {
    if name.starts_with("@types/") {
        return None;
    }
    Some(match name.strip_prefix('@') {
        Some(scoped) => format!("@types/{}", scoped.replacen('/', "__", 1)),
        None => format!("@types/{name}"),
    })
}

/// The packages a tsconfig/jsconfig names: its `types` (each resolved like
/// `/// <reference types>`), its `jsxImportSource`, and React for the
/// `react-jsx` transforms without one. Read with a regex, as the files
/// have comments and trailing commas.
pub fn config_seeds(text: &str) -> Vec<String> {
    static TYPES: OnceLock<Regex> = OnceLock::new();
    static STRING: OnceLock<Regex> = OnceLock::new();
    static JSX: OnceLock<Regex> = OnceLock::new();
    let types = TYPES.get_or_init(|| Regex::new(r#""types"\s*:\s*\[([^\]]*)\]"#).unwrap());
    let string = STRING.get_or_init(|| Regex::new(r#""([^"\n]+)""#).unwrap());
    let jsx =
        JSX.get_or_init(|| Regex::new(r#""(jsxImportSource|jsx)"\s*:\s*"([^"\n]+)""#).unwrap());
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if is_bare(s) && !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };
    for list in types.captures_iter(text) {
        for name in string.captures_iter(&list[1]) {
            push(&name[1]);
        }
    }
    for found in jsx.captures_iter(text) {
        match &found[1] {
            "jsxImportSource" => push(&found[2]),
            _ if found[2].starts_with("react-jsx") => push("react"),
            _ => {}
        }
    }
    out
}

/// The runtime dependencies a package.json declares (`dependencies`,
/// `peerDependencies`): what the code imports, or is about to. Not the dev
/// ones, which are mostly tools (and the `@types` among them are taken in
/// as a whole anyway).
pub fn manifest_seeds(text: &str) -> Vec<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for field in ["dependencies", "peerDependencies"] {
        let Some(deps) = json.get(field).and_then(|d| d.as_object()) else {
            continue;
        };
        for name in deps.keys() {
            if is_bare(name) && !out.iter().any(|x| x == name) {
                out.push(name.clone());
            }
        }
    }
    out
}

/// Node's built-in modules, importable without the `node:` prefix.
const NODE_BUILTINS: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "sys",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_kind_of_import_and_keeps_the_bare_ones() {
        let text = r#"
            import React, { useState } from "react";
            import type { A } from '@scope/pkg/sub';
            import './styles.css';
            import 'reflect-metadata';
            export * from "re-exported";
            export { x } from
              'next-line';
            const l = require('lodash/fp');
            const d = await import("dynamic");
            /// <reference types="vite/client" />
            import x = require("legacy");
            import abs from '/abs/path';
            import alias from '#alias';
            import url from 'https://esm.sh/x';
            import v from 'virtual:pwa';
            import again from "react";
        "#;
        assert_eq!(
            specifiers(text),
            [
                "react",
                "@scope/pkg/sub",
                "reflect-metadata",
                "re-exported",
                "next-line",
                "lodash/fp",
                "dynamic",
                "vite/client",
                "legacy",
            ]
        );
    }

    #[test]
    fn package_names_of_specifiers() {
        assert_eq!(package_name("react").as_deref(), Some("react"));
        assert_eq!(package_name("lodash/fp").as_deref(), Some("lodash"));
        assert_eq!(
            package_name("@scope/pkg/sub").as_deref(),
            Some("@scope/pkg")
        );
        assert_eq!(candidates("node:fs/promises"), ["@types/node"]);
        assert_eq!(candidates("fs"), ["fs", "@types/node"]);
        assert_eq!(
            candidates("@babel/core/x"),
            ["@babel/core", "@types/babel__core"]
        );
        assert_eq!(candidates("@types/react"), ["@types/react"]);
        assert!(candidates("@/x").is_empty());
        assert!(candidates("./local").is_empty(), "`.` is not a package");
        assert!(candidates("/abs").is_empty());
        assert_eq!(package_name("@/components"), None);
        assert_eq!(package_name("@scope"), None);
        assert_eq!(types_package("react").as_deref(), Some("@types/react"));
        assert_eq!(
            types_package("@babel/core").as_deref(),
            Some("@types/babel__core")
        );
        assert_eq!(types_package("@types/node"), None);
    }

    #[test]
    fn config_seeds_come_from_types_and_jsx() {
        let text = r#"{
          // comment
          "compilerOptions": {
            "jsx": "react-jsx",
            "types": ["node", "vite/client",],
          },
        }"#;
        assert_eq!(config_seeds(text), ["node", "vite/client", "react"]);
        assert_eq!(
            config_seeds(r#"{"compilerOptions":{"jsx":"preserve","jsxImportSource":"solid-js"}}"#),
            ["solid-js"]
        );
        assert!(config_seeds("{}").is_empty());
    }

    #[test]
    fn manifest_seeds_are_the_runtime_dependencies() {
        let text = r#"{"dependencies":{"solid-js":"1","@tauri-apps/api":"2"},
            "peerDependencies":{"react":"18"},"devDependencies":{"vite":"5"}}"#;
        let mut seeds = manifest_seeds(text);
        seeds.sort();
        assert_eq!(seeds, ["@tauri-apps/api", "react", "solid-js"]);
        assert!(manifest_seeds("not json").is_empty());
    }
}
