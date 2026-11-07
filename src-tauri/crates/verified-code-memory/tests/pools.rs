mod support {
    pub mod pools;
}

#[test]
fn candidate_pool_capacity_counts_distinct_visible_claims() {
    let report = support::pools::evaluate();
    for case in report["cases"].as_array().unwrap() {
        assert_eq!(case["recalled"], case["required"], "{case}");
        assert_eq!(case["delivered"], case["required"], "{case}");
        assert_eq!(case["returned"], case["required"], "{case}");
        assert_eq!(case["safe"], true, "{case}");
        assert_eq!(case["stable"], true, "{case}");
        assert_eq!(case["history_unchanged"], true, "{case}");
        assert!(case["packet_tokens"].as_u64().unwrap() <= 1000, "{case}");
    }
}
