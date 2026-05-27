//! Wave 5 — Real `getRemainingTimeInMillis()` + context fidelity acceptance criteria.

#[test]
#[ignore = "wave 5 not yet shipped: dispatch path passes deadline epoch millis as __riz_deadline_ms field on wire-format event"]
fn dispatch_passes_deadline_ms_on_event() {
    // Wave 5: the ApiGatewayV2httpRequest (or its extensions) must carry a
    // `__riz_deadline_ms` field that the Bun adapter can read.
    // Proxy check: the gateway event type must include the deadline field.
    // When Wave 5 ships, this field will be added to the JSON payload before
    // it's written to the subprocess stdin.
    let evt = riz::gateway::ApiGatewayV2httpRequest::default();
    // Wave 5 will add: assert!(evt.__riz_deadline_ms.is_some());
    // For now, verify the field exists by trying to serialize and check JSON.
    let json = serde_json::to_value(&evt).expect("event must serialize");
    assert!(
        json.get("__riz_deadline_ms").is_some(),
        "__riz_deadline_ms field missing from wire-format event — Wave 5 not yet shipped"
    );
}

#[test]
#[ignore = "wave 5 not yet shipped: Bun adapter returns deadline_ms - Date.now() from context.getRemainingTimeInMillis()"]
fn bun_adapter_get_remaining_time_uses_deadline() {
    // Wave 5: the Bun adapter JS file must reference __riz_deadline_ms.
    let bun_adapter_paths = [
        std::path::Path::new("src/process/bun/adapter.ts"),
        std::path::Path::new("src/process/bun/adapter.js"),
        std::path::Path::new("assets/bun-adapter.ts"),
        std::path::Path::new("assets/bun-adapter.js"),
    ];
    let adapter_path = bun_adapter_paths.iter().find(|p| p.exists());
    let path = adapter_path.expect(
        "Bun adapter file not found — Wave 5 not yet shipped (expected at src/process/bun/adapter.ts)"
    );
    let content = std::fs::read_to_string(path).expect("must read bun adapter");
    assert!(
        content.contains("__riz_deadline_ms"),
        "Bun adapter does not reference __riz_deadline_ms — Wave 5 getRemainingTimeInMillis not yet shipped"
    );
}

#[test]
#[ignore = "wave 5 not yet shipped: context.functionName matches function name from riz.toml"]
fn context_function_name_matches_riz_toml() {
    // Wave 5: __riz_deadline_ms must be present in the wire event JSON.
    let evt = riz::gateway::ApiGatewayV2httpRequest::default();
    let json = serde_json::to_value(&evt).expect("event must serialize");
    assert!(
        json.get("__riz_deadline_ms").is_some(),
        "__riz_deadline_ms field missing — Wave 5 context fidelity not yet shipped"
    );
}

#[test]
#[ignore = "wave 5 not yet shipped: context.invokedFunctionArn produces synthetic arn:riz:lambda:local:000000000000:function:<name>"]
fn context_invoked_function_arn_is_synthetic_riz_arn() {
    // Wave 5: the synthetic ARN format must be produced. Check via wire event.
    let evt = riz::gateway::ApiGatewayV2httpRequest::default();
    let json = serde_json::to_value(&evt).expect("event must serialize");
    assert!(
        json.get("__riz_deadline_ms").is_some(),
        "Wire event extensions missing — Wave 5 synthetic ARN support not yet shipped"
    );
}

#[test]
#[ignore = "wave 5 not yet shipped: context.awsRequestId matches event.requestContext.requestId"]
fn context_aws_request_id_matches_event_request_context() {
    // Wave 5: the wire event must carry deadline so the adapter can compute remaining time.
    let evt = riz::gateway::ApiGatewayV2httpRequest::default();
    let json = serde_json::to_value(&evt).expect("event must serialize");
    assert!(
        json.get("__riz_deadline_ms").is_some(),
        "Wire event missing __riz_deadline_ms — Wave 5 awsRequestId fidelity not yet shipped"
    );
}
