//! Repository discovery reads immutable bytes and never runs repository setup scripts.

use crate::{
    identity::Digest,
    ledger::Ledger,
    profile::{Language, Profile},
    require,
    snapshot::Snapshot,
    wire, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One observed build root; detection is a suggestion, not a resolved target profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildUnit {
    /// Repository-relative directory; empty means repository root.
    pub root: String,
    /// Manifest which established this unit.
    pub manifest: String,
    /// Languages suggested by manifests and nearby source.
    pub languages: BTreeSet<Language>,
    /// Present lockfiles, still requiring immutable dependency preparation.
    pub locks: Vec<String>,
}

/// Exact input inventory, including mixed projects such as Tauri.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// Exactly 1.
    pub schema_version: u32,
    /// Every observation belongs to this immutable source snapshot.
    pub snapshot_digest: Digest,
    /// Independently configurable build roots.
    pub units: Vec<BuildUnit>,
    /// Required operator facts, never silently inferred from file extensions.
    pub unresolved: Vec<String>,
}

/// Inspect all nested manifests without modifying or executing the checkout.
pub fn inspect(ledger: &Ledger, snapshot: &Snapshot) -> Result<Inventory> {
    snapshot.validate(ledger, true)?;
    let mut units = Vec::new();
    for (path, entry) in &snapshot.files {
        let (root, name) = path.rsplit_once('/').unwrap_or(("", path));
        let joined = |name: &str| {
            if root.is_empty() {
                name.to_string()
            } else {
                format!("{root}/{name}")
            }
        };
        let language = match name {
            "Cargo.toml" => Some(Language::Rust),
            "package.json" => {
                let manifest: serde_json::Value =
                    wire::decode(&ledger.read(&entry.content.digest)?)?;
                require(manifest.is_object(), "package manifest is not an object")?;
                Some(
                    if snapshot.files.contains_key(&joined("tsconfig.json"))
                        || manifest.pointer("/devDependencies/typescript").is_some()
                        || manifest.pointer("/dependencies/typescript").is_some()
                    {
                        Language::TypeScript
                    } else {
                        Language::JavaScript
                    },
                )
            }
            "pyproject.toml" => Some(Language::Python),
            "requirements.lock" if !snapshot.files.contains_key(&joined("pyproject.toml")) => {
                Some(Language::Python)
            }
            "CMakeLists.txt" => Some(Language::C),
            _ => None,
        };
        let Some(language) = language else { continue };
        let mut languages = BTreeSet::from([language]);
        if name == "CMakeLists.txt" {
            languages.insert(Language::Cpp);
        }
        let locks = [
            "Cargo.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "requirements.lock",
            "uv.lock",
        ]
        .iter()
        .map(|name| joined(name))
        .filter(|path| snapshot.files.contains_key(path))
        .collect();
        units.push(BuildUnit {
            root: root.into(),
            manifest: path.clone(),
            languages,
            locks,
        });
    }
    let mut unresolved = vec!["Select explicit target/runtime facts, pinned toolchain and offline dependencies for every required build root.".into()];
    if units.is_empty() {
        unresolved.push("No supported manifest was found; register the project's native build commands explicitly.".into());
    }
    if snapshot
        .files
        .keys()
        .any(|p| p.ends_with("tauri.conf.json") || p.ends_with("Tauri.toml"))
    {
        unresolved.push("Tauri requires frontend, Rust backend and protected IPC/public-consumer checks; choose bundler/browser capabilities explicitly.".into());
    }
    Ok(Inventory {
        schema_version: 1,
        snapshot_digest: snapshot.digest()?,
        units,
        unresolved,
    })
}

/// Operator-selected profile for one observed build root.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSelection {
    /// Manifest from the immutable inventory.
    pub manifest: String,
    /// Explicit, fully pinned language/runtime facts.
    pub profile: Profile,
}

/// Resolve explicit selections against observed roots and registered immutable pins.
pub fn resolve(
    ledger: &Ledger,
    inventory: &Inventory,
    selections: &[ProfileSelection],
) -> Result<Vec<Profile>> {
    require(
        inventory.schema_version == 1,
        "unsupported repository inventory",
    )?;
    require(!selections.is_empty(), "no profiles selected")?;
    crate::unique(selections.iter().map(|s| s.profile.id.as_str()))?;
    for selection in selections {
        let unit = inventory
            .units
            .iter()
            .find(|u| u.manifest == selection.manifest)
            .ok_or_else(|| {
                crate::Error::new("selected manifest is absent from inventory".into())
            })?;
        require(
            unit.languages.contains(&selection.profile.language),
            "profile language differs from observed manifest",
        )?;
        require(
            selection.profile.build_root == unit.root,
            "profile build root differs from registered manifest",
        )?;
        selection.profile.require_resolved()?;
        if let Some(pin) = &selection.profile.toolchain_lock {
            ledger.copy_to(pin, std::io::sink())?;
        }
        if let Some(pin) = &selection.profile.dependency_bundle {
            let snapshot: Snapshot = wire::decode(&ledger.read(pin)?)?;
            snapshot.validate(ledger, true)?;
        }
    }
    require(inventory.units.iter().all(|u| selections.iter().any(|s| s.manifest == u.manifest)),
        "an observed build root is unresolved; register a scoped exclusion instead of silently omitting it")?;
    Ok(selections.iter().map(|s| s.profile.clone()).collect())
}
