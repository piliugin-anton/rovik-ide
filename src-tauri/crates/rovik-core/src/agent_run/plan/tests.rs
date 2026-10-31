use super::check::without_dots;
use super::file::file_stem;
use super::prompt::{
    AUTOPILOT_CRITERIA, AUTOPILOT_PLAN_STAGE, AUTOPILOT_QUESTIONS, PLAN_MODE_CHOSEN,
    PLAN_MODE_QUESTIONS, PLAN_SUMMARY_LINE,
};
use super::steps::is_fence;
use super::*;
use serde_json::Value;
use std::path::Path;

/// A04: Autopilot's plan stage has its own lines for Plan mode's on
/// asking and on the user's choice; the rest is Plan mode's. The lines
/// it replaces are Plan mode's still, or it would replace nothing.
#[test]
fn autopilots_plan_stage_asks_nothing() {
    for line in [PLAN_MODE_CHOSEN, PLAN_MODE_QUESTIONS] {
        assert!(PLAN_MODE_INSTRUCTIONS.contains(line), "{line}");
    }
    assert_eq!(plan_mode_instructions(false), PLAN_MODE_INSTRUCTIONS);
    let autopilot = plan_mode_instructions(true);
    assert!(autopilot.contains(AUTOPILOT_QUESTIONS) && autopilot.contains(AUTOPILOT_PLAN_STAGE));
    assert!(!autopilot.contains("ask_user") && !autopilot.contains("The user chose"));
    assert!(autopilot.contains("submit_plan") && autopilot.contains("## Decisions"));
    // S01: and what done means, before the steps.
    assert!(PLAN_MODE_INSTRUCTIONS.contains(PLAN_SUMMARY_LINE));
    let criteria = autopilot
        .find(AUTOPILOT_CRITERIA)
        .expect("the criteria line");
    assert!(criteria < autopilot.find("\"## Steps\"").unwrap());
    assert!(!PLAN_MODE_INSTRUCTIONS.contains("## Criteria"));
}

const CRITERIA_PLAN: &str = "# Add users\n\nUsers can sign up.\n\n## Criteria\n\n\
    1. `POST /users` with a new email creates a user and answers 201.\n\
    2. The same email again answers 409\n   and creates nothing.\n\
    3) `GET /users` lists them, newest first.\n\n\
    **Out of scope**: password resets.\n\n\
    **Assumptions**\n\n\
    - Emails compare case-insensitively: the request doesn't say, and `users.email` is `CITEXT`.\n\
    - Paging stays as the other lists do.\n\n\
    ## Steps\n\n1. **Users**\n   - Files: `src/users.rs`\n   - Add `create_user` to `src/users.rs`.\n";

/// S01: a plan's criteria, its parts, a criterion over two lines, the
/// numbered list's two spellings.
#[test]
fn a_plans_criteria_are_read() {
    let c = criteria(CRITERIA_PLAN).unwrap();
    assert_eq!(
        c.items,
        [
            "`POST /users` with a new email creates a user and answers 201.",
            "The same email again answers 409 and creates nothing.",
            "`GET /users` lists them, newest first.",
        ]
    );
    assert_eq!(c.out_of_scope, ["password resets."]);
    assert_eq!(c.assumptions.len(), 2);
    assert!(c.assumptions[0].starts_with("Emails compare case-insensitively"));
    assert!(c.has_out_of_scope && c.has_assumptions);
    assert!(criteria_problems(CRITERIA_PLAN).is_empty());
    // `## Acceptance criteria`, `### ` parts, "None." for a part.
    let other = "# T\n\n## Acceptance criteria\n\n1. It works.\n\n### Out of scope\n\nNone.\n\n### Assumptions\n- None\n\n## Steps\n";
    let c = criteria(other).unwrap();
    assert_eq!(c.items, ["It works."]);
    assert!(c.out_of_scope.is_empty() && c.assumptions.is_empty());
    assert!(c.has_out_of_scope && c.has_assumptions);
    assert!(criteria_problems(other).is_empty());
}

/// S01: what's missing, said so; garbage reads as no criteria, never as
/// some.
#[test]
fn a_plans_criteria_are_replaced_or_put_in_place() {
    let body = "1. It greets.\n\n**Out of scope**: None.\n\n**Assumptions**: English.";
    // Replaced where the plan has them, the rest as it was.
    let plan = "# Greet\n\nSay hi.\n\n## Criteria\n\n1. Old.\n\n## Steps\n\n1. **Greet**\n   - Files: `a.rs`\n";
    assert_eq!(
        with_criteria(plan, body),
        format!("# Greet\n\nSay hi.\n\n## Criteria\n\n{body}\n\n## Steps\n\n1. **Greet**\n   - Files: `a.rs`\n")
    );
    // Its last section.
    assert_eq!(
        with_criteria("# Greet\n\n## Acceptance criteria\n\n1. Old.\n", body),
        format!("# Greet\n\n## Criteria\n\n{body}\n")
    );
    // None: before the first section, after the title and summary.
    assert_eq!(
        with_criteria("# Greet\n\nSay hi.\n\n## Steps\n\n1. **Greet**\n", body),
        format!("# Greet\n\nSay hi.\n\n## Criteria\n\n{body}\n\n## Steps\n\n1. **Greet**\n")
    );
    assert_eq!(
        with_criteria("# Greet", body),
        format!("# Greet\n\n## Criteria\n\n{body}\n")
    );
    // A heading in a fence is the plan's text, not a section.
    let fenced = "# Greet\n\n```\n## Criteria\n```\n\n## Steps\n\n1. **Greet**\n";
    assert!(with_criteria(fenced, body)
        .starts_with("# Greet\n\n```\n## Criteria\n```\n\n## Criteria\n\n1. It greets."));
    let replaced = with_criteria(plan, body);
    assert_eq!(criteria(&replaced).unwrap().assumptions, ["English."]);
}

#[test]
fn criteria_missing_or_partial_say_what_to_add() {
    assert_eq!(criteria("# T\n\n## Steps\n\n1. x"), None);
    let none = criteria_problems("# T\n\n## Steps\n\n1. x");
    assert_eq!(none.len(), 1);
    assert!(
        none[0].starts_with("Add a \"## Criteria\" section"),
        "{none:?}"
    );
    // Bullets aren't criteria; the parts missing are named.
    let bullets = "# T\n\n## Criteria\n\n- It works.\n- It's fast.\n\n## Steps\n";
    assert!(criteria(bullets).unwrap().items.is_empty());
    let problems = criteria_problems(bullets);
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(problems[0].starts_with("List the criteria"));
    assert!(problems[1].contains("**Out of scope**") && problems[2].contains("**Assumptions**"));
    // A numbered list in a code block isn't one.
    let fenced = "# T\n\n## Criteria\n\n```\n1. not this\n```\n\n**Out of scope**: none\n**Assumptions**: none\n";
    assert!(criteria(fenced).unwrap().items.is_empty());
    for garbage in [
        "",
        "## Criteria",
        "## Criteria\n\n\u{0}\u{1}",
        "## criteria\n\n***\n1.",
    ] {
        let c = criteria(garbage).unwrap_or_default();
        assert!(c.items.is_empty(), "{garbage:?}: {c:?}");
    }
}

/// S01: Autopilot's plans need criteria; anyone else's, as before.
#[test]
fn only_autopilots_plans_need_criteria() {
    let plan = "# Make the test pass\n\nThe test passes once ok.txt says ok.\n\n## Steps\n\n\
        1. **Write ok**\n   - Files: `ok.txt`\n   - Replace the text of `ok.txt` with `ok`, which `check.js` checks.\n\
        2. **Check**\n   - Files: `check.js` (read)\n   - Read `check.js` to confirm it checks `ok.txt`; it keeps its code.\n\n\
        ## Validation\n- `npm test`\n";
    assert!(plan_problems_for(plan, None, false).is_empty());
    let problems = plan_problems_for(plan, None, true);
    assert_eq!(problems.len(), 1, "{problems:?}");
    let with = plan.replace(
        "## Steps",
        "## Criteria\n\n1. `npm test` passes.\n\n**Out of scope**: None.\n**Assumptions**: None.\n\n## Steps",
    );
    assert!(plan_problems_for(&with, None, true).is_empty());
}

/// A04: "Implement this plan" sent with the plan's path gets the text
/// the model is sent from the file as it is; other messages, and one
/// with its text already, are left as they are.
#[test]
fn an_implementation_request_is_written_from_the_plans_file() {
    let dir = crate::testing::temp_dir();
    let root = dir.0.to_string_lossy().into_owned();
    let file = dir.0.join(".rovik/plans/p.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "# P\n\nThe plan.\n").unwrap();
    let message = |meta: Value| super::super::UserMessage {
        id: "u".into(),
        content: "Implement the plan in `.rovik/plans/p.md`.".into(),
        meta: Some(meta),
        attachments: Vec::new(),
    };
    // A relative path is joined to the root with the system's
    // separators; an absolute one's rest is as it's written.
    let joined = Path::new(".rovik").join("plans").join("p.md");
    for (path, shown) in [
        (
            file.to_string_lossy().into_owned(),
            ".rovik/plans/p.md".into(),
        ),
        (
            ".rovik/plans/p.md".into(),
            joined.to_string_lossy().into_owned(),
        ),
    ] {
        let expected = implementation_request("# P\n\nThe plan.\n", Some(&shown), true);
        let mut m = message(json!({ "mode": "agent", "implementsPlan": true, "planPath": path }));
        expand_implementation(&mut m, Some(&root), true).unwrap();
        assert_eq!(m.meta.as_ref().unwrap()["sentContent"], expected, "{path}");
        assert_eq!(m.meta.as_ref().unwrap()["implementsPlan"], true);
    }
    for meta in [
        json!({ "mode": "agent" }),
        json!({ "implementsPlan": true }),
        json!({ "implementsPlan": true, "planPath": "x.md", "sentContent": "as sent" }),
    ] {
        let mut m = message(meta.clone());
        expand_implementation(&mut m, Some(&root), true).unwrap();
        assert_eq!(m.meta, Some(meta));
    }
    let mut gone =
        message(json!({ "implementsPlan": true, "planPath": ".rovik/plans/missing.md" }));
    let err = expand_implementation(&mut gone, Some(&root), true).unwrap_err();
    assert!(err.contains("can't be read"), "{err}");
    // A file that's no plan's isn't sent to the model as one: the
    // message may name what a server wrote.
    std::fs::write(dir.0.join("notes.md"), "private\n").unwrap();
    let mut other = message(json!({ "implementsPlan": true, "planPath": "notes.md" }));
    let err = expand_implementation(&mut other, Some(&root), true).unwrap_err();
    assert!(err.contains("isn't a plan's file"), "{err}");
}

/// A model without Rovik's `todo` (a connector's harness with its own
/// tools) isn't asked for it: it keeps its checklist with its own tool.
#[test]
fn an_implementation_without_rovik_s_todo_keeps_its_checklist_with_its_own() {
    let with = implementation_request("# P\n", Some(".rovik/plans/p.md"), true);
    assert!(with.contains("with the todo tool"), "{with}");
    let own = implementation_request("# P\n", Some(".rovik/plans/p.md"), false);
    assert!(!own.contains("todo"), "{own}");
    assert!(
        own.contains("with your own checklist tool, if you have one"),
        "{own}"
    );
    // The rest is the same.
    let rest = |text: &str| text[text.find("For each step").unwrap()..].to_string();
    assert_eq!(rest(&own), rest(&with));
    assert!(own.starts_with("Implement this plan. It's saved in .rovik/plans/p.md"));
}
use serde_json::json;

const GOOD_PLAN: &str = "# Add users\n\nSummary.\n\n## Steps\n\
    1. Users table\n   - Files: `migrations/002_users.sql` (new)\n   - Create the `users` table with `id`, `email`.\n\
    2. API\n   - Files: `src/api/users.rs`, `src/api/mod.rs`\n   - Add `fn list_users(db: &Db) -> Vec<User>` and register it in `src/api/mod.rs`.\n\n\
    ## Decisions\n- none\n\n## Validation\n- `cargo test`\n- open /users by hand";

/// A plan as Plan mode writes them: bold titles, a Files line with
/// marks and a `path:line`, sub-bullets with code spans and paths that
/// aren't the step's files, a folder, a group.
const REAL_PLAN: &str = "# Add a users page\n\nServe users from the API and list them.\n\n\
    ## Steps\n\n\
    1. **Users table**\n   - Files: `migrations/002_users.sql` (new), `migrations/README.md`\n   - Create `users` with `id`, `email`; register it in the README.\n\n\
    2. **List users in the API** — the backend part\n   - Files: `src/api/users.rs` (new), `src/api/mod.rs:12`\n   - Add `fn list_users(db: &Db) -> Result<Vec<User>>`; read `state.config` for the page size; register the route in `src/api/mod.rs`.\n   - Group: backend\n\n\
    3. **Page**\n   - Files: `src/pages/Users.tsx` (new), `src/pages/`\n   - Render the list; call `api.listUsers()` from `src/lib/api.ts`.\n   - Group: backend\n\n\
    ## Decisions\n- Paginate at 50 (the existing default).\n- Keep `users` in SQLite.\n\n\
    ## Validation\n- `cargo test`\n- open /users by hand\n";

struct Tree(std::path::PathBuf);
impl Tree {
    fn new(files: &[&str]) -> Tree {
        let dir = std::env::temp_dir().join(format!("plan-tree-{}", uuid::Uuid::new_v4()));
        for f in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        Tree(dir)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A step's files are its Files line: paths in its text (prose,
/// nested lists, code blocks) aren't its files, and a `## ` line in a
/// code block doesn't end the steps.
#[test]
fn nested_lists_belong_to_their_step_and_code_blocks_stay() {
    let plan = "# API\n\nTwo parts.\n\n## Steps\n\n\
        1. **API**\n   - Files: `src/api/users.rs` (new), `src/api/mod.rs`\n   1. Add the users module.\n   2. Register it in `src/lib.rs`.\n\
        2. **Pages**\n   - Files: `src/pages.rs`\n   - Render it, with exactly this signature:\n\n     ```rust\n     \
        ## not a heading\n     - Files: `src/fenced.rs`\n     pub fn page(id: u32) -> Html { todo!(`src/other.rs`) }\n     ```\n\
        3. **Docs**\n   - Files: `README.md`\n   - Say so in the intro.\n\n## Validation\n- `cargo test`\n";
    let s = steps(plan);
    let numbers: Vec<usize> = s.iter().map(|x| x.number).collect();
    assert_eq!(numbers, [1, 2, 3], "{s:#?}");
    assert_eq!(s[0].title, "API");
    let files = |i: usize| {
        s[i].files
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        files(0),
        ["src/api/users.rs", "src/api/mod.rs"],
        "`src/lib.rs` in the text isn't a file"
    );
    // The code stays in the body; its Files line and paths aren't the step's.
    assert!(s[1].body.contains("pub fn page(id: u32)"), "{}", s[1].body);
    assert_eq!(files(1), ["src/pages.rs"]);
    assert_eq!(files(2), ["README.md"]);
    assert_eq!(plan_problems(plan), Vec::<String>::new());
}

/// The Files line in the forms a model writes it: a bulleted or bold
/// label, items after it or listed under it, with or without
/// backticks, commas or semicolons, a mark after each path (bold too),
/// two paths in one item, a `path:line`, a repeated path, "none".
#[test]
fn files_line_forms() {
    let paths = |step: &str| -> Vec<String> {
        step_files(step)
            .files
            .into_iter()
            .map(|f| {
                format!(
                    "{}{}{}",
                    f.path,
                    if f.new { " (new)" } else { "" },
                    if f.read_only { " (read)" } else { "" }
                )
            })
            .collect()
    };
    for step in [
        "1. **A**\n   - Files: `src/a.rs` (new), `src/types.rs` (read)\n   - Add a.",
        "1. **A**\n   - **Files:** `src/a.rs` **(new)**; `src/types.rs` _(read-only)_\n   - Add a.",
        "1. **A**\n   - **Files**: `src/a.rs` (new file) and `src/types.rs` (context)\n   - Add a.",
        "1. **A**\n   * Files: src/a.rs (new), src/types.rs (read).\n   * Add a.",
        "1. **A**\n   - Files:\n     - `src/a.rs` (new)\n     - `src/types.rs` (read)\n   - Add a.",
        "1. **A**\n   - Files: `src/a.rs:10` (new), `src\\types.rs` (read), `./src/a.rs`\n   - Add a.",
    ] {
        assert_eq!(
            paths(step),
            ["src/a.rs (new)", "src/types.rs (read)"],
            "{step}"
        );
    }
    // A list under the label ends at the step's next bullet; only the
    // first Files line counts; changed once, a file isn't read-only.
    let step = "1. **A**\n   - Files:\n     - `src/a.rs`\n     - `src/b/`\n   - (new) `src/c.rs`: in the text, not a file of the step.\n   - Files: `src/x.rs`";
    assert_eq!(paths(step), ["src/a.rs", "src/b/"]);
    assert_eq!(
        paths("1. **A**\n   - Files: `src/t.rs` (read), `src/t.rs`"),
        ["src/t.rs"]
    );
    // "none" names nothing, nor does a Files line in a code block.
    assert!(
        step_files("1. **A**\n   - Files: none\n   - Run it by hand.")
            .files
            .is_empty()
    );
    let fenced = step_files("1. **A**\n   - Run:\n\n     ```\n     Files: `src/a.rs`\n     ```\n");
    assert!(!fenced.line && fenced.files.is_empty(), "{fenced:?}");
    // Folders keep their `/`; accented and CJK names are paths.
    assert_eq!(
        paths("1. **A**\n   - Files: `src/a/b/c/`, `src/données/é.rs`, `src/深/a.rs`"),
        ["src/a/b/c/", "src/données/é.rs", "src/深/a.rs"]
    );
}

/// Items of a Files line that aren't paths are reported with what to
/// write instead and aren't the step's files; a step whose line has
/// only such items isn't also told it names no file.
#[test]
fn files_line_problems() {
    let plan = |files: &str| {
        format!(
            "# P\n\nx\n\n## Steps\n\n1. **One**\n   - Files: {files}\n   - Add the loader to it.\n\
             2. **Two**\n   - Files: `src/b.rs`\n   - Add the saver to it.\n\n## Validation\n- `cargo test`\n"
        )
    };
    // Written from the plan's folder: the path from the root is given.
    let p = plan_problems(&plan("`../../src/a.rs`, `../src/a.rs`"));
    assert_eq!(p.len(), 2, "{p:?}");
    for m in &p {
        assert!(
            m.starts_with("Step 1's Files line has `../")
                && m.contains("never with `..`")
                && m.ends_with("write it as `src/a.rs`."),
            "{m}"
        );
    }
    let p = plan_problems(&plan("`..\\..\\src\\a.rs`"));
    assert!(
        p[0].starts_with("Step 1's Files line has `../../src/a.rs`"),
        "{p:?}"
    );
    let p = plan_problems(&plan("`src/../x.rs`, `src/api/../lib/`"));
    assert!(
        p[0].ends_with("write it as `x.rs`.") && p[1].ends_with("write it as `src/lib/`."),
        "{p:?}"
    );
    assert_eq!(
        plan_problems(&plan("`src/../../k.rs`")),
        ["Step 1's Files line has `src/../../k.rs`, which is outside the project: use a path inside it, from the project root."]
    );
    for abs in [
        "/etc/hosts",
        "C:\\proj\\src\\a.rs",
        "c:/proj/a.rs",
        "D:a.rs",
        "\\\\server\\share\\a.rs",
    ] {
        let p = plan_problems(&plan(&format!("`{abs}`")));
        assert_eq!(p.len(), 1, "{abs}: {p:?}");
        assert!(p[0].contains("an absolute path"), "{abs}: {p:?}");
    }
    for junk in [
        "`src/my file.rs`",
        "`https://x.io/a.js`",
        "`*.ts`",
        "`fn a(x: u8)`",
        "`./`",
        "`src/..`",
        "the loader module",
    ] {
        let p = plan_problems(&plan(junk));
        assert_eq!(p.len(), 1, "{junk}: {p:?}");
        assert!(
            p[0].starts_with("Step 1's Files line has `") && p[0].contains("which isn't a path"),
            "{junk}: {p:?}"
        );
    }
    // Each reported once; the step's good paths are still its files.
    let mixed = plan("`src/a.rs`, `../src/a.rs`, `../src/a.rs`");
    assert_eq!(plan_problems(&mixed).len(), 1);
    assert_eq!(steps(&mixed)[0].files[0].path, "src/a.rs");
}

#[test]
fn what_counts_as_a_file() {
    for (token, path) in [
        ("src/a.rs", "src/a.rs"),
        ("Cargo.toml", "Cargo.toml"),
        ("./src/b.ts", "src/b.ts"),
        ("src/components/", "src/components/"),
        ("Dockerfile", "Dockerfile"),
        (".env", ".env"),
        ("src/a.rs:10-20", "src/a.rs"),
        ("docs/design", "docs/design"),
        ("app/(auth)/login/page.tsx", "app/(auth)/login/page.tsx"),
        ("src\\a\\b\\c\\", "src/a/b/c/"),
        ("./src//a/b/c/", "src/a/b/c/"),
        (".\\src\\a.rs", "src/a.rs"),
        // On a Files line an item is a path, whatever it looks like.
        (".btn-primary", ".btn-primary"),
    ] {
        assert_eq!(file_path(token).as_deref(), Ok(path), "{token}");
    }
    for (token, why) in [
        ("fn a(x: u8)", BadPath::NotAPath),
        ("https://x.io/a.js", BadPath::NotAPath),
        ("*.ts", BadPath::NotAPath),
        ("src/my file.rs", BadPath::NotAPath),
        ("./", BadPath::NotAPath),
        (".", BadPath::NotAPath),
        ("src/..", BadPath::NotAPath),
        ("", BadPath::NotAPath),
        ("/etc/hosts", BadPath::Absolute),
        ("C:/x.rs", BadPath::Absolute),
        ("D:a.rs", BadPath::Absolute),
        ("\\\\server\\share\\a.rs", BadPath::Absolute),
        ("../x.rs", BadPath::Outside(Some("x.rs".into()))),
        ("../../src/", BadPath::Outside(Some("src/".into()))),
        ("src/../x.rs", BadPath::Outside(Some("x.rs".into()))),
        ("src/../../k.rs", BadPath::Outside(None)),
        ("..", BadPath::Outside(None)),
    ] {
        assert_eq!(file_path(token), Err(why), "{token}");
    }
    assert_eq!(without_dots("a/b/../../c"), Some("c".into()));
    assert_eq!(without_dots("a/../.."), None);
    assert_eq!(without_dots("./a/./b/"), Some("a/b/".into()));
}

/// Round 3: a code span opened and closed on one line isn't an open
/// block, and the steps after it are read.
#[test]
fn one_line_fence_is_not_open() {
    let plan = GOOD_PLAN.replace(
        "2. API\n",
        "2. API\n   - ```ts export const x = 1``` is enough in `src/x.ts`.\n",
    );
    assert_eq!(plan_problems(&plan), Vec::<String>::new());
    assert_eq!(steps(&plan).len(), 2);
    let open = GOOD_PLAN.replace("2. API\n", "2. API\n   ```ts\n   const x = 1;\n");
    assert!(plan_problems(&open)
        .iter()
        .any(|p| p.contains("Close the code block")));
}

/// Round 3: a precise one-line step with a verb the list lacked (bump,
/// upgrade, revert) isn't vague.
#[test]
fn bump_and_upgrade_steps_are_not_vague() {
    let plan = GOOD_PLAN.replace(
        "## Decisions",
        "3. Bump\n   - Files: `migrations/002_users.sql`\n   - Bump `tokio` to 1.40.\n\
         4. Revert\n   - Files: `src/api/mod.rs`\n   - Revert the `--release` flag.\n\n## Decisions",
    );
    assert_eq!(plan_problems(&plan), Vec::<String>::new());
}

#[test]
fn steps_parse_into_number_title_files_and_group() {
    let s = steps(REAL_PLAN);
    assert_eq!(s.len(), 3);
    assert_eq!((s[0].number, s[0].title.as_str()), (1, "Users table"));
    assert_eq!(s[1].title, "List users in the API");
    assert_eq!(s[2].title, "Page");
    let files = |i: usize| {
        s[i].files
            .iter()
            .map(|f| (f.path.as_str(), f.new))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        files(0),
        [
            ("migrations/002_users.sql", true),
            ("migrations/README.md", false)
        ]
    );
    // A path:line is its file; the text's paths and spans aren't files.
    assert_eq!(
        files(1),
        [("src/api/users.rs", true), ("src/api/mod.rs", false)]
    );
    assert_eq!(
        files(2),
        [("src/pages/Users.tsx", true), ("src/pages/", false)]
    );
    assert_eq!(s[1].group.as_deref(), Some("backend"));
    assert_eq!(s[0].group, None);
    assert!(s[1].body.contains("register the route"));
    assert!(
        !s[0].body.contains("List users"),
        "a step ends where the next begins"
    );

    assert_eq!(
        section(REAL_PLAN, "Decisions").as_deref(),
        Some("- Paginate at 50 (the existing default).\n- Keep `users` in SQLite.")
    );
    assert_eq!(
        section(REAL_PLAN, "validation").as_deref(),
        Some("- `cargo test`\n- open /users by hand")
    );
    assert_eq!(section(REAL_PLAN, "Risks"), None);
    // The checker's own fixture.
    let g = steps(GOOD_PLAN);
    assert_eq!(g.len(), 2);
    assert_eq!(
        g[1].files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["src/api/users.rs", "src/api/mod.rs"]
    );
    assert!(steps("# No steps\n\nJust text.").is_empty());
}

/// A "## Findings" section in a submitted plan (with "None." or with
/// findings), or the IDE's "## Open review findings" from an earlier
/// save, is left out before the plan is checked, reviewed and saved;
/// one inside a code block is code.
#[test]
fn findings_sections_are_left_out_of_a_plan() {
    let plan = format!("{GOOD_PLAN}\n\n## Findings\n\nNone.\n");
    let (stripped, dropped) = without_findings(&plan);
    assert!(dropped);
    assert_eq!(stripped, format!("{GOOD_PLAN}\n"));
    assert!(plan_problems(&stripped).is_empty());
    let with_content = format!(
        "{GOOD_PLAN}\n\n## Review findings\n\n1. [high] Step 2 — x\n - Scenario: s\n\n## Risks\n\n- none\n\n## Open review findings\n\n- **[high] Step 1 — y**: z\n"
    );
    let (stripped, dropped) = without_findings(&with_content);
    assert!(dropped);
    assert_eq!(stripped, format!("{GOOD_PLAN}\n\n## Risks\n\n- none\n"));
    // CRLF stays CRLF; nothing to drop leaves the plan as it is.
    let crlf = plan.replace('\n', "\r\n");
    let (stripped, _) = without_findings(&crlf);
    assert_eq!(stripped, format!("{GOOD_PLAN}\n").replace('\n', "\r\n"));
    assert_eq!(without_findings(GOOD_PLAN), (GOOD_PLAN.to_string(), false));
    let fenced = format!("{GOOD_PLAN}\n\n```\n## Findings\nNone.\n```\n");
    assert_eq!(without_findings(&fenced), (fenced.clone(), false));
}

/// Steps written as `### Step N:` or `### N.` headings, or as bold
/// `**Step N:**` lines, with the Files line as `**Files:**` under the
/// heading, are the steps; a list of steps keeps winning over headings
/// inside them.
#[test]
fn steps_as_headings() {
    let tree = Tree::new(&["src/a.rs", "src/b.rs"]);
    for (one, two) in [
        ("### Step 1: One", "### Step 2: Two"),
        ("### 1. One", "### 2. Two"),
        ("#### Step 1 — One", "#### Step 2 — Two"),
        ("**Step 1: One**", "**Step 2: Two**"),
    ] {
        let plan = format!(
            "# P\n\nx\n\n## Steps\n\n{one}\n**Files:** `src/a.rs`\n\n- Add the loader to it.\n\n\
             {two}\n- Files: `src/b.rs` (read), `src/c.rs` (new)\n- Add the saver, reading the loader.\n\n\
             ## Validation\n- `cargo test`\n"
        );
        let s = steps(&plan);
        assert_eq!(
            s.iter()
                .map(|s| (s.number, s.title.as_str()))
                .collect::<Vec<_>>(),
            [(1, "One"), (2, "Two")],
            "{one}: {s:#?}"
        );
        assert_eq!(
            s[0].files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["src/a.rs"],
            "{one}"
        );
        assert_eq!(s[1].files.len(), 2, "{one}");
        assert!(s[1].files[1].new, "{one}");
        assert_eq!(
            plan_problems_in(&plan, Some(&tree.0)),
            Vec::<String>::new(),
            "{one}"
        );
    }
    // Headings inside a listed step don't split it.
    let listed = "# P\n\n## Steps\n\n1. **One**\n   - Files: `src/a.rs`\n   ### Step 1a: detail\n   - Add the loader to it.\n\
        2. **Two**\n   - Files: `src/b.rs`\n   - Add the saver to it.\n\n## Validation\n- `cargo test`\n";
    assert_eq!(steps(listed).len(), 2);
    assert!(
        plan_problems(listed).is_empty(),
        "{:?}",
        plan_problems(listed)
    );
}

/// The Files label in its other spellings: `File:` for one file,
/// `Paths:`, `File paths:`.
#[test]
fn files_label_variants() {
    for label in ["File", "file", "Paths", "Path", "File paths", "**File**"] {
        let step =
            format!("1. **A**\n   - {label}: `src/a.rs` (new), `src/b.rs`\n   - Add a to it.");
        let files = step_files(&step).files;
        assert_eq!(
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["src/a.rs", "src/b.rs"],
            "{label}"
        );
        assert!(files[0].new, "{label}");
    }
    assert!(
        step_files("1. **A**\n   - Profile: `src/a.rs`\n   - Add a.")
            .files
            .is_empty()
    );
}

/// P4.4: CRLF, `1. 1. 1.` numbering, `Group: none`, a "Next steps"
/// note before the steps.
#[test]
fn steps_parse_robustly() {
    let plan = "# P\r\n\r\n## Next steps (later)\r\n\r\n1. Nothing here.\r\n\r\n## Steps\r\n\r\n\
        1. **One**\r\n   - Files: `src/a.rs`\r\n   - Add a to it.\r\n   - Group: Pages\r\n\
        1. **Two**\r\n   - Files: `src/b.rs`\r\n   - Add b to it.\r\n   - Group: none\r\n\
        1. **Three**\r\n   - Files: `src/c.rs`\r\n   - Add c to it.\r\n   - Group: pages\r\n\r\n\
        ## Validation\r\n- `cargo test`\r\n";
    let s = steps(plan);
    assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(s[0].title, "One");
    assert_eq!(s[0].files[0].path, "src/a.rs");
    assert_eq!(s[0].group.as_deref(), Some("pages"));
    assert_eq!(s[1].group, None);
    assert_eq!(s[2].group.as_deref(), Some("pages"));
    assert!(!s[0].body.contains('\r'));
    assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
}

/// Round 2, P4.4: `1. 2. 2. 3.` is steps 1 to 4, and the last two say
/// what the plan numbers them, in messages too.
#[test]
fn duplicate_number_in_the_middle() {
    let plan = "# P\n\n## Steps\n\n1. **A**\n   - Files: `src/a.rs`\n   - Add a to it.\n\
        2. **B**\n   - Files: `src/b.rs`\n   - Add b to it.\n2. **C**\n   - Files: `src/c.rs`\n   - Add c to it.\n\
        3. **D**\n   - Files: `src/gone.rs`\n   - Add d to it.\n\n## Validation\n- `cargo test`\n";
    let s = steps(plan);
    assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert_eq!(
        s.iter().map(|s| s.written).collect::<Vec<_>>(),
        [None, None, Some(2), Some(3)]
    );
    let tree = Tree::new(&["src/a.rs", "src/b.rs", "src/c.rs"]);
    let p = project_problems(plan, &tree.0);
    assert!(
        p[0].starts_with("Step 4 (numbered 3. in the plan) names `src/gone.rs`"),
        "{p:?}"
    );
    // Numbers that go up one by one are as written.
    assert!(steps(REAL_PLAN).iter().all(|s| s.written.is_none()));
}

#[test]
fn plans_are_checked_against_the_project() {
    let tree = Tree::new(&["migrations/README.md", "src/api/mod.rs"]);
    // Every file exists or is new, and steps 2 and 3 name different files.
    assert_eq!(project_problems(REAL_PLAN, &tree.0), Vec::<String>::new());

    // A missing path, reported once.
    let missing = REAL_PLAN.replace("`src/api/mod.rs:12`", "`src/api/routes.rs`");
    let p = project_problems(&missing, &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with("Step 2 names `src/api/routes.rs`, which isn't in the project"),
        "{p:?}"
    );

    // Used before the step that creates it.
    let early = REAL_PLAN.replace("(new), `migrations/README.md`", "(new), `src/api/users.rs`");
    let p = project_problems(&early, &tree.0);
    assert_eq!(p, ["Step 1 uses `src/api/users.rs`, which step 2 creates: put the steps in an order where a file is created before it's used."]);

    // Two steps of a group naming one file.
    let shared = REAL_PLAN.replace("(new), `src/pages/`", "(new), `src/api/mod.rs`");
    let p = project_problems(&shared, &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with(
            "Steps 2 and 3 are both in group \"backend\" but both name `src/api/mod.rs`"
        ),
        "{p:?}"
    );
    // A path in the step's text isn't checked: only the Files line is.
    let in_text = "# M\n\n## Steps\n\n1. **A**\n   - Files: `src/a.rs` (new)\n   - Register it in `src/routs.rs`.\n\
        2. **B**\n   - Files: `src/api/mod.rs`\n   - Add the route for it.\n\n## Validation\n- `cargo test`\n";
    assert_eq!(project_problems(in_text, &tree.0), Vec::<String>::new());
    // P4.1: a folder and a file in it; a file one changes and another
    // reads; each refused with its steps.
    let group = |a: &str, b: &str| {
        format!(
            "# G\n\n## Steps\n\n1. **Contract**\n   - Files: `src/api/mod.rs`\n   - Add the stubs there.\n\
             2. **A**\n   - Files: {a}\n   - Add the handlers there.\n   - Group: g\n\
             3. **B**\n   - Files: {b}\n   - Register the handlers there.\n   - Group: g\n\n## Validation\n- `cargo test`\n"
        )
    };
    let p = project_problems(&group("`src/api/`", "`src/api/mod.rs`"), &tree.0);
    assert!(
        p.iter()
            .any(|p| p
                .starts_with("Steps 2 and 3 are both in group \"g\" but both name `src/api`")),
        "{p:?}"
    );
    let p = project_problems(
        &group("`src/api/mod.rs`", "`src/api/mod.rs` (read)"),
        &tree.0,
    );
    assert!(
        p.iter().any(|p| p
            .starts_with("Steps 2 and 3 are both in group \"g\" but both name `src/api/mod.rs`")),
        "{p:?}"
    );
    // F03: shared context needs a contract step before the group.
    let contract = "# C\n\n## Steps\n\n\
        1. **Contract**\n   - Files: `src/api/mod.rs`\n   - Declare `fn list_users()` and `fn add_user()` with stub bodies.\n\
        2. **List**\n   - Files: `src/api/users.rs` (new), `src/api/mod.rs` (read)\n   - Implement `list_users` against the signature.\n   - Group: api\n\
        3. **Add**\n   - Files: `src/api/add.rs` (new), `src/api/mod.rs` (read-only)\n   - Implement `add_user` against the signature.\n   - Group: api\n\n\
        ## Validation\n- `cargo test`\n";
    assert_eq!(project_problems(contract, &tree.0), Vec::<String>::new());
    let s = steps(contract);
    assert!(s[1]
        .files
        .iter()
        .any(|f| f.path == "src/api/mod.rs" && f.read_only));
    assert!(!s[0].files[0].read_only);
    let no_contract = contract.replace(
        "Files: `src/api/mod.rs`\n",
        "Files: `migrations/README.md`\n",
    );
    let p = project_problems(&no_contract, &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].contains("all build on `src/api/mod.rs`, but no step before the group settles it"),
        "{p:?}"
    );
    // Out of a group, sharing is fine.
    let ungrouped = shared.replace("   - Group: backend\n", "");
    assert!(project_problems(&ungrouped, &tree.0).is_empty());

    // With a project, plan_problems_in adds these to the form checks.
    assert_eq!(plan_problems_in(&missing, Some(&tree.0)).len(), 1);
    assert!(plan_problems_in(&missing, None).is_empty());
}

/// CSS classes, library names, word pairs and paths in a step's text
/// aren't its files, so they don't stop the plan being saved; without
/// a Files line they don't stand in for the file either.
#[test]
fn prose_is_not_files() {
    let tree = Tree::new(&["src/styles/app.css", "src/App.tsx"]);
    let plan = "# Tidy the sidebar\n\nx\n\n## Steps\n\n\
        1. **Narrow it**\n   - Files: `src/styles/app.css`\n   - `.sidebar`: set the width to 240px; `.sidebar__item`: add padding; \
        mind Node.js, the add/remove buttons, A/B tests, src/other.css and `src/gone.rs`.\n\
        2. **Hover**\n   - Files: `src/App.tsx`\n   - Add the `.sidebar__item:hover` class and change `.nav-item.active`, per github.com/foo/bar/README.md.\n\n\
        ## Validation\n- `npm run build`\n";
    assert_eq!(plan_problems_in(plan, Some(&tree.0)), Vec::<String>::new());
    assert_eq!(steps(plan)[0].files.len(), 1);
    let none = plan.replace("   - Files: `src/styles/app.css`\n", "");
    let p = plan_problems(&none);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with("Step 1 names no file: give each step a \"Files:\" line"),
        "{p:?}"
    );
}

/// What a chat's rows name is a plan's file only when it's a Markdown
/// file in a `.rovik/plans` folder that no link leads out of: the rows
/// may be a server's, naming any file of this computer's.
#[test]
fn only_a_plans_own_file_is_a_plan_file() {
    let dir = crate::testing::temp_dir();
    let plans = dir.0.join(".rovik/plans");
    std::fs::create_dir_all(&plans).unwrap();
    let file = plans.join("a.md");
    std::fs::write(&file, "# A\n").unwrap();
    let text = |p: &std::path::Path| p.to_string_lossy().into_owned();
    assert!(is_plan_file(&text(&file)));
    assert!(is_plan_file(&text(&plans.join("new.md"))), "one to save");
    assert!(!is_plan_file(&text(&plans.join("a.txt"))));
    assert!(!is_plan_file(&text(&dir.0.join("a.md"))));
    assert!(!is_plan_file("/home/user/.ssh/id_rsa"));
    assert!(!is_plan_file(&format!("{}/../../x.md", text(&plans))));
    #[cfg(unix)]
    {
        let secret = dir.0.join("secret");
        std::fs::write(&secret, "s").unwrap();
        let link = plans.join("link.md");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(!is_plan_file(&text(&link)), "a link out");
        // A plans folder that's a link elsewhere.
        let other = dir.0.join("other");
        std::fs::create_dir_all(other.join(".rovik")).unwrap();
        std::os::unix::fs::symlink(&dir.0, other.join(".rovik/plans")).unwrap();
        assert!(!is_plan_file(&text(&other.join(".rovik/plans/secret.md"))));
    }
}

/// Round 2: a plan written with CRLF is saved with LF only.
#[test]
fn save_plan_file_normalises_line_endings() {
    let tree = Tree::new(&["src/a.rs"]);
    let root = tree.0.to_string_lossy().into_owned();
    let path = save_plan_file(
        &root,
        "Do it",
        "# P\r\n\r\n## Steps\r\n1. x\r\n",
        None,
        "2026-01-01",
    )
    .unwrap();
    let saved = std::fs::read_to_string(path).unwrap();
    assert_eq!(saved, "# P\n\n## Steps\n1. x\n");
}

#[test]
fn plan_checks() {
    assert!(
        plan_problems(GOOD_PLAN).is_empty(),
        "{:?}",
        plan_problems(GOOD_PLAN)
    );
    let bad = "Some plan\n## Steps\n1. Only one";
    let problems = plan_problems(bad);
    assert_eq!(problems.len(), 3, "title, steps, validation: {problems:?}");
    assert_eq!(plan_problems("  "), vec!["The plan is empty."]);
    let code = format!("{GOOD_PLAN}\n```\n{}```", "x\n".repeat(20));
    assert_eq!(plan_problems(&code).len(), 1);
    assert!(plan_problems(&code)[0].starts_with("Leave the code out"));
    // One-line signatures don't count as code.
    let sigs = format!(
        "{GOOD_PLAN}\n{}",
        "```rust\nfn a() -> u8;\n```\n".repeat(30)
    );
    assert!(
        plan_problems(&sigs).is_empty(),
        "{:?}",
        plan_problems(&sigs)
    );
    assert!(looks_like_plan("# P\n## Steps\n1. do it"));
    assert!(looks_like_plan(
        "Here:\n\n## Steps\n1. a\n2. b\n\n## Validation\n- `cargo test`"
    ));
    assert!(!looks_like_plan("Done. 1. thing"));
    // An explanation with numbered steps isn't a plan to submit.
    assert!(!looks_like_plan(
        "The panel flickers because of a race.\n\n## Steps to reproduce\n1. Open a file\n2. Resize\n\n## Cause\nx"
    ));
}

#[test]
fn steps_need_files_and_changes() {
    // Titles alone aren't a plan.
    let terse = "# T\n## Steps\n1. Table\n2. API\n## Validation\n- run the tests";
    let problems = plan_problems(terse);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0].starts_with("Steps 1 and 2 name no file"),
        "{problems:?}"
    );
    assert!(
        problems[1].starts_with("Steps 1 and 2 are too vague"),
        "{problems:?}"
    );
    // Only the step without a Files line is reported.
    let one = "# T\n## Steps\n1. Loader\n   - Files: `src/a.rs`\n   - Add the `load` function for reading users.\n\
               2. Docs\n   - Update the docs so they explain the new loading, e.g. in the readme.\n## Testing\n- cargo test";
    assert_eq!(plan_problems(one), vec![plan_problems(one)[0].clone()]);
    assert!(plan_problems(one)[0].starts_with("Step 2 names no file"));
    // The Files line decides, backticks or not; a path in the text doesn't.
    for (step, path) in [
        (
            "1. **G**\n   - Files: `src/grammar.pest`\n   - Add a rule.",
            "src/grammar.pest",
        ),
        ("1. **P**\n   - Files: src/plain.rs\n   - Log the id.", "src/plain.rs"),
        (
            "1. **W**\n   - Files: src\\api\\users.rs\n   - Add a field.",
            "src/api/users.rs",
        ),
        (
            "1. **D**\n   - Files: Dockerfile, `Cargo.toml` (new section)\n   - Change the base image.",
            "Dockerfile",
        ),
    ] {
        let files = step_files(step).files;
        assert_eq!(files.first().map(|f| f.path.as_str()), Some(path), "{step}");
        assert!(files.iter().all(|f| !f.new), "{step}");
    }
    assert!(
        step_files("1. **X**\n   - Change the base image in Dockerfile and `src/x.rs`.")
            .files
            .is_empty()
    );
}

#[test]
fn plans_need_validation() {
    let no_check = GOOD_PLAN.replace("## Validation", "## Notes");
    let problems = plan_problems(&no_check);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].starts_with("Add a \"## Validation\""));
    // An empty section doesn't count; other names do.
    assert_eq!(
        plan_problems(&GOOD_PLAN.replace("- `cargo test`\n- open /users by hand", "")).len(),
        1
    );
    assert!(plan_problems(&GOOD_PLAN.replace("## Validation", "## How to verify")).is_empty());
    assert!(
        plan_problems(&GOOD_PLAN.replace("## Validation", "## Testing and validation")).is_empty()
    );
}

#[test]
fn stems_and_relative_paths() {
    assert_eq!(
        file_stem("Add a users table, please!", "2026-10-04"),
        "2026-10-04-add-a-users-table-please"
    );
    assert_eq!(file_stem("???", "2026-10-04"), "2026-10-04-plan");
    assert_eq!(
        relative_plan_path("/p/.rovik/plans/a.md", Some("/p/")),
        ".rovik/plans/a.md"
    );
    assert_eq!(relative_plan_path("/q/a.md", Some("/p")), "/q/a.md");
    // P4.9: a sibling folder with the same start; no panic on a
    // multi-byte character after the root.
    assert_eq!(relative_plan_path("/pq/a.md", Some("/p")), "/pq/a.md");
    assert_eq!(relative_plan_path("/pé/a.md", Some("/p")), "/pé/a.md");
}

/// A file named by its file name alone is pointed to its full path;
/// a new source file without a folder is sent back for one.
#[test]
fn bare_file_names_get_their_full_path() {
    let tree = Tree::new(&[
        "src/db/users.rs",
        "src/api/users.rs",
        "src/db/mod.rs",
        "Cargo.toml",
        "index.html",
    ]);
    let plan = |a: &str, b: &str| {
        format!(
            "# P\n\nx\n\n## Steps\n\n1. **One**\n   - Files: {a}\n   - Add `fn load()` to it.\n\
             2. **Two**\n   - Files: {b}\n   - Add `fn save()` to it.\n\n## Validation\n- `cargo test`\n"
        )
    };
    let p = project_problems(&plan("`mod.rs`", "`Cargo.toml`"), &tree.0);
    assert_eq!(
        p,
        ["Step 1 names `mod.rs` by its file name alone: write its full path from the project root, `src/db/mod.rs`."]
    );
    let p = project_problems(&plan("`users.rs`", "`src/db/mod.rs`"), &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].ends_with("one of `src/api/users.rs`, `src/db/users.rs`."),
        "{p:?}"
    );
    // Reported once, with the full-path hint when nothing matches.
    let p = project_problems(&plan("`other.rs`", "`other.rs`"), &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with(
            "Step 1 names `other.rs`, which isn't in the project: fix the path (in full"
        ),
        "{p:?}"
    );
    // New: a source file at the root needs its folder; a root file is fine.
    let p = project_problems(&plan("`handlers.rs` (new)", "`src/db/mod.rs`"), &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with("Step 1 creates `handlers.rs` at the project root")
            && p[0].contains("no `.rs` file"),
        "{p:?}"
    );
    assert!(
        project_problems(&plan("`CHANGELOG.md` (new)", "`Dockerfile` (new)"), &tree.0).is_empty()
    );
    assert!(project_problems(&plan("`src/db/new.rs` (new)", "`index.html`"), &tree.0).is_empty());
    // The form check alone doesn't know the project: no hint without a root.
    assert!(plan_problems_in(&plan("`mod.rs`", "`Cargo.toml`"), None).is_empty());
}

#[test]
fn plan_edge_cases() {
    // A steps heading with more words, steps with ")".
    let parens = GOOD_PLAN
        .replace("## Steps", "## Implementation steps")
        .replace("1. ", "1) ")
        .replace("2. ", "2) ");
    assert!(
        plan_problems(&parens).is_empty(),
        "{:?}",
        plan_problems(&parens)
    );
    // Steps belong under the steps heading, not a later section.
    let misplaced = GOOD_PLAN.replace("## Steps\n", "## Steps\nnone\n## Other\n");
    assert_eq!(plan_problems(&misplaced).len(), 1);
    assert!(
        plan_problems(&format!("{GOOD_PLAN}\n{}", "x".repeat(40_001)))
            .iter()
            .any(|p| p.contains("over 40,000"))
    );
    // Leading blank lines before the title are fine.
    assert!(plan_problems(&format!("\n\n{GOOD_PLAN}")).is_empty());
}

/// "n. **title**" with a Files line, a change and an optional group.
fn step(n: usize, title: &str, files: &str, group: Option<&str>) -> String {
    let group = group
        .map(|g| format!("   - Group: {g}\n"))
        .unwrap_or_default();
    format!("{n}. **{title}**\n   - Files: {files}\n   - Add {title} to it.\n{group}")
}

/// Round 2 (§4.2), P4.2: a folder that only a new file of the plan is in
/// counts as there, from another step and a level up; a folder whose
/// name only starts the same doesn't.
#[test]
fn new_file_makes_its_folder_exist() {
    let tree = Tree::new(&["src/lib.rs"]);
    let plan = "# F\n\n## Steps\n\n\
        1. **Module**\n   - Files: `src/feature/sub/mod.rs` (new)\n   - Add the module.\n\
        2. **Wire**\n   - Files: `src/feature/`, `src/lib.rs`\n   - Register and declare the module.\n\n\
        ## Validation\n- `cargo test`\n";
    assert_eq!(project_problems(plan, &tree.0), Vec::<String>::new());
    let p = project_problems(&plan.replace("`src/feature/`", "`src/feat/`"), &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with("Step 2 names `src/feat/`, which isn't in the project"),
        "{p:?}"
    );
}

/// Round 2 (§4.2): a step that names no file is still a step (number,
/// title and body kept, no files); the form check names it, the project
/// check adds nothing, and its worker's scope is empty until a grant.
#[test]
fn step_with_zero_files() {
    use crate::agent_run::worker::WriteScope;
    let plan = "# Z\n\nx\n\n## Steps\n\n\
        1. **Helper**\n   - Files: `src/a.rs`\n   - Add `fn helper()`.\n\
        2. **Run it** — run the migration by hand and check the logs for errors.\n\
        3. **Use**\n   - Files: `src/b.rs`\n   - Call `helper()` from `main`.\n\n\
        ## Validation\n- `cargo test`\n";
    let s = steps(plan);
    assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(s[1].title, "Run it");
    assert!(s[1].files.is_empty(), "{:?}", s[1].files);
    assert!(s[1].body.contains("check the logs"), "{}", s[1].body);
    let p = plan_problems(plan);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].starts_with("Step 2 names no file"), "{p:?}");
    let tree = Tree::new(&["src/a.rs", "src/b.rs"]);
    assert_eq!(project_problems(plan, &tree.0), Vec::<String>::new());
    let scope = WriteScope::for_step(&s[1]);
    assert_eq!(scope, WriteScope::default());
    assert_eq!(scope.describe(), "nothing yet");
    assert!(!scope.allows("src/a.rs", true) && !scope.allows("new.rs", false));
}

/// Round 2, P4.4: numbers that go up with gaps (`2. 4. 7.`) are kept as
/// written, with no separate written number, and the checks use them.
#[test]
fn gapped_numbers_are_kept() {
    let plan = format!(
        "# P\n\n## Steps\n\n{}{}{}\n## Validation\n- `cargo test`\n",
        step(2, "B", "`src/b.rs`", None),
        step(4, "D", "`src/d.rs`", None),
        step(7, "G", "`src/gone.rs`", None)
    );
    let s = steps(&plan);
    assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [2, 4, 7]);
    assert!(s.iter().all(|s| s.written.is_none()), "{s:#?}");
    assert_eq!(s[2].named(), "7");
    assert!(
        plan_problems(&plan).is_empty(),
        "{:?}",
        plan_problems(&plan)
    );
    let tree = Tree::new(&["src/b.rs", "src/d.rs"]);
    let p = project_problems(&plan, &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].starts_with("Step 7 names `src/gone.rs`"), "{p:?}");
}

/// P4.4: `## Steps:` is `## Steps` (any case, a space before the colon
/// too), taken before a "## Next steps" note.
#[test]
fn steps_heading_colon() {
    let plan = format!(
        "# P\n\n## Next steps (later)\n\n1. Nothing here.\n\n## Steps:\n\n{}{}\n## Validation\n- `cargo test`\n",
        step(1, "One", "`src/a.rs`", None),
        step(2, "Two", "`src/b.rs`", None)
    );
    let s = steps(&plan);
    assert_eq!(
        s.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
        ["One", "Two"]
    );
    assert!(
        plan_problems(&plan).is_empty(),
        "{:?}",
        plan_problems(&plan)
    );
    let spaced = plan.replace("## Steps:", "## STEPS :");
    assert_eq!(steps(&spaced).len(), 2);
}

/// P4.4: a `Group:` line in a ``` or ~~~ code block isn't the step's
/// group; a real one after the block still is.
#[test]
fn group_in_fenced_code_ignored() {
    let plan = "# P\n\n## Steps\n\n\
        1. **One**\n   - Files: `src/a.rs`\n   - Add a, configured as:\n\n     ```yaml\n     Group: fenced\n     ```\n\
        2. **Two**\n   - Files: `src/b.rs`\n   - Add b, configured as:\n\n     ~~~\n     group: tilde\n     ~~~\n\
        3. **Three**\n   - Files: `src/c.rs`\n   - Add c to it.\n\n     ```\n     Group: fenced\n     ```\n   - Group: Real\n\n\
        ## Validation\n- `cargo test`\n";
    let s = steps(plan);
    assert_eq!(s.len(), 3, "{s:#?}");
    assert_eq!(s[0].group, None);
    assert_eq!(s[1].group, None);
    assert_eq!(s[2].group.as_deref(), Some("real"));
    // The code stays in the body.
    assert!(s[1].body.contains("group: tilde"), "{}", s[1].body);
}

/// P4.4: numbered sub-lists indented with tabs, spaces or both belong
/// to their step: only the least indented numbered lines are steps.
#[test]
fn tabs_vs_spaces_sub_lists() {
    let plan = "# P\n\n## Steps\n\n\
        1. **Tabs**\n\t- Files: `src/a.rs` (new), `src/lib.rs`\n\t1. Add it.\n\t2. Register it.\n\
        2. **Spaces**\n    - Files: `src/b.rs` (new), `src/lib.rs`\n    1. Add it.\n    2. Register it.\n\
        3. **Both**\n\t- Files: `src/c.rs` (new), `src/lib.rs`\n\t1. Add it.\n  \t2. Register it.\n\t\t1. Export it.\n\n\
        ## Validation\n- `cargo test`\n";
    let s = steps(plan);
    assert_eq!(
        s.iter().map(|s| s.number).collect::<Vec<_>>(),
        [1, 2, 3],
        "{s:#?}"
    );
    for (step, new) in s.iter().zip(["src/a.rs", "src/b.rs", "src/c.rs"]) {
        let files: Vec<(&str, bool)> = step
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.new))
            .collect();
        assert_eq!(files, [(new, true), ("src/lib.rs", false)], "{}", step.body);
    }
    assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
}

/// P4.4: numbered sub-lists (`.` or `)`, nested twice) belong to their
/// step: they aren't steps, don't renumber the steps and don't count as
/// the numbers the plan shows.
#[test]
fn numbered_sub_lists_inside_step() {
    let plan = "# P\n\n## Steps\n\n\
        1. **Store**\n   - Files: `src/store.rs` (new), `src/lib.rs`\n   1. Add `fn save(x: &X)`.\n   2. Register it.\n      1. Export `save` from the crate root.\n\
        2. **Use it**\n   - Files: `src/api.rs`, `src/api_test.rs`, `README.md`\n   1) Call `save`.\n   2) Check it in the test.\n   3) Document it.\n\n\
        ## Validation\n- `cargo test`\n";
    let s = steps(plan);
    assert_eq!(
        s.iter().map(|s| (s.number, s.written)).collect::<Vec<_>>(),
        [(1, None), (2, None)],
        "{s:#?}"
    );
    assert_eq!(s[1].title, "Use it");
    let files = |i: usize| {
        s[i].files
            .iter()
            .map(|f| (f.path.as_str(), f.new))
            .collect::<Vec<_>>()
    };
    assert_eq!(files(0), [("src/store.rs", true), ("src/lib.rs", false)]);
    assert_eq!(
        files(1),
        [
            ("src/api.rs", false),
            ("src/api_test.rs", false),
            ("README.md", false)
        ]
    );
    assert!(s[0].body.contains("1. Export `save`"), "{}", s[0].body);
    assert!(!s[0].body.contains("Use it"));
    assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
}

/// P4.4: a group's name reads the same in backticks, in bold or both,
/// in any case, so these steps are one group; `none` in backticks is
/// still no group.
#[test]
fn group_name_with_backticks_and_bold() {
    let plan = format!(
        "# P\n\n## Steps\n\n{}{}{}{}{}{}\n## Validation\n- `cargo test`\n",
        step(1, "A", "`src/a.rs`", Some("`Backend`")),
        step(2, "B", "`src/b.rs`", None)
            .replace("   - Add B", "   - **Group:** backend\n   - Add B"),
        step(3, "C", "`src/c.rs`", None)
            .replace("   - Add C", "   - **Group**: **backend**\n   - Add C"),
        step(4, "D", "`src/d.rs`", Some("**`backend`**")),
        step(5, "E", "`src/e.rs`", None)
            .replace("   - Add E", "   - **Group: `BACKEND`**\n   - Add E"),
        step(6, "F", "`src/f.rs`", Some("`none`"))
    );
    let s = steps(&plan);
    assert_eq!(
        s.iter().map(|s| s.group.as_deref()).collect::<Vec<_>>(),
        [
            Some("backend"),
            Some("backend"),
            Some("backend"),
            Some("backend"),
            Some("backend"),
            None
        ],
        "{s:#?}"
    );
    // One group: two of its steps can't share a file.
    let shared = plan.replace("Files: `src/b.rs`", "Files: `src/a.rs`");
    let tree = Tree::new(&["src/a.rs", "src/c.rs", "src/d.rs", "src/e.rs", "src/f.rs"]);
    let p = project_problems(&shared, &tree.0);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with("Steps 1 and 2 are both in group \"backend\" but both name `src/a.rs`"),
        "{p:?}"
    );
}

/// Round 2, P4.4: `## Steps (ordered)` is the steps heading, alone and
/// after a "## Next steps" note (which would otherwise be read first).
#[test]
fn steps_heading_with_suffix() {
    let plan = format!(
        "# P\n\n## Next steps (later)\n\n1. Nothing here.\n\n## Steps (ordered)\n\n{}{}\n## Validation\n- `cargo test`\n",
        step(1, "One", "`src/a.rs`", None),
        step(2, "Two", "`src/b.rs`", None)
    );
    let alone = plan.replace("## Next steps (later)\n\n1. Nothing here.\n\n", "");
    for plan in [alone.as_str(), plan.as_str()] {
        let s = steps(plan);
        assert_eq!(
            s.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            ["One", "Two"],
            "{plan}"
        );
        assert_eq!(s[1].files[0].path, "src/b.rs");
        assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
    }
}

/// Round 2, P4.4: a numbered `## 3. Steps` is the steps heading, alone
/// and after a "## 2. Next steps" note; its number isn't a step's.
#[test]
fn steps_heading_numbered() {
    let plan = format!(
        "# P\n\n## 1. Summary\n\nTwo files.\n\n## 2. Next steps (later)\n\n1. Nothing here.\n\n\
        ## 3. Steps\n\n{}{}\n## 4. Validation\n- `cargo test`\n",
        step(1, "One", "`src/a.rs`", None),
        step(2, "Two", "`src/b.rs`", None)
    );
    let alone = plan.replace("## 2. Next steps (later)\n\n1. Nothing here.\n\n", "");
    for plan in [alone.as_str(), plan.as_str()] {
        let s = steps(plan);
        assert_eq!(
            s.iter().map(|s| s.number).collect::<Vec<_>>(),
            [1, 2],
            "{plan}"
        );
        assert_eq!(s[0].title, "One");
        assert_eq!(s[1].files[0].path, "src/b.rs");
        assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
    }
}

/// Round 2: a numbered heading (`## 5. Decisions`) is its name's
/// section, so a step worker's brief keeps the plan's decisions.
#[test]
fn section_finds_numbered_headings() {
    let plan = format!(
        "# P\n\n## 1. Summary\n\nTwo files.\n\n## 3. Steps\n\n{}{}\n## 4) Validation\n- `cargo test`\n\n## 5. Decisions\n\n- Keep the old API.\n",
        step(1, "One", "`src/a.rs`", None),
        step(2, "Two", "`src/b.rs`", None)
    );
    assert_eq!(
        section(&plan, "Decisions").as_deref(),
        Some("- Keep the old API.")
    );
    assert_eq!(
        section(&plan, "validation").as_deref(),
        Some("- `cargo test`")
    );
    assert_eq!(section(&plan, "Summary").as_deref(), Some("Two files."));
    assert_eq!(section("## 2024 roadmap\n\nx\n", "roadmap"), None);
    assert_eq!(
        section("## 2024 roadmap\n\nx\n", "2024").as_deref(),
        Some("x")
    );
}

/// Round 2, P4.4: the form check names a step as `steps` numbers it:
/// in `2. 4. 7.` the step without a file is step 4, not "step 2"; in
/// `1. 2. 2. 3.` the last is step 4, numbered 3. in the plan.
#[test]
fn gapped_numbers_in_form_check_messages() {
    let plan = format!(
        "# P\n\n## Steps\n\n{}4. **D**\n   - Add the d helper next to b, with its own tests.\n{}\n## Validation\n- `cargo test`\n",
        step(2, "B", "`src/b.rs`", None),
        step(7, "G", "`src/g.rs`", None)
    );
    let p = plan_problems(&plan);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].starts_with("Step 4 names no file"), "{p:?}");
    let plan = format!(
        "# P\n\n## Steps\n\n{}{}{}3. **D**\n   - Files: `src/d.rs`\n\n## Validation\n- `cargo test`\n",
        step(1, "A", "`src/a.rs`", None),
        step(2, "B", "`src/b.rs`", None),
        step(2, "C", "`src/c.rs`", None)
    );
    let p = plan_problems(&plan);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].starts_with("Step 4 (numbered 3. in the plan) is too vague"),
        "{p:?}"
    );
}

/// Round 2, P4.4: a code block that's never closed runs to the end of
/// the plan (no guess at where it was meant to end), so the steps after
/// it are its step's body; the form check asks for it to be closed.
#[test]
fn unclosed_fence_swallows_rest() {
    let plan = "# P\n\n## Steps\n\n\
        1. **One**\n   - Files: `src/a.rs`\n   - Add `a` to the module.\n\
        2. **Two**\n   - Files: `src/b.rs`\n   - Add `b` with this signature:\n\n     ```rust\n     pub fn b() -> u8\n\
        3. **Three**\n   - Files: `src/c.rs`\n   - Call `b` from `c`.\n\n## Validation\n- `cargo test`\n";
    let s = steps(plan);
    assert_eq!(s.len(), 2, "{s:#?}");
    assert_eq!(
        s[1].files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["src/b.rs"]
    );
    assert!(
        s[1].body.contains("3. **Three**") && s[1].body.contains("## Validation"),
        "{}",
        s[1].body
    );
    let p = plan_problems(plan);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].starts_with("Close the code block"), "{p:?}");
    let closed = plan.replace("pub fn b() -> u8\n", "pub fn b() -> u8\n     ```\n");
    assert_eq!(steps(&closed).len(), 3);
    assert!(
        plan_problems(&closed).is_empty(),
        "{:?}",
        plan_problems(&closed)
    );
}

/// Round 2, P4.9: a Windows root, with or without its last separator,
/// and a `/`-written root a path was joined onto; the shown path keeps
/// the OS's separators. A sibling with the same start isn't under it.
#[test]
fn relative_plan_path_windows_root() {
    let file = r"C:\p\.rovik\plans\a.md";
    assert_eq!(
        relative_plan_path(file, Some(r"C:\p")),
        r".rovik\plans\a.md"
    );
    assert_eq!(
        relative_plan_path(file, Some(r"C:\p\")),
        r".rovik\plans\a.md"
    );
    assert_eq!(
        relative_plan_path(r"C:/p\.rovik\plans\a.md", Some("C:/p")),
        r".rovik\plans\a.md"
    );
    assert_eq!(
        relative_plan_path(r"C:\pq\a.md", Some(r"C:\p")),
        r"C:\pq\a.md"
    );
    assert_eq!(relative_plan_path(r"C:\a.md", Some(r"C:\")), "a.md");
}

/// Round 2, P4.9: the root itself is shown whole, never as an empty
/// path; without a root the path is shown as it is.
#[test]
fn relative_plan_path_root_equals_path() {
    assert_eq!(relative_plan_path("/p", Some("/p")), "/p");
    assert_eq!(relative_plan_path("/p", Some("/p/")), "/p");
    assert_eq!(relative_plan_path("/p/", Some("/p")), "/p/");
    assert_eq!(relative_plan_path("/", Some("/")), "/");
    assert_eq!(relative_plan_path(r"C:\p", Some(r"C:\p\")), r"C:\p");
    assert_eq!(relative_plan_path("/p/a.md", None), "/p/a.md");
}

/// Round 2, P4.4: a period or a note after a group's name isn't part of
/// it, so these steps are the group the lead asks for ("backend").
#[test]
fn group_name_followed_by_a_note_or_period() {
    let plan = format!(
        "# P\n\n## Steps\n\n{}{}{}{}{}\n## Validation\n- `cargo test`\n",
        step(1, "A", "`src/a.rs`", Some("`backend`.")),
        step(
            2,
            "B",
            "`src/b.rs`",
            Some("`Backend` (runs alongside step 1)")
        ),
        step(3, "C", "`src/c.rs`", Some("backend.")),
        step(4, "D", "`src/d.rs`", None).replace(
            "   - Add D",
            "   - **Group:** backend (with step 1)\n   - Add D"
        ),
        step(5, "E", "`src/e.rs`", Some("`none`."))
    );
    assert_eq!(
        steps(&plan)
            .iter()
            .map(|s| s.group.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("backend"),
            Some("backend"),
            Some("backend"),
            Some("backend"),
            None
        ]
    );
}
/// V02: the commands of a plan's Validation section, as checks: code
/// spans and code blocks that read as checks, not files, functions,
/// steps by hand or commands that never end.
#[test]
fn a_plans_validation_commands_are_its_checks() {
    let plan = "# Add users\n\n## Steps\n\n1. **Users**\n   - Files: `src/users.rs`\n   - Run `cargo fmt` there.\n\n\
        ## Validation\n\
        - `cargo test -p users` and `cargo clippy --all-targets`\n\
        - `npx tsc --noEmit`; `npm run test:unit`\n\
        - `python -m pytest tests/test_users.py`\n\
        - Add a test for `parse_args` in `tests/test_users.py`, as `test_parse_args()`\n\
        - `node check.js`, `./scripts/ci.sh check`\n\
        - Open /users by hand; `npm run dev` and `cargo run` to look; `vitest --watch`\n\
        - `cargo test -p users` again\n\
        ```sh\n$ go vet ./...\nmake test\n```\n";
    assert_eq!(
        validation_commands(plan),
        [
            "cargo test -p users",
            "cargo clippy --all-targets",
            "npx tsc --noEmit",
            "npm run test:unit",
            "python -m pytest tests/test_users.py",
            "node check.js",
            "./scripts/ci.sh check",
            "go vet ./...",
            "make test",
        ]
    );
    // No section, an empty one, or nothing that runs: none.
    assert!(validation_commands("# Plan\n\n## Steps\n1. x\n").is_empty());
    assert!(validation_commands("# Plan\n\n## Validation\n\n## Decisions\n- x\n").is_empty());
    assert!(
        validation_commands("## Validation\n- Check it by hand.\n- `ok.txt` says ok\n").is_empty()
    );
    assert!(validation_commands("## Validation\n- ``\n- `   `\n- `` ` ``\n").is_empty());
    assert_eq!(
        [
            "npx tsc --noEmit",
            "cargo clippy",
            "go vet ./...",
            "npm run build",
            "cargo test",
            "node check.js"
        ]
        .map(validation_kind),
        ["typecheck", "lint", "lint", "build", "test", "test"]
    );
}

/// Balanced Autopilot: `normalize` fixes `../` and bare names only on
/// `Files:` lines, the criteria's parts, an open fence and the title.
#[test]
fn normalize_fixes_only_mechanical_problems() {
    let tree = Tree::new(&["src/api/users.rs", "src/db/pool.rs", "a/x.rs", "b/x.rs"]);
    let plan = "## Steps\n\n1. Change users\n   Files: `../../src/api/users.rs`, `pool.rs`, `x.rs`, `new.rs` (new)\n   See ../../src/api/users.rs and pool.rs in prose.\n2. Add the pool\n   - Files:\n     - ../src/db/pool.rs\n\n## Validation\n- `cargo test`\n\n## Criteria\n\n1. Users load.\n\n```ts\nlet x = 1;\n";
    let (fixed, fixes) = normalize(plan, Some(&tree.0), Some("Fix the users page\nmore"));
    assert!(
        fixed.starts_with("# Fix the users page\n\n## Steps"),
        "{fixed}"
    );
    assert!(
        fixed.contains("Files: `src/api/users.rs`, `src/db/pool.rs`, `x.rs`, `new.rs` (new)"),
        "{fixed}"
    );
    // Prose is never touched.
    assert!(fixed.contains("See ../../src/api/users.rs and pool.rs in prose."));
    assert!(fixed.contains("     - src/db/pool.rs\n"), "{fixed}");
    assert!(fixed.contains("**Out of scope:** None."));
    assert!(fixed.contains("**Assumptions:** None."));
    assert_eq!(fixed.lines().filter(|l| is_fence(l)).count(), 2, "{fixed}");
    assert_eq!(fixes.len(), 7, "{fixes:?}");
    let c = criteria(&fixed).unwrap();
    assert!(c.has_out_of_scope && c.has_assumptions);
    assert_eq!(c.items, vec!["Users load.".to_string()]);
    // Without a request, no title; without a root, no bare-name fix.
    let (fixed, _) = normalize(plan, None, None);
    assert!(fixed.starts_with("## Steps"));
    assert!(fixed.contains("`pool.rs`"));
    // A plan with nothing to fix is returned as it was.
    let fine = "# T\r\n\r\n## Steps\r\n1. A\r\n   Files: `src/a.rs`\r\n";
    assert_eq!(normalize(fine, None, None), (fine.to_string(), Vec::new()));
}

/// `skeleton` keeps the title, the steps' files, and marks what's missing.
#[test]
fn a_skeleton_keeps_the_frame_and_marks_what_is_missing() {
    let plan = "# Users\n\n## Steps\n1. Load the users\n   Files: `src/a.rs`, `src/b.rs` (new), `../x`\n2. Show them\n   no files here\n";
    let s = skeleton(plan);
    assert!(s.starts_with("# Users\n"), "{s}");
    assert!(
        s.contains("1. Load the users\n   Files: `src/a.rs`, `src/b.rs` (new), TODO: fix `../x`"),
        "{s}"
    );
    assert!(s.contains("2. Show them\n   Files: TODO:"), "{s}");
    assert!(s.contains("## Validation\n\nTODO:"));
    assert!(s.contains("## Criteria\n\n1. TODO:"));
    let s = skeleton("just text");
    assert!(s.starts_with("# TODO: title"));
    assert!(s.contains("1. TODO: step title"));
}

/// The checker and execution agree on the steps, including tolerant forms.
#[test]
fn checks_and_steps_read_the_same_steps_from_every_plan() {
    let headings = regex::Regex::new(r"(?m)^(\d+)\. (.+)$").unwrap();
    let mut samples = vec![
        GOOD_PLAN.to_string(),
        REAL_PLAN.to_string(),
        CRITERIA_PLAN.to_string(),
    ];
    for heading in [
        "## 3. Steps",
        "## Next steps",
        "## Steps:",
        "## Steps (ordered)",
    ] {
        samples.push(REAL_PLAN.replace("## Steps", heading));
    }
    samples.extend([
        REAL_PLAN
            .replace("2. **", "1. **")
            .replace("3. **", "1. **"),
        REAL_PLAN
            .replace("2. **", "4. **")
            .replace("3. **", "7. **"),
        headings
            .replace_all(GOOD_PLAN, "### Step $1: $2")
            .into_owned(),
        headings
            .replace_all(GOOD_PLAN, "**Step $1: $2**")
            .into_owned(),
        GOOD_PLAN.replace("\n2. API", "\n   ```rs\n1. This is code\n   ```\n2. API"),
        GOOD_PLAN.replace("\n2. API", "\n   1. Nested list\n2. API"),
        GOOD_PLAN.replace('\n', "\r\n"),
    ]);
    for sample in samples {
        // Removing the file labels makes the checker name every step.
        let no_files: String = sample
            .replace("\r\n", "\n")
            .split_inclusive('\n')
            .filter(|line| !super::steps::FILES_LINE.is_match(line))
            .collect();
        let parsed = PlanDoc::parse(&no_files);
        let steps = parsed.steps();
        if steps.len() < 2 {
            continue;
        }
        let expected: Vec<_> = steps.iter().map(PlanStep::named).collect();
        let names: Vec<_> = expected.iter().map(String::as_str).collect();
        let problem = parsed
            .plan_problems()
            .into_iter()
            .find(|p| p.contains("name no file"))
            .unwrap();
        assert!(
            problem.starts_with(&format!(
                "Steps {} name no file",
                super::check::list_steps(&names)
            )),
            "{problem}\n{sample}"
        );
        for step in &steps {
            assert!(
                parsed.text().contains(&step.body),
                "step body must stay as written"
            );
        }
    }
}
