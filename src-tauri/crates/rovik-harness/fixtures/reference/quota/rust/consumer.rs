#[path = "/snapshot/implementations/quota/lib.rs"]
mod quota;
use quota::{Amount, Quota, ReserveError};

// This consumer belongs to the protected oracle, not the implementation snapshot.
fn main() -> std::io::Result<()> {
    let cases = [
        ("reserve-success", 7, 3, Ok(()), 4),
        ("reserve-exact", 7, 7, Ok(()), 0),
        (
            "reserve-insufficient",
            7,
            8,
            Err(ReserveError::Insufficient),
            7,
        ),
        ("reserve-zero", 7, 0, Err(ReserveError::InvalidAmount), 7),
        ("reserve-empty", 0, 1, Err(ReserveError::Insufficient), 0),
        ("reserve-maximum", u64::MAX, u64::MAX, Ok(()), 0),
        (
            "reserve-insufficient-maximum",
            1,
            u64::MAX,
            Err(ReserveError::Insufficient),
            1,
        ),
    ];
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for &(id, before, raw, expected, after) in &cases {
        let observed = std::panic::catch_unwind(|| {
            let mut value = Quota::new(before);
            let result = Amount::new(raw).and_then(|a| value.reserve(a));
            result == expected && value.remaining() == after
        })
        .unwrap_or(false);
        if observed {
            passed.push(id);
        } else {
            failed.push(id);
        }
    }
    let array = |ids: Vec<&str>| {
        format!(
            "[{}]",
            ids.iter()
                .map(|id| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let all = array(cases.iter().map(|c| c.0).collect());
    let failures = failed.len();
    std::fs::write("/outputs/cases.json",format!("{{\"schema_version\":1,\"discovered\":{all},\"selected\":{all},\"passed\":{},\"failed\":{},\"skipped\":[]}}",array(passed),array(failed)))?;
    if failures != 0 {
        std::process::exit(1);
    }
    Ok(())
}
