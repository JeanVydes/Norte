use norte_core::{
    ContinuityFragment, ContinuityScope, ContinuitySemantics, Model, ModelRef, ReplayDecision,
};

/// Runs the [`Model`] contract every adapter must satisfy (FR-TEST-1).
///
/// The suite is socket-free: it checks the trait surface an adapter reports and
/// the determinism of its [`norte_core::ContinuityAdapter`], so it can run
/// in the hermetic test suite against every wire.
///
/// # Panics
///
/// Panics when the model violates the contract.
pub fn model_conformance(model: &dyn Model, expected: &ModelRef) {
    assert_eq!(
        model.reference(),
        expected,
        "the model must report the reference it was built with"
    );

    let capabilities = model.capabilities();
    assert!(
        !capabilities.modalities.input.is_empty(),
        "a model must accept at least one input modality"
    );
    assert!(
        !capabilities.modalities.output.is_empty(),
        "a model must produce at least one output modality"
    );

    let continuity = model.continuity();
    let wire = serde_json::json!({ "opaque": "state" });
    let token = continuity.build(&wire).expect("build a continuity token");
    assert_eq!(
        token,
        continuity.build(&wire).expect("build a continuity token"),
        "building a token from the same wire state must be deterministic"
    );
    assert_eq!(
        continuity.replay(expected, &[], &token),
        ReplayDecision::Replay,
        "a token built for this model must replay for it"
    );

    let fragment = ContinuityFragment::new(
        token.family.clone(),
        ContinuityScope::Exact(expected.clone()),
        token.schema_version,
        ContinuitySemantics::Append,
        b"tail".to_vec(),
    );
    let merged = continuity
        .merge(Some(token.clone()), fragment.clone())
        .expect("merge a fragment");
    assert_eq!(
        merged,
        continuity
            .merge(Some(token), fragment)
            .expect("merge a fragment"),
        "merging the same fragment must be deterministic"
    );
    assert!(merged.payload().ends_with(b"tail"));
}
