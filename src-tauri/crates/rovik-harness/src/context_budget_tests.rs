use super::*;
use crate::wire;

fn item(ledger: &Ledger, id: &str, text: &str, mandatory: bool) -> ContextItem {
    let artifact = ledger
        .store("context", "text/plain", text.as_bytes(), 0)
        .unwrap();
    ContextItem {
        item_id: id.into(),
        source_revision: artifact.digest.clone(),
        artifact_digest: artifact.digest,
        mandatory,
        trust: if mandatory {
            Trust::ApprovedSpec
        } else {
            Trust::ObservedCode
        },
        text: text.into(),
    }
}

#[test]
fn many_omitted_files_leave_room_for_ranked_source_and_an_exact_omission_record() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let required = item(
        &ledger,
        "contract",
        "Keep every approved requirement.",
        true,
    );
    let preferred = item(
        &ledger,
        "source:preferred.rs",
        "preferred implementation",
        false,
    );
    let other = item(&ledger, "source:other.rs", &"x".repeat(2048), false);
    let omitted: Vec<_> = (0..100)
        .map(|i| format!("source:directory with spaces/file-{i}.rs"))
        .collect();
    let mut input = vec![preferred.clone(), required.clone()];
    input.extend(omitted.iter().map(|id| ContextItem {
        item_id: id.clone(),
        ..other.clone()
    }));
    let bundle = build(&ledger, input, 1024).unwrap();
    let bytes = wire::canonical(&bundle).unwrap();
    assert!(bytes.len() <= 1024);
    assert_eq!(bundle.items.len(), 2);
    assert_eq!(bundle.items[0].text, required.text);
    assert_eq!(bundle.items[1].item_id, preferred.item_id);
    let value: serde_json::Value = wire::decode(&bytes).unwrap();
    let manifest = &value["omitted"];
    assert_eq!(manifest["count"], omitted.len());
    let digest: Digest = serde_json::from_value(manifest["manifest_digest"].clone()).unwrap();
    assert_eq!(
        wire::decode::<Vec<String>>(&ledger.read(&digest).unwrap()).unwrap(),
        omitted
    );
    assert_eq!(
        wire::canonical(&wire::decode::<ContextBundle>(&bytes).unwrap()).unwrap(),
        bytes
    );
}

#[test]
fn small_omission_lists_keep_their_v1_encoding_and_fit_the_exact_byte_limit() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let policy = item(&ledger, "contract", "Keep the approved scope.", true);
    let first = item(&ledger, "source:selected.rs", "quoted \"source\"\n", false);
    let other = item(
        &ledger,
        "source:quoted-\"雪.rs",
        &"omitted".repeat(256),
        false,
    );
    let original = serde_json::json!({
        "schema_version": 1, "items": [&policy, &first], "omitted": [&other.item_id]
    });
    let old_bytes = wire::canonical(&original).unwrap();
    let prior: ContextBundle = wire::decode(&old_bytes).unwrap();
    assert_eq!(wire::canonical(&prior).unwrap(), old_bytes);
    let inputs = vec![first.clone(), other.clone(), policy.clone()];
    let exact = build(&ledger, inputs.clone(), old_bytes.len()).unwrap();
    assert_eq!(wire::canonical(&exact).unwrap(), old_bytes);
    let smaller = build(&ledger, inputs, old_bytes.len() - 1).unwrap();
    assert_eq!(smaller.items.len(), 1);
    assert_eq!(smaller.items[0].text, policy.text);
    assert!(wire::canonical(&smaller).unwrap().len() < old_bytes.len());
    assert_eq!(
        serde_json::to_value(smaller.omitted).unwrap(),
        serde_json::json!([first.item_id, other.item_id])
    );
}

#[test]
fn limits_that_cannot_fit_required_inputs_or_the_smallest_omission_record_explain_why() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let mandatory = item(&ledger, "contract", "Approved requirements", true);
    let bundle = build(&ledger, vec![mandatory.clone()], 1024).unwrap();
    let size = wire::canonical(&bundle).unwrap().len();
    let error = build(&ledger, vec![mandatory.clone()], size - 1).unwrap_err();
    assert!(error.message.contains(&format!("need {size} bytes")));
    assert!(error
        .message
        .contains(&format!("limit is {} bytes", size - 1)));

    let optional = item(&ledger, "x", "code", false);
    let error = build(&ledger, vec![mandatory, optional], size).unwrap_err();
    assert!(error.message.contains("leave 0 bytes"));
    assert!(error.message.contains("smallest record needs 3 bytes"));
}

#[test]
fn an_optional_file_larger_than_the_wire_limit_can_be_skipped_but_its_provenance_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    // Quotes double in size when serialized, even though the source blob fits.
    let mut large = item(
        &ledger,
        "source:quoted.txt",
        &"\"".repeat(wire::MAX_JSON_BYTES / 2),
        false,
    );
    let bundle = build(&ledger, vec![large.clone()], 1024).unwrap();
    assert!(bundle.items.is_empty());
    assert_eq!(
        serde_json::to_value(bundle.omitted).unwrap(),
        serde_json::json!([large.item_id])
    );
    large.text.clear();
    assert!(build(&ledger, vec![large], 1024)
        .unwrap_err()
        .message
        .contains("stale context item"));
}
