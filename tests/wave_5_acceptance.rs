//! Wave 5 — Real `getRemainingTimeInMillis()` + context fidelity acceptance criteria.

#[test]
#[ignore = "wave 5 not yet shipped: dispatch path passes deadline epoch millis as __riz_deadline_ms field on wire-format event"]
fn dispatch_passes_deadline_ms_on_event() {
    // Implementer fills in during Wave 5 tasks.
}

#[test]
#[ignore = "wave 5 not yet shipped: Bun adapter returns deadline_ms - Date.now() from context.getRemainingTimeInMillis()"]
fn bun_adapter_get_remaining_time_uses_deadline() {
    // Implementer fills in during Wave 5 tasks.
}

#[test]
#[ignore = "wave 5 not yet shipped: context.functionName matches function name from riz.toml"]
fn context_function_name_matches_riz_toml() {
    // Implementer fills in during Wave 5 tasks.
}

#[test]
#[ignore = "wave 5 not yet shipped: context.invokedFunctionArn produces synthetic arn:riz:lambda:local:000000000000:function:<name>"]
fn context_invoked_function_arn_is_synthetic_riz_arn() {
    // Implementer fills in during Wave 5 tasks.
}

#[test]
#[ignore = "wave 5 not yet shipped: context.awsRequestId matches event.requestContext.requestId"]
fn context_aws_request_id_matches_event_request_context() {
    // Implementer fills in during Wave 5 tasks.
}
