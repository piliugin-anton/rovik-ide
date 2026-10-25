use super::*;
use std::path::Path;

/// A project's compose file as Compose prints it: a database and a cache
/// with published ports, a mail catcher with none, and the app built from
/// the project's Dockerfile.
const SHOP: &str = r#"{
  "name": "shop",
  "services": {
    "web": {"build": {"context": "/p/shop"}, "ports": [{"target": 3000, "published": "3000", "protocol": "tcp"}]},
    "db": {"image": "postgres:16-alpine", "ports": [{"target": 5432, "published": "5432", "protocol": "tcp"}]},
    "cache": {"image": "redis:7-alpine", "depends_on": {"db": {"condition": "service_started"}},
              "ports": [{"target": 6379, "published": "6379", "protocol": "tcp"}]},
    "mail": {"image": "axllent/mailpit"}
  }
}"#;

fn shop(file: &str) -> Arc<Model> {
    Arc::new(Model::parse(Path::new("/p/shop"), file, SHOP).unwrap())
}

/// A runner that's never asked: where only the words are tested.
struct Idle;

impl Runner for Idle {
    fn start(
        &self,
        _chat: String,
        _model: Arc<Model>,
        _names: Vec<String>,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Err("not asked".to_string()) })
    }
}

#[test]
fn the_prompt_names_the_backing_services_and_where_the_code_reaches_them() {
    let line = prompt_line_for(&[shop("compose.yaml")], None).unwrap();
    assert!(
        line.starts_with(
            "Services (compose.yaml): `cache` (redis:7-alpine, localhost:6379), `db` \
             (postgres:16-alpine, localhost:5432), `mail` (axllent/mailpit, no port published)."
        ),
        "{line}"
    );
    assert!(
        !line.contains("`web`"),
        "the app is the agent's to run: {line}"
    );
    // On the user's own computer the agent asks them.
    assert!(
        line.contains("ask the user to start it (`docker compose up -d`)"),
        "{line}"
    );
}

#[test]
fn the_prompt_says_what_this_machine_does_with_the_services() {
    let runs = Host::Runs(Arc::new(Idle));
    let line = prompt_line_for(&[shop("compose.yaml")], Some(&runs)).unwrap();
    assert!(line.contains("Rovik starts that service"), "{line}");
    assert!(
        line.contains("a `docker compose up` has Rovik start what it names instead"),
        "{line}"
    );
    assert!(
        line.contains("not even with the server's `podman`, which is Rovik's"),
        "the `podman` the agent finds: {line}"
    );
    assert!(line.contains("at localhost, not by their names"), "{line}");
    let cannot = Host::Cannot("Podman isn't installed".into());
    let line = prompt_line_for(&[shop("compose.yaml")], Some(&cannot)).unwrap();
    assert!(line.contains("This server can't run them"), "{line}");
}

#[test]
fn a_project_without_backing_services_has_no_line() {
    let only_app = r#"{"name": "a", "services": {"web": {"build": {"context": "/a"}}}}"#;
    let model = Arc::new(Model::parse(Path::new("/a"), "compose.yaml", only_app).unwrap());
    assert_eq!(prompt_line_for(&[model], None), None);
    assert_eq!(prompt_line_for(&[], None), None);
}

#[test]
fn the_prompt_names_a_few_services_and_counts_the_rest() {
    let many: String = (0..9)
        .map(|i| format!(r#""s{i}": {{"image": "redis"}}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let json = format!(r#"{{"name": "m", "services": {{{many}}}}}"#);
    let model = Arc::new(Model::parse(Path::new("/m"), "compose.yaml", &json).unwrap());
    let line = prompt_line_for(&[model], None).unwrap();
    assert!(
        line.contains("`s5` (redis, no port published) and 3 more."),
        "{line}"
    );
}

#[test]
fn a_refused_port_or_a_looked_up_name_is_the_service_that_publishes_it() {
    let model = shop("compose.yaml");
    assert_eq!(
        needed(&model, &[5432, 9999], &[]),
        [Needed {
            name: "db".into(),
            ports: vec![5432],
            by_name: false
        }]
    );
    // Looked up by name and refused at its port: one service, both said.
    let both = needed(&model, &[6379], &["cache".into(), "db".into()]);
    assert_eq!(
        both,
        [
            Needed {
                name: "cache".into(),
                ports: vec![6379],
                by_name: true
            },
            Needed {
                name: "db".into(),
                ports: vec![],
                by_name: true
            }
        ]
    );
    // The app's own port, or its name, is no service's.
    assert_eq!(needed(&model, &[3000], &["web".into()]), []);
}

#[test]
fn on_the_users_computer_the_agent_asks_them_to_start_the_service() {
    let model = shop("compose.yaml");
    let note = not_started_note(&model, &needed(&model, &[5432], &[]), NotStarted::Here);
    assert_eq!(
        note,
        "The command needed the `db` (postgres:16-alpine) service of compose.yaml, which isn't \
         running. Your commands can't start containers here: ask the user to start it \
         (`docker compose up -d db`), then run the command again."
    );
    // A compose file one level down is started in its folder.
    let model = shop("api/compose.yaml");
    let note = not_started_note(
        &model,
        &needed(&model, &[5432, 6379], &[]),
        NotStarted::Here,
    );
    assert!(
        note.contains("the `db` (postgres:16-alpine) and `cache` (redis:7-alpine) services of api/compose.yaml"),
        "{note}"
    );
    assert!(
        note.contains("(`cd api && docker compose up -d db cache`)"),
        "{note}"
    );
}

#[test]
fn a_server_that_cant_run_them_and_plan_mode_say_why() {
    let model = shop("compose.yaml");
    let wanted = needed(&model, &[5432], &[]);
    let note = not_started_note(
        &model,
        &wanted,
        NotStarted::Cannot("Podman isn't installed"),
    );
    assert!(
        note.ends_with(
            "this server can't run the project's services (Podman isn't installed). Tell the user."
        ),
        "{note}"
    );
    let note = not_started_note(&model, &wanted, NotStarted::PlanMode);
    assert!(note.contains("Plan mode starts nothing"), "{note}");
}

#[test]
fn a_service_looked_up_by_its_name_is_pointed_at_localhost() {
    let model = shop("compose.yaml");
    let wanted = needed(&model, &[], &["db".into()]);
    assert_eq!(
        started(&model, &wanted, "at localhost:5432"),
        "Rovik started the `db` (postgres:16-alpine) service of compose.yaml, which the command \
         needed (at localhost:5432)"
    );
    assert_eq!(
        by_name_note(&model, &wanted).unwrap(),
        "The command looked a service up by its name, which only the project's other \
         containers can: its code reaches `db` at localhost:5432 instead, so point it there (in \
         its `.env` or the variable it reads, such as DATABASE_URL) for this run."
    );
    assert_eq!(by_name_note(&model, &needed(&model, &[5432], &[])), None);
    assert!(
        not_started_because(&model, &wanted, "the image couldn't be pulled").ends_with(
            "which isn't running, and Rovik couldn't start it: the image couldn't be pulled"
        ),
    );
}

#[test]
fn a_compose_up_asks_for_the_services_that_run_from_their_own_image() {
    let model = shop("compose.yaml");
    let names =
        |needed: Vec<Needed>| -> Vec<String> { needed.into_iter().map(|n| n.name).collect() };
    assert_eq!(names(asked(&model, &["db".into()])), ["db"]);
    assert_eq!(
        names(asked(&model, &[])),
        ["cache", "db", "mail"],
        "all of them"
    );
    assert!(
        asked(&model, &["web".into()]).is_empty(),
        "the project's own code is the agent's to run"
    );
}

#[test]
fn a_command_that_reached_for_a_container_engine_is_told_what_runs_the_services() {
    let runs = Host::Runs(Arc::new(Idle));
    let listed = engine_note(&[shop("compose.yaml")], &[], Some(&runs));
    assert!(
        listed.starts_with(
            "Your commands can't run containers on this server, not even with its `podman`, \
             which is Rovik's: their sandbox keeps every engine from them."
        ),
        "{listed}"
    );
    assert!(
        listed.contains("compose.yaml declares `cache` (redis:7-alpine, localhost:6379)"),
        "{listed}"
    );
    assert!(
        listed.ends_with(
            "`web` is built from the project's code: run that code with your commands, not in \
             a container."
        ),
        "{listed}"
    );
    assert!(
        listed.contains("`docker compose up -d` with the service's name"),
        "{listed}"
    );
    let why = "Compose couldn't read compose.yaml: yaml: line 3: mapping values are not allowed";
    let unread = engine_note(&[], &[why.to_string()], Some(&runs));
    assert!(
        unread.contains(&format!("this project's: {why}")),
        "{unread}"
    );
    let none = engine_note(&[], &[], Some(&runs));
    assert!(none.contains("declare one in compose.yaml"), "{none}");
    let only_app = r#"{"name": "a", "services": {"web": {"build": {"context": "/a"}}}}"#;
    let app = Arc::new(Model::parse(Path::new("/a"), "docker-compose.yml", only_app).unwrap());
    let app = engine_note(&[app], &[], Some(&runs));
    assert!(
        app.contains(
            "from an image of their own (a database, cache or queue), and starts each when a \
             command needs it: docker-compose.yml declares none. `web` is built from the \
             project's code: run that code with your commands, not in a container."
        ),
        "{app}"
    );
    let cannot = Host::Cannot("Podman isn't installed".into());
    let cannot = engine_note(&[shop("compose.yaml")], &[], Some(&cannot));
    assert!(
        cannot.ends_with("either: Podman isn't installed. Tell the user."),
        "{cannot}"
    );
    let here = engine_note(&[], &[], None);
    assert!(
        here.contains("Ask the user to start the services"),
        "{here}"
    );
}

#[test]
fn the_project_code_a_command_asked_for_is_the_agents_to_run() {
    let models = [shop("compose.yaml")];
    assert_eq!(
        built_note(&models, &[]).as_deref(),
        Some(
            "`web` is built from the project's code: run that code with your commands, not in a \
             container."
        )
    );
    assert_eq!(built_note(&models, &["db".into()]), None, "none asked for");
    let two = r#"{"name": "a", "services": {"web": {"build": {}}, "worker": {"build": {}}}}"#;
    let two = [Arc::new(
        Model::parse(Path::new("/a"), "compose.yaml", two).unwrap(),
    )];
    assert!(
        built_note(&two, &[])
            .unwrap()
            .starts_with("`web` and `worker` are built"),
        "{:?}",
        built_note(&two, &[])
    );
}

#[test]
fn the_files_a_command_names_are_a_models_with_only_its_overrides() {
    let model = shop("compose.yaml");
    let at = |path: &str| std::path::PathBuf::from(path);
    assert!(model.read_from(&[at("/p/shop/compose.yaml")]));
    assert!(model.read_from(&[
        at("/p/shop/compose.yaml"),
        at("/p/shop/compose.override.yaml")
    ]));
    assert!(
        !model.read_from(&[at("/p/shop/compose.yaml"), at("/p/shop/compose.prod.yaml")]),
        "another file changes what runs"
    );
    assert!(
        !model.read_from(&[at("/p/shop/compose.override.yaml")]),
        "an override alone isn't the file"
    );
    assert!(!model.read_from(&[at("/p/shop/docker-compose.yml")]));
    let below = Model::parse(Path::new("/p/shop/api"), "api/compose.yaml", SHOP).unwrap();
    assert!(below.read_from(&[at("/p/shop/api/compose.yaml")]));
    assert!(!below.read_from(&[at("/p/shop/compose.yaml")]));
}
