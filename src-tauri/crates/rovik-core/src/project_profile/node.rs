//! What the checks know about a Node project (parent: `project_profile.rs`):
//! the stacks its dependencies name, the package.json scripts that run each
//! kind of check, and how to run one so that it checks and ends. Each is a
//! table: to teach the checks another framework, script name or test
//! runner, add a row to it.

/// Dependencies that name the project's stack, in the order they're told.
pub const STACKS: &[(&str, &str)] = &[
    ("react", "React"),
    ("react-native", "React Native"),
    ("expo", "Expo"),
    ("solid-js", "Solid"),
    ("vue", "Vue"),
    ("svelte", "Svelte"),
    ("next", "Next.js"),
    ("vite", "Vite"),
    ("@tauri-apps/api", "Tauri"),
    ("express", "Express"),
];

/// The scripts that run each kind of check, in the order they're looked
/// for: the first the project has is its check ([`pick`]).
pub const CHECK_SCRIPTS: &[(&str, &[&str])] = &[
    (
        "typecheck",
        &["typecheck", "type-check", "check-types", "types", "tsc"],
    ),
    // Ignite's React Native apps: `lint` fixes, `lint:check` checks.
    ("lint", &["lint", "lint:check", "check:lint", "lint:ci"]),
    ("test", &["test"]),
    ("build", &["build"]),
];

/// The script names that run `kind`'s check (none for a kind without).
pub fn check_scripts(kind: &str) -> &'static [&'static str] {
    CHECK_SCRIPTS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(&[], |(_, names)| names)
}

/// Flags that make a checker change the files it checks
/// (`eslint . --fix`, `prettier --write .`).
const REWRITES: &[&str] = &["--fix", "--write"];

/// Whether a script changes the files it checks.
pub fn rewrites_files(body: &str) -> bool {
    body.split_whitespace().any(|w| {
        REWRITES
            .iter()
            .any(|flag| w == *flag || w == format!("{flag}=true"))
    })
}

/// Of `names` (one kind's, [`check_scripts`]), the script that checks:
/// the first the project has (`body` gives a name's text, None when it
/// has none) that doesn't change the files it checks, else the first it
/// has. A check that fixes what it finds would make its own changes, in
/// files the agent never touched.
pub fn pick(names: &[&str], body: impl Fn(&str) -> Option<String>) -> Option<String> {
    let found: Vec<(&str, String)> = names
        .iter()
        .filter_map(|n| body(n).map(|b| (*n, b)))
        .filter(|(_, b)| !b.trim().is_empty())
        .collect();
    found
        .iter()
        .find(|(_, b)| !rewrites_files(b))
        .or(found.first())
        .map(|(n, _)| n.to_string())
}

/// Test runners that keep watching for changes when a flag says so, never
/// exiting: the program, and each such flag with the one that turns it
/// off again when it comes after it. Expo's templates had `"test": "jest
/// --watchAll"`; the check ran until its time ran out.
pub const WATCHERS: &[(&str, &[(&str, &str)])] = &[(
    "jest",
    &[
        ("--watchAll", "--watchAll=false"),
        ("--watch", "--watch=false"),
    ],
)];

/// `command`, the package manager `pm`'s run of a script whose text is
/// `body`, made to end: a script that runs a [`WATCHERS`] runner in watch
/// mode gets the flags that turn it off after its own, which the runner
/// takes over them (npm passes them on after `--`). Left as it is when
/// the script is more than one command, where they would go to the last.
pub fn run_once(command: String, body: &str, pm: &str) -> String {
    let words: Vec<&str> = body.split_whitespace().collect();
    if body.contains(['&', '|', ';', '\n']) {
        return command;
    }
    let runs = |program: &str| words.iter().any(|w| w.rsplit('/').next() == Some(program));
    let on = |flag: &str| {
        words
            .iter()
            .any(|w| *w == flag || *w == format!("{flag}=true"))
    };
    let off: Vec<&str> = WATCHERS
        .iter()
        .filter(|(program, _)| runs(program))
        .flat_map(|(_, flags)| flags.iter())
        .filter(|(flag, _)| on(flag))
        .map(|(_, off)| *off)
        .collect();
    if off.is_empty() {
        return command;
    }
    let pass_on = if pm == "npm" { " --" } else { "" };
    format!("{command}{pass_on} {}", off.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watching_runner_is_run_once_and_others_as_they_are() {
        let yarn = |body: &str| run_once("yarn run test".into(), body, "yarn");
        assert_eq!(yarn("jest --watchAll"), "yarn run test --watchAll=false");
        assert_eq!(
            yarn("TZ=UTC jest --watch --coverage"),
            "yarn run test --watch=false"
        );
        assert_eq!(
            yarn("node_modules/.bin/jest --watchAll=true"),
            "yarn run test --watchAll=false"
        );
        assert_eq!(yarn("jest"), "yarn run test");
        assert_eq!(yarn("jest --watch=false"), "yarn run test");
        // The flags would go to the last command, not to Jest.
        assert_eq!(yarn("jest --watchAll && echo done"), "yarn run test");
        // Another runner's `--watch` isn't Jest's.
        assert_eq!(yarn("mocha --watch"), "yarn run test");
        assert_eq!(
            run_once("npm test".into(), "jest --watchAll", "npm"),
            "npm test -- --watchAll=false"
        );
    }

    #[test]
    fn the_script_that_checks_is_picked_over_one_that_fixes() {
        let scripts = |list: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                list.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, b)| b.to_string())
            }
        };
        let lint = check_scripts("lint");
        // Ignite: `lint` fixes, `lint:check` only checks.
        let ignite = scripts(&[
            ("lint", "eslint App --fix && npm run format -- --write"),
            ("lint:check", "eslint App"),
        ]);
        assert_eq!(pick(lint, ignite).as_deref(), Some("lint:check"));
        // Only a fixing one: it's the check all the same.
        let fixes = scripts(&[("lint", "eslint . --fix")]);
        assert_eq!(pick(lint, fixes).as_deref(), Some("lint"));
        let plain = scripts(&[("lint", "eslint ."), ("lint:check", "eslint src")]);
        assert_eq!(pick(lint, plain).as_deref(), Some("lint"));
        let blank = scripts(&[("lint", "  ")]);
        assert_eq!(pick(lint, blank), None);
        assert!(check_scripts("nope").is_empty());
    }
}
