//! Offline runtime preparation and native test execution inside the same sandbox.
use super::{args, checked, checked_source, copy_tree, read, run};
use crate::{
    profile::Language,
    require,
    runtime::{TestInput, TestOutput},
    Error, Result,
};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

pub(super) const PROJECT: &str = "/build/project";
pub(super) const TYPESCRIPT_CONFIG: &str = ".rovik-verification.tsconfig.json";
pub(super) const NODE: &str = "/usr/local/bin/node";
const NPM: &str = "/usr/local/lib/node_modules/npm/bin/npm-cli.js";
const TSC: &str = "/usr/local/lib/node_modules/typescript/bin/tsc";
pub(super) const PYTHON: &str = "/usr/bin/python3";

fn version(command: &[&str], prefix: &str) -> Result<String> {
    let text = checked(&args(command), Path::new("/build"))?;
    Ok(text
        .trim()
        .strip_prefix(prefix)
        .ok_or_else(|| Error::new("malformed runtime version output".into()))?
        .to_string())
}
pub(super) fn versions(language: Language) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    if language == Language::Python {
        result.insert(
            "python_version".into(),
            version(&[PYTHON, "-I", "--version"], "Python ")?,
        );
        result.insert(
            "pytest_version".into(),
            version(&[PYTHON, "-I", "-m", "pytest", "--version"], "pytest ")?,
        );
    } else {
        result.insert("node_version".into(), version(&[NODE, "--version"], "v")?);
        result.insert(
            "npm_version".into(),
            version(&[NODE, NPM, "--version"], "")?,
        );
        if language == Language::TypeScript {
            result.insert(
                "typescript_version".into(),
                version(&[NODE, TSC, "--version"], "Version ")?,
            );
        }
    }
    Ok(result)
}
pub(crate) fn validate_node_lock(manifest: &Value, lock: &Value) -> Result<()> {
    require(
        manifest.get("type").and_then(Value::as_str) == Some("module"),
        "Node profile requires an explicit ESM package",
    )?;
    require(
        lock.get("lockfileVersion").and_then(Value::as_u64) == Some(3),
        "npm lockfile v3 is required",
    )?;
    let packages = lock
        .get("packages")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::new("npm lockfile has no package inventory".into()))?;
    require(packages.contains_key(""), "npm lockfile lacks root package")?;
    for (name, package) in packages {
        if name.is_empty() {
            continue;
        }
        require(
            package.get("link").and_then(Value::as_bool) != Some(true)
                && package
                    .get("integrity")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("sha512-")),
            "runtime dependencies require locked integrity; workspace links are unsupported",
        )?;
    }
    Ok(())
}

fn stage_typescript_project(oracle: &Path, root: &Path) -> Result<std::path::PathBuf> {
    let bytes = std::fs::read(oracle)?;
    let config: Value = crate::wire::decode(&bytes)?;
    require(
        config.is_object() && config.get("extends").is_none() && config.get("references").is_none(),
        "Use a self-contained protected TypeScript JSON project without extends or references",
    )?;
    // Resolve source paths and ambient packages from the prepared build root.
    // Never overwrite candidate files or let them replace the approved config.
    let path = root.join(TYPESCRIPT_CONFIG);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    Ok(path)
}

fn prepare_node(typescript_project: Option<&str>, source_root: &Path) -> Result<()> {
    let root = Path::new(PROJECT);
    let manifest: Value = read(root.join("package.json"))?;
    let lock: Value = read(root.join("package-lock.json"))?;
    validate_node_lock(&manifest, &lock)?;
    if Path::new("/dependencies/npm").is_dir() {
        copy_tree(
            Path::new("/dependencies/npm"),
            Path::new("/build/npm-cache"),
        )?;
    }
    std::fs::write("/build/npm-user.rc", "")?;
    std::fs::write("/build/npm-global.rc", "")?;
    checked(
        &args(&[
            NODE,
            NPM,
            "ci",
            "--offline",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--cache=/build/npm-cache",
            "--userconfig=/build/npm-user.rc",
            "--globalconfig=/build/npm-global.rc",
        ]),
        root,
    )?;
    if let Some(typescript_project) = typescript_project {
        require(
            !Path::new("/build/project/dist").exists(),
            "TypeScript output must be generated in a fresh directory",
        )?;
        let project =
            stage_typescript_project(&Path::new("/oracle").join(typescript_project), root)?;
        // These flags cannot be weakened by repository tsconfig files. The project
        // itself is an approved oracle asset, including its source selection.
        checked_source(
            &args(&[
                NODE,
                TSC,
                "--project",
                &project.to_string_lossy(),
                "--strict",
                "--noEmitOnError",
                "--noCheck",
                "false",
                "--noEmit",
                "false",
                "--emitDeclarationOnly",
                "false",
                "--incremental",
                "false",
                "--composite",
                "false",
                "--rootDir",
                PROJECT,
                "--outDir",
                "/build/project/dist",
            ]),
            root,
        )?;
    } else {
        let mut files = BTreeMap::new();
        super::imports::sources(source_root, source_root, false, &mut files, &mut 0)?;
        for path in files.keys().filter(|path| {
            [".js", ".mjs", ".cjs"]
                .iter()
                .any(|extension| path.ends_with(extension))
        }) {
            checked_source(
                &args(&[NODE, "--check", &format!("{PROJECT}/{path}")]),
                root,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn validate_python_lock(lock: &str) -> Result<()> {
    for line in lock
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        let mut fields = line.split_whitespace();
        let requirement = fields
            .next()
            .ok_or_else(|| Error::new("empty Python requirement".into()))?;
        let (name, version) = requirement
            .split_once("==")
            .ok_or_else(|| Error::new("Python requirements must pin exact versions".into()))?;
        let safe = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-+!".contains(&b))
        };
        require(
            safe(name)
                && name
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && name
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                && safe(version),
            "unsupported Python requirement; use one pinned wheel per line",
        )?;
        let hashes: Vec<_> = fields.collect();
        require(
            !hashes.is_empty()
                && hashes.iter().all(|h| {
                    h.strip_prefix("--hash=sha256:")
                        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                }),
            "Python requirements need SHA-256 hashes and cannot add pip options",
        )?;
    }
    Ok(())
}
fn prepare_python() -> Result<()> {
    let root = Path::new(PROJECT);
    let lock = std::fs::read_to_string(root.join("requirements.lock"))?;
    require(
        lock.len() <= crate::wire::MAX_JSON_BYTES,
        "Python lock exceeds limit",
    )?;
    validate_python_lock(&lock)?;
    // A dedicated target avoids global installs and preserves the pinned pytest.
    checked(
        &args(&[
            PYTHON,
            "-I",
            "-m",
            "pip",
            "install",
            "--no-index",
            "--require-hashes",
            "--only-binary=:all:",
            "--no-compile",
            "--no-cache-dir",
            "--disable-pip-version-check",
            "--find-links=/dependencies/wheels",
            "--target=/build/site",
            "-r",
            "requirements.lock",
        ]),
        root,
    )?;
    checked_source(
        &args(&[PYTHON, "-I", "-m", "compileall", "-q", "-f", PROJECT]),
        root,
    )?;
    Ok(())
}

/// Prepare immutable dependencies without executing project modules or tests.
pub(super) fn prepare(
    profile: &crate::profile::Profile,
    typescript_project: Option<&str>,
) -> Result<BTreeMap<String, String>> {
    let language = profile.language;
    let observed = versions(language)?;
    if !observed
        .iter()
        .all(|(key, value)| profile.facts.get(key) == Some(value))
    {
        return Err(Error::classified(
            crate::FailureCategory::Capability,
            "installed runtime/tool versions differ from approved profile",
        ));
    }
    require(
        !Path::new(PROJECT).exists(),
        "runtime build must start in a fresh directory",
    )?;
    let source_root = Path::new("/snapshot").join(&profile.build_root);
    copy_tree(&source_root, Path::new(PROJECT))?;
    if language == Language::Python {
        prepare_python()?;
    } else {
        prepare_node(typescript_project, &source_root)?;
    }
    Ok(observed)
}

pub(super) fn check(input: &TestInput) -> Result<TestOutput> {
    let config = input.validate()?;
    let language = input.profile.language;
    let observed = prepare(&input.profile, input.typescript_project.as_deref())?;
    let mut argv = if language == Language::Python {
        // -I excludes PYTHONPATH/sitecustomize injection before pytest starts.
        // Configuration, plugins and conftest discovery cannot come from the candidate.
        std::fs::write("/build/pytest.ini", "[pytest]\n")?;
        args(&[
            PYTHON,
            "-I",
            "-m",
            "pytest",
            "-c",
            "/build/pytest.ini",
            "--noconftest",
            "--confcutdir=/oracle",
            "--override-ini=pythonpath=/build/project /build/site",
            "--override-ini=addopts=",
            "--junitxml=/outputs/pytest.xml",
            "-q",
        ])
    } else {
        args(&[NODE, "--test", "--test-reporter=junit"])
    };
    argv.extend(input.test_files.iter().map(|s| format!("/oracle/{s}")));
    let output = run(
        &argv,
        Path::new(PROJECT),
        &[
            ("PYTEST_DISABLE_PLUGIN_AUTOLOAD", "1"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("PIP_CONFIG_FILE", "/dev/null"),
        ],
    )?;
    require(
        matches!(output.code, Some(0 | 1)),
        "runtime discovery, collection or test execution did not complete",
    )?;
    let report = if language == Language::Python {
        std::fs::read_to_string("/outputs/pytest.xml")?
    } else {
        String::from_utf8(output.stdout).map_err(|e| Error::new(e.to_string()))?
    };
    require(
        report.len() <= crate::wire::MAX_JSON_BYTES,
        "runtime report exceeds limit",
    )?;
    Ok(TestOutput {
        language,
        configuration_digest: config.digest()?,
        versions: observed,
        exit_code: output
            .code
            .ok_or_else(|| Error::new("runtime process was killed".into()))?,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires ROVIK_TYPESCRIPT_COMPILER pointing to the pinned TypeScript CLI"]
    fn real_typescript_resolves_ambient_packages_from_the_prepared_build_root() {
        let compiler = std::env::var("ROVIK_TYPESCRIPT_COMPILER").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let oracle = dir.path().join("oracle/web/tsconfig.json");
        std::fs::create_dir_all(oracle.parent().unwrap()).unwrap();
        for package in ["@types/node", "vite"] {
            std::fs::create_dir_all(root.join("node_modules").join(package)).unwrap();
        }
        std::fs::write(
            root.join("node_modules/@types/node/index.d.ts"),
            "declare const nodeType: string;",
        )
        .unwrap();
        std::fs::write(
            root.join("node_modules/vite/client.d.ts"),
            "declare const viteType: string;",
        )
        .unwrap();
        std::fs::write(
            root.join("main.ts"),
            "export const value = nodeType + viteType;",
        )
        .unwrap();
        let config = serde_json::json!({"compilerOptions":{"types":["vite/client","node"],"noEmit":true},"files":[root.join("main.ts")]});
        std::fs::write(&oracle, serde_json::to_vec(&config).unwrap()).unwrap();
        let compile = |path: &Path| {
            std::process::Command::new("node")
                .arg(&compiler)
                .arg("--project")
                .arg(path)
                .current_dir(&root)
                .output()
                .unwrap()
        };
        let before = compile(&oracle);
        assert!(!before.status.success());
        assert!(String::from_utf8_lossy(&before.stdout).contains("TS2688"));
        let staged = stage_typescript_project(&oracle, &root).unwrap();
        let after = compile(&staged);
        assert!(
            after.status.success(),
            "{}{}",
            String::from_utf8_lossy(&after.stdout),
            String::from_utf8_lossy(&after.stderr)
        );
        std::fs::remove_file(root.join("node_modules/vite/client.d.ts")).unwrap();
        assert!(
            !compile(&staged).status.success(),
            "missing declared types must still fail"
        );
    }

    #[test]
    fn protected_typescript_config_is_staged_beside_dependencies_without_overwriting_sources() {
        let dir = tempfile::tempdir().unwrap();
        let oracle = dir.path().join("oracle.json");
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let config = br#"{"compilerOptions":{"types":["vite/client","node"]},"include":["src"]}"#;
        std::fs::write(&oracle, config).unwrap();
        let path = stage_typescript_project(&oracle, &root).unwrap();
        assert_eq!(path.parent(), Some(root.as_path()));
        assert_eq!(std::fs::read(&path).unwrap(), config);
        assert!(stage_typescript_project(&oracle, &root).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), config);
        for config in [r#"{"extends":"./candidate.json"}"#, r#"{"references":[]}"#] {
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&oracle, config).unwrap();
            assert!(stage_typescript_project(&oracle, &root).is_err());
            assert!(!path.exists());
            std::fs::write(&path, "reserved").unwrap();
        }
    }

    #[test]
    fn python_locks_require_pinned_hashed_wheels_and_reject_options_and_urls() {
        validate_python_lock("").unwrap();
        validate_python_lock(&format!("example==1.0.0 --hash=sha256:{}", "a".repeat(64))).unwrap();
        for name in [
            "--trusted-host",
            "--index-url",
            ".relative",
            "trailing-",
            "invalid+name",
        ] {
            assert!(
                validate_python_lock(&format!("{name}==1.0.0 --hash=sha256:{}", "a".repeat(64)))
                    .is_err(),
                "{name}"
            );
        }
        for bad in [
            "example>=1",
            "example==1",
            "--index-url https://example.test",
            "./package.whl",
            "example @ https://example.test/a.whl",
            "example==1 --no-binary=:all:",
        ] {
            assert!(validate_python_lock(bad).is_err());
        }
    }
}
