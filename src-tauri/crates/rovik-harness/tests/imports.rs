mod support;
use rovik_harness::{
    fixtures,
    identity::Digest,
    ledger::Ledger,
    pack_protocol::{
        self, Edge, ForbiddenEdge, Graph, NativeInput, Node, Operation, Precision, Request,
    },
    profile::Language,
};

fn input(ledger: &Ledger, language: Language) -> NativeInput {
    let setup =
        fixtures::quota_runtime(ledger, "imports", Digest::of(b"image"), "base", language).unwrap();
    NativeInput {
        snapshot_handle: "snapshot:active".into(),
        configuration_id: setup.profiles[0].configurations[0].id.clone(),
        profile: setup.profiles[0].clone(),
        forbidden_edges: vec![ForbiddenEdge {
            source: "domain/**".into(),
            target: "infrastructure/**".into(),
        }],
        required_test_ids: Default::default(),
        typescript_project: (language == Language::TypeScript).then(|| "compiler.json".into()),
    }
}

#[test]
fn runtime_boundaries_cover_every_module_and_reject_transitive_edges_and_incomplete_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    for language in [Language::JavaScript, Language::TypeScript, Language::Python] {
        let input = input(&ledger, language);
        let mut graph = Graph {
            precision: Precision::ProjectImports,
            nodes: [
                "domain/new",
                "shared/api",
                "infrastructure/store",
                "domain-other/outside",
            ]
            .iter()
            .map(|id| Node {
                id: (*id).into(),
                path: Some((*id).into()),
                kind: "module".into(),
            })
            .collect(),
            edges: vec![Edge {
                source: "domain/new".into(),
                target: "shared/api".into(),
                kind: "import".into(),
            }],
            unresolved: vec![],
            configuration_digest: input.profile.configurations[0].digest().unwrap(),
            compiler_commands: vec![],
            test_inventory: vec![],
        };
        graph.validate(&input).unwrap();
        graph.edges.push(Edge {
            source: "shared/api".into(),
            target: "infrastructure/store".into(),
            kind: "import".into(),
        });
        assert!(graph
            .validate(&input)
            .unwrap_err()
            .message
            .contains("forbidden dependency"));
        graph.edges[0].source = "domain-other/outside".into();
        graph.validate(&input).unwrap(); // A directory boundary cannot match a sibling prefix.
        let request = Request {
            protocol_version: 1,
            request_id: "imports".into(),
            operation: Operation::Index,
            payload: serde_json::to_value(&input).unwrap(),
        };
        let versions: std::collections::BTreeMap<_, _> = input
            .profile
            .facts
            .iter()
            .filter(|(name, _)| name.ends_with("_version"))
            .collect();
        let reply = serde_json::json!({"protocol_version":1,"request_id":"imports","ok":true,"result":{"language":language,"graph":graph,"versions":versions}});
        let encode = |value: &serde_json::Value| format!("{value}\n").into_bytes();
        pack_protocol::verify_index(&request, &encode(&reply), &graph.configuration_digest)
            .unwrap();
        for pointer in [
            "/result/versions",
            "/result/graph/precision",
            "/result/graph/configuration_digest",
        ] {
            let mut bad = reply.clone();
            *bad.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
            assert!(pack_protocol::verify_index(
                &request,
                &encode(&bad),
                &graph.configuration_digest
            )
            .is_err());
        }
        let mut bad = reply.clone();
        bad["result"]["versions"] = serde_json::json!({});
        assert!(
            pack_protocol::verify_index(&request, &encode(&bad), &graph.configuration_digest)
                .unwrap_err()
                .message
                .contains("versions")
        );
        graph.nodes.retain(|n| !n.id.starts_with("domain/"));
        assert!(graph
            .validate(&input)
            .unwrap_err()
            .message
            .contains("source disappeared"));
    }
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the rebuilt pinned native image"]
fn real_runtime_imports_enforce_layers_reexports_initializers_aliases_and_resolution_failures() {
    use rovik_harness::{
        packs,
        plan::{CheckSpec, Phase},
        registry::CommandRegistry,
        runner::{Cancellation, DockerBackend},
    };
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
    for language in [Language::TypeScript, Language::JavaScript, Language::Python] {
        for variant in [
            "allowed",
            "direct",
            "transitive",
            "alias",
            "unresolved",
            "dynamic",
            "external",
            "conditional",
            "nested",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let ledger = Ledger::open(&dir.path().join("state")).unwrap();
            let mut setup =
                fixtures::quota_runtime(&ledger, "imports", image.clone(), "base", language)
                    .unwrap();
            eprintln!("Checking {language:?}: {variant}");
            let mut input = input(&ledger, language);
            input.profile = setup.profiles[0].clone();
            let python = language == Language::Python;
            let typescript = language == Language::TypeScript;
            let ext = if python {
                "py"
            } else if typescript {
                "ts"
            } else {
                "mjs"
            };
            let import_ext = if typescript { "js" } else { ext };
            let direct = if python {
                "from infrastructure.store import reserve\n".into()
            } else {
                format!("export {{ reserve }} from '../infrastructure/store.{import_ext}';")
            };
            let mut domain = if python {
                "from shared.api import reserve\n".into()
            } else {
                format!("export {{ reserve }} from '../shared/api.{import_ext}';")
            };
            let mut shared = if python {
                "def reserve(): return 1\n".into()
            } else {
                "export const reserve = () => 1;".into()
            };
            let mut init = "";
            match variant {
                "direct" | "nested" => domain = direct.clone(),
                "transitive" => shared = direct.clone(),
                "alias" if python => init = "from infrastructure.store import reserve\n",
                "alias" if typescript => domain = "import type { Marker } from '@infra/store.js'; import { type Marker as Inline } from '@infra/store.js'; type Queried = import('@infra/store.js').Marker; export const value: Marker & Inline & Queried = 1;".into(),
                "conditional" if typescript => domain = "export { reserve } from '#conditional';".into(),
                "conditional" if python => domain = "if False:\n    import infrastructure.store\n".into(),
                "conditional" => domain = "if (false) { import('../infrastructure/store.mjs'); }".into(),
                "alias" => domain = "export { reserve } from '#infra';".into(),
                "unresolved" => domain = if python {"import missing_dependency\n"} else {"import './missing.js';"}.into(),
                "dynamic" => domain = if python {"from importlib import import_module as load\nload('infrastructure.store')\n"} else {"const name = '../infrastructure/store.mjs'; export const loaded = import(name);"}.into(),
                "external" => {
                    domain = if python {"import fixture_identity\n"} else {"import 'fixture-identity';"}.into();
                    input.forbidden_edges[0].target = if python {"python:fixture_identity"} else {"npm:fixture-identity"}.into();
                }
                _ => {}
            }
            if variant == "allowed" {
                shared.push_str(if python {
                    "\nraise RuntimeError('architecture checks must not import this module')\n"
                } else {
                    "\nthrow new Error('architecture checks must not execute this module');"
                });
            }
            let domain_path = format!("domain/order.{ext}");
            let shared_path = format!("shared/api.{ext}");
            let infra_path = format!("infrastructure/store.{ext}");
            let infra = if python {
                "def reserve(): return 2\n"
            } else if typescript {
                "export type Marker = number; export const reserve = () => 2;"
            } else {
                "export const reserve = () => 2;"
            };
            let mut files = vec![
                (domain_path.as_str(), domain.as_str()),
                (shared_path.as_str(), shared.as_str()),
                (infra_path.as_str(), infra),
            ];
            let mut manifest: serde_json::Value =
                serde_json::from_str(include_str!("../fixtures/runtime/package.json")).unwrap();
            manifest["imports"] =
                serde_json::json!({"#infra":format!("./infrastructure/store.{import_ext}")});
            if typescript {
                manifest["imports"]["#conditional"] = serde_json::json!({"types":"./shared/api.ts", "default":"./dist/infrastructure/store.js"});
            }
            let manifest = manifest.to_string();
            if python {
                files.extend([
                    ("shared/__init__.py", init),
                    ("__init__.py", "from infrastructure.store import reserve\n"),
                    (
                        "requirements.lock",
                        include_str!("../fixtures/runtime/requirements.lock"),
                    ),
                ]);
            } else {
                files.extend([
                    ("package.json", manifest.as_str()),
                    (
                        "package-lock.json",
                        include_str!("../fixtures/runtime/package-lock.json"),
                    ),
                ]);
            }
            let nested: Vec<_> = files
                .iter()
                .map(|(path, source)| (format!("frontend/{path}"), *source))
                .collect();
            let nested_refs: Vec<_> = nested
                .iter()
                .map(|(path, source)| (path.as_str(), *source))
                .collect();
            if variant == "nested" {
                input.profile.build_root = "frontend".into();
                input.forbidden_edges[0].source = "frontend/domain/**".into();
                input.forbidden_edges[0].target = "frontend/infrastructure/**".into();
            }
            let candidate = fixtures::snapshot(
                &ledger,
                "base",
                if variant == "nested" {
                    &nested_refs
                } else {
                    &files
                },
            )
            .unwrap();
            let request = Request {
                protocol_version: 1,
                request_id: "imports".into(),
                operation: Operation::Index,
                payload: serde_json::to_value(&input).unwrap(),
            };
            let request_text = format!("{}\n", serde_json::to_string(&request).unwrap());
            let compiler = serde_json::json!({"compilerOptions":{"module":"NodeNext","target":"ES2022","types":[],"paths":{"@infra/*":["/build/project/infrastructure/*"]}},"files":[format!("/build/project/{domain_path}"),format!("/build/project/{shared_path}"),format!("/build/project/{infra_path}")]}).to_string();
            let oracle = fixtures::snapshot(
                &ledger,
                "oracle",
                &[
                    ("requests/imports.json", request_text.as_str()),
                    ("compiler.json", compiler.as_str()),
                ],
            )
            .unwrap();
            let command = packs::graph_command(["imports".into()].into()).unwrap();
            let registry = CommandRegistry {
                commands: vec![command.clone()],
            };
            setup.environment.registry_digest = Digest::of_json(&registry).unwrap();
            let config = input.profile.configurations[0].digest().unwrap();
            let spec = CheckSpec {
                check_id: "imports".into(),
                configuration_digest: config.clone(),
                checker_digest: command.checker_digest,
                command_id: command.id,
                validated_parameters: [("request_id".into(), "imports".into())].into(),
                phase: Phase::Final,
                required: true,
                obligation_ids: vec![],
                expected_case_ids: vec![],
                minimum_test_count: 0,
                success_exit_codes: vec![0],
                timeout_ms: 180_000,
                output_limit_bytes: 4 * 1024 * 1024,
                dependencies: vec![],
            };
            let output = DockerBackend::local(&support::docker_path())
                .unwrap()
                .execute(
                    &ledger,
                    &candidate,
                    &oracle,
                    &setup.dependencies,
                    &setup.environment,
                    &registry,
                    &spec,
                    &Cancellation::default(),
                )
                .unwrap();
            assert!(
                !output.incomplete(),
                "{language:?} {variant}: {}",
                String::from_utf8_lossy(output.stderr())
            );
            let result = pack_protocol::verify_index(&request, output.stdout(), &config);
            if variant == "allowed" {
                result.unwrap_or_else(|e| {
                    panic!(
                        "{language:?} {variant}: {e}\n{}",
                        String::from_utf8_lossy(output.stderr())
                    )
                });
            } else {
                let error = result.unwrap_err();
                if !matches!(variant, "unresolved" | "dynamic") {
                    assert!(
                        error.message.contains("forbidden dependency"),
                        "{language:?} {variant}: {error}\n{}",
                        String::from_utf8_lossy(output.stderr())
                    );
                }
            }
        }
    }
}
