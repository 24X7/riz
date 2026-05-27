//! Wave 5 — Real `getRemainingTimeInMillis()` + context fidelity acceptance criteria.

#[test]
fn context_deadline_emitted_on_event() {
    // build_envelope_payload must emit __riz_deadline_ms as a positive epoch-ms integer.
    #[derive(serde::Serialize)]
    struct FakeEvent {
        path: &'static str,
    }
    let event = FakeEvent { path: "/test" };
    let json_str =
        riz::process::build_envelope_payload(&event, "api", 5000).expect("envelope must serialize");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("envelope must be valid JSON");

    let deadline = parsed["__riz_deadline_ms"]
        .as_i64()
        .expect("__riz_deadline_ms must be an integer");
    assert!(
        deadline > 0,
        "__riz_deadline_ms must be > 0, got {deadline}"
    );
}

#[test]
fn context_function_name_from_toml() {
    // The envelope must carry __riz_function_name matching the argument passed.
    #[derive(serde::Serialize)]
    struct FakeEvent {}
    let event = FakeEvent {};
    let json_str =
        riz::process::build_envelope_payload(&event, "api", 5000).expect("envelope must serialize");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("envelope must be valid JSON");

    assert_eq!(
        parsed["__riz_function_name"], "api",
        "__riz_function_name must match the function_name argument"
    );
}

#[test]
fn context_arn_is_synthetic_when_no_override() {
    // The Bun adapter must contain the synthetic ARN template string.
    let adapter_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/bun-adapter.mjs");
    let content =
        std::fs::read_to_string(&adapter_path).expect("assets/bun-adapter.mjs must be readable");
    assert!(
        content.contains("arn:riz:lambda:local:000000000000:function:${function_name}"),
        "Bun adapter must contain the synthetic ARN template — Wave 5 not yet shipped"
    );
}

#[test]
fn context_aws_request_id_matches_request_context() {
    // The Bun adapter must set awsRequestId from event.requestContext.requestId.
    let adapter_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/bun-adapter.mjs");
    let content =
        std::fs::read_to_string(&adapter_path).expect("assets/bun-adapter.mjs must be readable");
    assert!(
        content.contains("event?.requestContext?.requestId"),
        "Bun adapter must reference event?.requestContext?.requestId for awsRequestId"
    );
}

#[test]
fn context_remaining_time_uses_deadline() {
    // The Bun adapter must compute getRemainingTimeInMillis via the deadline.
    let adapter_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/bun-adapter.mjs");
    let content =
        std::fs::read_to_string(&adapter_path).expect("assets/bun-adapter.mjs must be readable");
    assert!(
        content.contains("Math.max(0, deadline_ms - Date.now())"),
        "Bun adapter must use Math.max(0, deadline_ms - Date.now()) for getRemainingTimeInMillis"
    );
}
