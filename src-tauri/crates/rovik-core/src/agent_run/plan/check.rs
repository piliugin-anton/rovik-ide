//! The checks a plan gets when it's submitted: its form (`plan_problems`),
//! an Autopilot plan's criteria, and its steps against the project's files
//! (B07).

use super::steps::{BadPath, PlanStep};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// S01: what an Autopilot plan's criteria lack (empty: they're there).
pub fn criteria_problems(markdown: &str) -> Vec<String> {
    super::PlanDoc::parse(markdown).criteria_problems()
}

impl super::PlanDoc {
    /// Criteria problems using this document's indexed structure.
    pub fn criteria_problems(&self) -> Vec<String> {
        let Some(c) = self.criteria() else {
            return vec![
            "Add a \"## Criteria\" section: what done means, as numbered statements (1., 2., ...), each one a test or a \
             reading of the result can check; then **Out of scope** and **Assumptions** (either may say \"None.\")."
                .into(),
        ];
        };
        let mut problems = Vec::new();
        if c.items.is_empty() {
            problems.push(
            "List the criteria under \"## Criteria\" as a numbered list (1., 2., ...): each a statement of what done \
             means that a test or a reading of the result can check."
                .into(),
        );
        }
        if !c.has_out_of_scope {
            problems.push(
            "Add **Out of scope** under \"## Criteria\": what the task leaves alone, or \"None.\"."
                .into(),
        );
        }
        if !c.has_assumptions {
            problems.push(
            "Add **Assumptions** under \"## Criteria\": what you decided that the request doesn't say, with why, or \
             \"None.\"."
                .into(),
        );
        }
        problems
    }
}

/// Words that say what a step does to its files.
static CHANGE_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(add\w*|chang\w*|creat\w*|bump\w*|upgrad\w*|downgrad\w*|install\w*|pin|pins|pinned|rewrit\w*|rework\w*|revert\w*|restor\w*|simplif\w*|deprecat\w*|reorder\w*|run|runs|rais\w*|lower\w*|increas\w*|decreas\w*|apply|applies|requir\w*|allow\w*|skip\w*|ignor\w*|comput\w*|sort\w*|filter\w*|build\w*|compil\w*|format\w*|remov\w*|delet\w*|renam\w*|mov\w*|updat\w*|replac\w*|extend\w*|edit\w*|modif\w*|introduc\w*|implement\w*|refactor\w*|split\w*|extract\w*|merg\w*|wire\w*|register\w*|expos\w*|export\w*|import\w*|call\w*|return\w*|handl\w*|pass\w*|store\w*|emit\w*|drop\w*|switch\w*|convert\w*|define\w*|declare\w*|new|fix\w*|adjust\w*|insert\w*|append\w*|keep\w*|use|uses|using|set|sets|write\w*|read\w*|check\w*|validat\w*|test\w*|migrat\w*|document\w*|configur\w*|enabl\w*|disabl\w*|wrap\w*|guard\w*|inline\w*|generat\w*|render\w*|show\w*|hide\w*|load\w*|sav\w*|pars\w*|map\w*|send\w*|fetch\w*|accept\w*|reject\w*|limit\w*|cach\w*)\b",
    )
    .unwrap()
});

/// "4", or "4 (numbered 3. in the plan)": how messages name a step.
pub(super) fn step_name(number: usize, written: Option<usize>) -> String {
    match written {
        Some(w) => format!("{number} (numbered {w}. in the plan)"),
        None => number.to_string(),
    }
}

/// "2", "2 and 4", "2, 4 and 5".
pub(in crate::agent_run) fn list_steps(names: &[&str]) -> String {
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.join(""),
    }
}

/// What's wrong with a plan, if anything (empty: it's fine). Kept to the
/// minimum a plan needs to be built from (a title, steps that each name a
/// file and what changes there, a way to validate it), so a model doesn't
/// invent detail just to get past the check.
pub fn plan_problems(markdown: &str) -> Vec<String> {
    super::PlanDoc::parse(markdown).plan_problems()
}

impl super::PlanDoc {
    /// Plan problems using this document's indexed structure.
    pub fn plan_problems(&self) -> Vec<String> {
        let text = self.text().trim();
        if text.is_empty() {
            return vec!["The plan is empty.".into()];
        }
        let mut problems = Vec::new();
        if self.title().is_none() {
            problems.push("Start with a \"# \" title line.".into());
        }
        match self.steps_section() {
            None => problems
                .push("Add a \"## Steps\" section with the steps as a numbered list.".into()),
            Some(_) => {
                let steps = self.raw_steps();
                if steps.len() < 2 {
                    problems.push(
                        "List the steps under \"## Steps\" as a numbered list (1., 2., ...)."
                            .into(),
                    );
                } else {
                    // P4.4: named as `steps` numbers them (`2. 4. 7.` keeps its numbers).
                    let names: Vec<String> = steps.iter().map(|s| s.step.named()).collect();
                    let checked: Vec<_> = steps.iter().map(|s| &s.files).collect();
                    let no_file: Vec<&str> = (0..steps.len())
                        .filter(|&i| checked[i].files.is_empty() && checked[i].bad.is_empty())
                        .map(|i| names[i].as_str())
                        .collect();
                    if !no_file.is_empty() {
                        problems.push(format!(
                        "Step{} {} name{} no file: give each step a \"Files:\" line with the full path from the project root \
                         of every file it changes, in backticks and comma-separated, new files marked \"(new)\" \
                         (e.g. Files: `src/db/users.rs`, `src/api/users.rs` (new)). Only that line names a step's files.",
                        if no_file.len() > 1 { "s" } else { "" },
                        list_steps(&no_file),
                        if no_file.len() > 1 { "" } else { "s" },
                    ));
                    }
                    for (i, step) in checked.iter().enumerate() {
                        for (token, why) in &step.bad {
                            let name = &names[i];
                            problems.push(match why {
                            BadPath::Absolute => format!(
                                "Step {name}'s Files line has `{token}`, an absolute path: write it from the project root."
                            ),
                            BadPath::Outside(Some(inside)) => format!(
                                "Step {name}'s Files line has `{token}`: paths start at the project root, never with `..` \
                                 (the plan is saved in .rovik/plans/, but its paths aren't from that folder). If the file is \
                                 in the project, write it as `{inside}`."
                            ),
                            BadPath::Outside(None) => format!(
                                "Step {name}'s Files line has `{token}`, which is outside the project: use a path inside it, \
                                 from the project root."
                            ),
                            BadPath::NotAPath => format!(
                                "Step {name}'s Files line has `{token}`, which isn't a path: the line holds only the step's \
                                 file paths, in backticks and comma-separated; say the rest in the step's text."
                            ),
                        });
                        }
                    }
                    let vague: Vec<&str> = (0..steps.len())
                        .filter(|&i| {
                            let step = self.prose(steps[i].range.clone());
                            step.split_whitespace().count() < 6 || !CHANGE_WORD.is_match(&step)
                        })
                        .map(|i| names[i].as_str())
                        .collect();
                    if !vague.is_empty() {
                        problems.push(format!(
                        "Step{} {} {} too vague: say what to add, change or remove in the files, by name (the functions, \
                         types, components or settings, and how they behave); a one-line signature is fine.",
                        if vague.len() > 1 { "s" } else { "" },
                        list_steps(&vague),
                        if vague.len() > 1 { "are" } else { "is" },
                    ));
                    }
                }
            }
        }
        let validation = self.validation();
        match validation {
        Some(section) if !section.is_empty() => {}
        _ => problems.push(
            "Add a \"## Validation\" section: the commands to run (build, typecheck, tests, lint), the tests to add or \
             update, and what to check by hand."
                .into(),
        ),
    }
        // A code block left open runs to the end: the steps after it would be
        // its step's body, silently.
        if self.open_fence().is_some() {
            problems.push(
            "Close the code block that's left open (```): everything after it reads as code, steps included.".into(),
        );
        }
        // Code belongs in the implementation, not the plan. Signatures and
        // config lines (up to 3 lines) are welcome and don't count.
        let (mut code_lines, mut longest) = (0usize, 0usize);
        for block in self.code_blocks() {
            let lines = self.text()[block.clone()]
                .split('\n')
                .count()
                .saturating_sub(2);
            if lines <= 3 {
                continue;
            }
            code_lines += lines;
            longest = longest.max(lines);
        }
        if longest > 15 || code_lines > 40 {
            problems.push(
            "Leave the code out: say what changes and where, in words; short snippets (a signature, a config line) are fine."
                .into(),
        );
        }
        if text.chars().count() > 40_000 {
            problems.push("Shorten it: the plan is over 40,000 characters.".into());
        }
        problems
    }
}

/// Where a relative path's `..` parts take it, as written (no links
/// followed); None when out of the project.
pub(super) fn without_dots(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(if path.ends_with('/') && !parts.is_empty() {
        format!("{}/", parts.join("/"))
    } else {
        parts.join("/")
    })
}

/// B07: what's wrong with a plan against the project, beyond its form:
/// files that aren't there, a file used before the step that creates it,
/// and steps of one group that share a file.
pub fn project_problems(markdown: &str, root: &Path) -> Vec<String> {
    super::PlanDoc::parse(markdown).project_problems(root)
}

impl super::PlanDoc {
    /// Project problems using this document's indexed structure.
    pub fn project_problems(&self, root: &Path) -> Vec<String> {
        let steps = self.steps();
        let mut problems = Vec::new();
        // Created by which step.
        let created: Vec<(&str, usize)> = steps
            .iter()
            .flat_map(|s| {
                s.files
                    .iter()
                    .filter(|f| f.new)
                    .map(move |f| (f.path.as_str(), s.number))
            })
            .collect();
        // A folder a new file of the plan is in comes with it (P4.2).
        let exists = |path: &str| {
            root.join(path.trim_end_matches('/')).exists()
                || (path.ends_with('/')
                    && created.iter().any(|(p, _)| {
                        super::super::paths::under(path, p) && !super::super::paths::same(path, p)
                    }))
        };
        let mut missing: Vec<String> = Vec::new();
        // A file named by its file name alone (`users.rs`) that isn't at the
        // root: where it is in the project, so the step can name it in full.
        let bare = |path: &str| !path.trim_end_matches('/').contains('/');
        let bare_missing: Vec<&str> = steps
            .iter()
            .flat_map(|s| s.files.iter())
            .filter(|f| !f.new && bare(&f.path) && !exists(&f.path))
            .map(|f| f.path.as_str())
            .collect();
        let located = files_named(root, &bare_missing);
        for step in &steps {
            for file in step.files.iter().filter(|f| !f.new) {
                let creator = created
                    .iter()
                    .find(|(p, _)| p.trim_end_matches('/') == file.path.trim_end_matches('/'))
                    .map(|(_, n)| *n);
                match creator {
                Some(n) if n > step.number && !exists(&file.path) => problems.push(format!(
                    "Step {} uses `{}`, which step {n} creates: put the steps in an order where a file is created before it's used.",
                    step.named(), file.path
                )),
                Some(_) => {}
                None if !exists(&file.path) && !missing.contains(&file.path) => {
                    missing.push(file.path.clone());
                    let found = located
                        .iter()
                        .find(|(name, _)| *name == file.path)
                        .map(|(_, paths)| paths)
                        .filter(|paths| !paths.is_empty());
                    match found {
                        Some(paths) => problems.push(format!(
                            "Step {} names `{}` by its file name alone: write its full path from the project root, {}.",
                            step.named(),
                            file.path,
                            match paths.as_slice() {
                                [one] => format!("`{one}`"),
                                many => format!(
                                    "one of {}",
                                    many.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ")
                                ),
                            }
                        )),
                        None => problems.push(format!(
                            "Step {} names `{}`, which isn't in the project: fix the path (in full from the project root), \
                             or mark it \"(new)\" if the step creates it.",
                            step.named(),
                            file.path
                        )),
                    }
                }
                None => {}
            }
            }
            // A new file named by its file name alone goes at the project
            // root; a source file there is almost always a path cut short.
            for file in step.files.iter().filter(|f| f.new && bare(&f.path)) {
                let ext = file
                    .path
                    .rsplit_once('.')
                    .map(|(_, e)| e.to_ascii_lowercase())
                    .unwrap_or_default();
                if SOURCE_EXTENSIONS.contains(&ext.as_str())
                    && !root_has_extension(root, &ext)
                    && !missing.contains(&file.path)
                {
                    missing.push(file.path.clone());
                    problems.push(format!(
                    "Step {} creates `{}` at the project root (a file name alone is a root file), where the project keeps no \
                     `.{ext}` file: write its full path from the project root, with the folder it goes in.",
                    step.named(),
                    file.path
                ));
                }
            }
        }
        // A group's steps run side by side: they can't share a file.
        let mut groups: Vec<&str> = steps.iter().filter_map(|s| s.group.as_deref()).collect();
        groups.dedup();
        let mut seen_groups: Vec<&str> = Vec::new();
        for group in groups {
            if seen_groups.contains(&group) {
                continue;
            }
            seen_groups.push(group);
            let members: Vec<&PlanStep> = steps
                .iter()
                .filter(|s| s.group.as_deref() == Some(group))
                .collect();
            for (i, a) in members.iter().enumerate() {
                for b in &members[i + 1..] {
                    // P4.1: what one changes, the other may not name at all
                    // (read-only too: it would read a moving target), and a
                    // folder covers what's in it. (New files in one folder are
                    // kept apart by the workers' scopes: see group.rs.)
                    let mut shared: Vec<String> = Vec::new();
                    for (x, y) in [(a, b), (b, a)] {
                        for f in x.files.iter().filter(|f| !f.read_only) {
                            for g in &y.files {
                                if files_overlap(&f.path, &g.path) {
                                    let p = format!("`{}`", f.path.trim_end_matches('/'));
                                    if !shared.contains(&p) {
                                        shared.push(p);
                                    }
                                }
                            }
                        }
                    }
                    if !shared.is_empty() {
                        problems.push(format!(
                        "Steps {} and {} are both in group \"{group}\" but both name {}: steps in a group are built side by side, \
                         so they can't share a file. Move the shared change into a step before the group, or take one of the steps out of it.",
                        a.number,
                        b.number,
                        shared.join(", ")
                    ));
                    }
                }
            }
            // F03: a file the group's steps use as shared context is settled
            // first, by a contract step before the group that changes it.
            let first = members.iter().map(|s| s.number).min().unwrap_or(0);
            let mut context: Vec<&str> = Vec::new();
            for f in members
                .iter()
                .flat_map(|s| s.files.iter().filter(|f| f.read_only))
            {
                let users = members
                    .iter()
                    .filter(|s| s.files.iter().any(|g| g.path == f.path))
                    .count();
                if users >= 2 && !context.contains(&f.path.as_str()) {
                    context.push(&f.path);
                }
            }
            for path in context {
                let settled = steps.iter().any(|s| {
                    s.number < first
                        && s.group.as_deref() != Some(group)
                        && s.files.iter().any(|f| f.path == path && !f.read_only)
                });
                if !settled {
                    problems.push(format!(
                    "The steps of group \"{group}\" all build on `{path}`, but no step before the group settles it: add a contract step \
                     before the group that adds the shared types, signatures or names there (with stub bodies), so the group's steps \
                     work against the same interface."
                ));
                }
            }
        }
        problems
    }
}

/// Extensions of source files, which live in a folder, not at the root.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "swift", "c", "h",
    "cc", "cpp", "hpp", "cs", "rb", "php", "css", "scss", "sass", "less", "vue", "svelte", "ex",
    "exs", "dart", "scala", "lua", "sql", "astro", "elm", "zig", "hs",
];

/// Whether a file with this extension is at the project root.
fn root_has_extension(root: &Path, ext: &str) -> bool {
    std::fs::read_dir(root).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.file_type().is_ok_and(|t| t.is_file())
                && e.path()
                    .extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(ext))
        })
    })
}

/// How many paths `files_named` gives for one name, and how many files
/// it looks at.
const NAMED_PATHS: usize = 4;

const NAMED_WALK_LIMIT: usize = 50_000;

/// Where the project keeps files with these names: each name with its
/// repo-relative paths (up to NAMED_PATHS, sorted), as git sees the
/// project (ignored files, `.git`, `node_modules` and `target` left out).
pub(super) fn files_named(root: &Path, names: &[&str]) -> Vec<(String, Vec<String>)> {
    let mut names: Vec<&str> = names.to_vec();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(String, Vec<String>)> =
        names.iter().map(|n| (n.to_string(), Vec::new())).collect();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .filter_entry(|e| {
            let name = e.file_name();
            name != ".git" && name != "node_modules" && name != "target"
        })
        .build();
    for entry in walker.flatten().take(NAMED_WALK_LIMIT) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy();
        let Some((_, paths)) = found.iter_mut().find(|(n, _)| *n == file_name) else {
            continue;
        };
        if let Ok(rel) = entry.path().strip_prefix(root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !rel.contains('/') || paths.contains(&rel) {
                continue;
            }
            paths.push(rel);
        }
    }
    for (_, paths) in &mut found {
        paths.sort();
        paths.truncate(NAMED_PATHS);
    }
    found
}

/// P4.1: two plan paths that touch the same files: the same path in any
/// spelling, or one a folder (`src/api/`) the other is in.
fn files_overlap(a: &str, b: &str) -> bool {
    use super::super::paths::{same, under};
    let folder = |p: &str| p.trim_end().ends_with('/');
    same(a, b) || (folder(a) && under(a, b)) || (folder(b) && under(b, a))
}

/// B07b: the note the agent gets when the plan it's asked to implement
/// has problems (the user may have edited it after it was checked).
pub fn plan_check_note(file: &str, problems: &[String]) -> String {
    format!(
        "[Automatic check by the IDE, not written by the user]\nThe plan file {file} was checked as it is now (it may have been edited since it was written), and the checker found:\n- {}\n\nCheck each against the code before you start. Where the plan is wrong, follow the code and the user's intent, and say what you changed from the plan when you finish.",
        problems.join("\n- ")
    )
}

/// `plan_problems` and, with a project, `project_problems`.
pub fn plan_problems_in(markdown: &str, root: Option<&Path>) -> Vec<String> {
    super::PlanDoc::parse(markdown).plan_problems_in(root)
}

impl super::PlanDoc {
    /// Plan problems in using this document's indexed structure.
    pub fn plan_problems_in(&self, root: Option<&Path>) -> Vec<String> {
        let mut problems = self.plan_problems();
        if let Some(root) = root {
            problems.extend(self.project_problems(root));
        }
        problems
    }
}

/// `plan_problems_in`, and in an Autopilot task's plan its criteria (S01).
pub fn plan_problems_for(markdown: &str, root: Option<&Path>, autopilot: bool) -> Vec<String> {
    super::PlanDoc::parse(markdown).plan_problems_for(root, autopilot)
}

impl super::PlanDoc {
    /// Plan problems for using this document's indexed structure.
    pub fn plan_problems_for(&self, root: Option<&Path>, autopilot: bool) -> Vec<String> {
        let mut problems = self.plan_problems_in(root);
        if autopilot {
            problems.extend(self.criteria_problems());
        }
        problems
    }
}
