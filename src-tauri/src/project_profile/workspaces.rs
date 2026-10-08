//! Workspace expansion for project detection (parent: `project_profile.rs`).
//!
//! Reads workspace membership from manifest files only — no recursion into
//! member folders beyond one `package.json` / `Cargo.toml` read each, capped
//! at [`MAX_WORKSPACE_MEMBERS`] members. Globs are matched against the
//! immediate directory listing, so cost tracks what's listed, not total
//! project size.

use std::fs;
use std::path::Path;

pub const MAX_WORKSPACE_PATTERNS: usize = 8;
pub const MAX_WORKSPACE_MEMBERS: usize = 20;

pub fn detect_package_manager(root: &Path) -> &'static str {
    if root.join("pnpm-lock.yaml").exists() || root.join("pnpm-workspace.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    }
}

pub fn is_yarn_berry(root: &Path, pkg: Option<&serde_json::Value>) -> bool {
    if root.join(".yarnrc.yml").exists() || root.join(".yarnrc.yaml").exists() {
        return true;
    }
    if let Some(pm) = pkg.and_then(|p| p.get("packageManager")).and_then(|v| v.as_str()) {
        // Yarn Berry is 2+. Classic "yarn@1.x" keeps the v1 syntax.
        if pm.starts_with("yarn@") && !pm.starts_with("yarn@1.") && !pm.starts_with("yarn@0.") {
            return true;
        }
    }
    false
}

/// One workspace-wide command that runs `script` in every package that has
/// it. Best effort per package manager; the user can edit it in Project checks.
pub fn workspace_run(pm: &str, script: &str, yarn_berry: bool) -> String {
    match pm {
        "pnpm" => format!("pnpm -r --if-present run {script}"),
        "yarn" if yarn_berry => format!("yarn workspaces foreach -A run {script}"),
        "yarn" => format!("yarn workspaces run {script}"),
        "bun" => format!("bun run --filter '*' {script}"),
        _ if script == "test" => "npm test --workspaces --if-present".to_string(),
        _ => format!("npm run {script} --workspaces --if-present"),
    }
}

/// `*` matches any run of characters except `/` (one path segment).
fn segment_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let mut rest = name;
    let chunks: Vec<&str> = pattern.split('*').collect();
    if !chunks[0].is_empty() {
        if !rest.starts_with(chunks[0]) {
            return false;
        }
        rest = &rest[chunks[0].len()..];
    }
    for (i, chunk) in chunks.iter().enumerate().skip(1) {
        if chunk.is_empty() {
            continue;
        }
        let last = i == chunks.len() - 1;
        if last {
            return rest.len() >= chunk.len() && rest.ends_with(chunk);
        }
        match rest.find(chunk) {
            Some(pos) => rest = &rest[pos + chunk.len()..],
            None => return false,
        }
    }
    true
}

fn expand_parts(
    root: &Path,
    dir: &Path,
    parts: &[&str],
    manifest: &str,
    out: &mut Vec<std::path::PathBuf>,
) {
    if out.len() >= MAX_WORKSPACE_MEMBERS {
        return;
    }
    if parts.is_empty() {
        if dir.join(manifest).is_file() {
            out.push(dir.to_path_buf());
        }
        return;
    }
    let (head, tail) = (parts[0], &parts[1..]);
    if head == "**" {
        expand_parts(root, dir, tail, manifest, out);
        if let Ok(entries) = fs::read_dir(dir) {
            let mut dirs: Vec<_> = entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| !n.starts_with('.') && n != "node_modules" && n != "target")
                })
                .collect();
            dirs.sort_by_key(|e| e.file_name());
            for e in dirs {
                if out.len() >= MAX_WORKSPACE_MEMBERS {
                    return;
                }
                expand_parts(root, &e.path(), parts, manifest, out);
            }
        }
        return;
    }
    if head.contains('*') {
        if let Ok(entries) = fs::read_dir(dir) {
            let mut names: Vec<_> = entries.flatten().collect();
            names.sort_by_key(|e| e.file_name());
            for e in names {
                let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
                if !segment_matches(head, &name) || !e.path().is_dir() || name.starts_with('.') {
                    continue;
                }
                expand_parts(root, &e.path(), tail, manifest, out);
                if out.len() >= MAX_WORKSPACE_MEMBERS {
                    return;
                }
            }
        }
        return;
    }
    if head.is_empty() {
        return;
    }
    let next = dir.join(head);
    if next.is_dir() {
        expand_parts(root, &next, tail, manifest, out);
    }
}

/// Expand workspace glob patterns (e.g. `packages/*`) to project-relative
/// directories holding `manifest` (package.json / Cargo.toml), forward slashes.
pub fn expand_member_dirs(root: &Path, patterns: &[String], manifest: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for pattern in patterns.iter().take(MAX_WORKSPACE_PATTERNS) {
        let clean = pattern.trim().trim_start_matches("./").trim_matches('/').replace('\\', "/");
        if clean.is_empty() || clean == "." || clean.starts_with("..") || clean.starts_with('/') {
            continue;
        }
        let parts: Vec<&str> = clean.split('/').collect();
        let mut found = Vec::new();
        expand_parts(root, root, &parts, manifest, &mut found);
        for path in found {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !rel.is_empty() && !out.contains(&rel) {
                out.push(rel);
            }
            if out.len() >= MAX_WORKSPACE_MEMBERS {
                break;
            }
        }
        if out.len() >= MAX_WORKSPACE_MEMBERS {
            break;
        }
    }
    out.sort();
    out
}

/// `packages:` list of a pnpm-workspace.yaml: includes and `!`-excludes.
fn pnpm_workspace_lists(text: &str) -> (Vec<String>, Vec<String>) {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    let mut in_list = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if !in_list {
            if trimmed == "packages:" {
                in_list = true;
            }
            continue;
        }
        if let Some(item) = trimmed.strip_prefix('-') {
            let mut item = item.trim().trim_matches(|c| c == '\'' || c == '"').trim();
            if let Some(pos) = item.find(" #") {
                item = item[..pos].trim();
            }
            if item.is_empty() || item.starts_with('#') {
                continue;
            }
            if let Some(neg) = item.strip_prefix('!') {
                excludes.push(neg.trim().to_string());
            } else {
                includes.push(item.to_string());
            }
        } else if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        } else {
            break;
        }
    }
    (includes, excludes)
}

/// Workspace glob patterns from package.json `workspaces` and
/// pnpm-workspace.yaml. Returns (includes, excludes).
fn node_workspace_patterns(
    pkg: Option<&serde_json::Value>,
    root: &Path,
) -> (Vec<String>, Vec<String>) {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    if let Some(pkg) = pkg {
        match pkg.get("workspaces") {
            Some(serde_json::Value::Array(arr)) => {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        includes.push(s.to_string());
                    }
                }
            }
            Some(serde_json::Value::Object(obj)) => {
                if let Some(serde_json::Value::Array(arr)) = obj.get("packages") {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            includes.push(s.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if root.join("pnpm-workspace.yaml").exists() {
        if let Ok(text) = fs::read_to_string(root.join("pnpm-workspace.yaml")) {
            let (mut inc, mut exc) = pnpm_workspace_lists(&text);
            includes.append(&mut inc);
            excludes.append(&mut exc);
        }
    }
    includes.sort();
    includes.dedup();
    excludes.sort();
    excludes.dedup();
    (includes, excludes)
}

/// Project-relative member dirs for a Node workspace, excludes applied.
/// Empty when the root declares no workspace patterns.
pub fn node_members(root: &Path, pkg: Option<&serde_json::Value>) -> Vec<String> {
    let (includes, excludes) = node_workspace_patterns(pkg, root);
    if includes.is_empty() {
        return Vec::new();
    }
    let mut members = expand_member_dirs(root, &includes, "package.json");
    if !excludes.is_empty() {
        let skipped = expand_member_dirs(root, &excludes, "package.json");
        members.retain(|m| !skipped.contains(m));
    }
    members
}

/// `(package name, script names)` of one workspace member.
pub fn read_node_member(root: &Path, rel: &str) -> Option<(Option<String>, Vec<String>)> {
    let text = fs::read_to_string(root.join(rel).join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = v.get("name").and_then(|n| n.as_str()).map(str::to_string);
    let mut scripts = Vec::new();
    if let Some(map) = v.get("scripts").and_then(|s| s.as_object()) {
        for (k, v) in map {
            if v.as_str().is_some_and(|s| !s.trim().is_empty()) {
                scripts.push(k.clone());
            }
        }
    }
    scripts.sort();
    Some((name, scripts))
}

/// Quoted strings of `key = [ ... ]` (possibly multi-line) inside
/// `[section]` of a TOML file. None when the section/key isn't there.
/// Line comments are stripped; good enough for member/exclude lists.
pub fn toml_string_array(content: &str, section: &str, key: &str) -> Option<Vec<String>> {
    let header = format!("[{section}]");
    let mut in_section = false;
    let mut collecting = false;
    let mut buf = String::new();
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            if collecting {
                break;
            }
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        if !collecting {
            let flat: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if flat.starts_with(&format!("{key}=")) {
                let after = &flat[key.len() + 1..];
                if after.starts_with('[') {
                    buf.push_str(after);
                    collecting = true;
                    if after.contains(']') {
                        break;
                    }
                }
            }
        } else {
            buf.push_str(line);
            if line.contains(']') {
                break;
            }
        }
    }
    if !collecting {
        return None;
    }
    let mut out = Vec::new();
    let mut chars = buf.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let mut s = String::new();
            for ch in chars.by_ref() {
                if ch == c {
                    break;
                }
                s.push(ch);
            }
            out.push(s);
            if out.len() >= MAX_WORKSPACE_MEMBERS * 2 {
                break;
            }
        }
    }
    Some(out)
}

pub fn toml_has_section(content: &str, section: &str) -> bool {
    let header = format!("[{section}]");
    content.lines().any(|l| l.trim() == header)
}

/// `[package] name = "..."` of a Cargo.toml, when present.
pub fn cargo_package_name(content: &str) -> Option<String> {
    toml_string_value(content, "package", "name")
}

fn toml_string_value(content: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut in_section = false;
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        // key = "value" (whitespace tolerated).
        let mut parts = line.splitn(2, '=');
        let (Some(k), Some(v)) = (parts.next(), parts.next()) else { continue };
        if k.trim() != key {
            continue;
        }
        let v = v.trim().trim_matches(',').trim();
        if v.len() >= 2 && (v.starts_with('"') || v.starts_with('\'')) {
            return Some(v[1..v.len() - 1].to_string());
        }
    }
    None
}

/// Project-relative member dirs of a Cargo workspace manifest.
/// `manifest_dir` is "" for the root Cargo.toml or a subdir like "src-tauri".
/// Falls back to `.` (the manifest itself) when `[workspace]` has no members
/// list but the manifest exists — i.e. a single-package setup.
pub fn cargo_members(root: &Path, manifest_dir: &str, content: &str) -> Vec<String> {
    let base = if manifest_dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(manifest_dir)
    };
    let mut members: Vec<String> = Vec::new();
    if let Some(patterns) = toml_string_array(content, "workspace", "members") {
        members = expand_member_dirs(&base, &patterns, "Cargo.toml")
            .into_iter()
            .map(|rel| {
                if manifest_dir.is_empty() {
                    rel
                } else {
                    format!("{manifest_dir}/{rel}")
                }
            })
            .collect();
        if let Some(excludes) = toml_string_array(content, "workspace", "exclude") {
            let skipped: Vec<String> = expand_member_dirs(&base, &excludes, "Cargo.toml")
                .into_iter()
                .map(|rel| {
                    if manifest_dir.is_empty() {
                        rel
                    } else {
                        format!("{manifest_dir}/{rel}")
                    }
                })
                .collect();
            members.retain(|m| !skipped.contains(m));
        }
        members.sort();
        members.dedup();
        members.truncate(MAX_WORKSPACE_MEMBERS);
    }
    members
}
