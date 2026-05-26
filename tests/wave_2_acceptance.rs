//! Wave 2 — Python runtime adapter acceptance criteria.

#[test]
#[ignore = "wave 2 not yet shipped: runtime = python accepted by Config::validate"]
fn python_runtime_accepted_by_config_validate() {
    // Implementer fills in during Wave 2 Task 4.
}

#[test]
#[ignore = "wave 2 not yet shipped: handler = app.lambda_handler resolves to app.py + lambda_handler attribute"]
fn python_handler_syntax_resolves_to_file_and_attribute() {
    // Implementer fills in during Wave 2 Task 2.
}

#[test]
#[ignore = "wave 2 not yet shipped: python3 subprocess spawned per concurrency slot"]
fn python_subprocess_spawned_per_concurrency_slot() {
    // Implementer fills in during Wave 2 Task 3.
}

#[test]
#[ignore = "wave 2 not yet shipped: adapter reads event per line, invokes handler(event, context), writes AWS-shaped response"]
fn python_adapter_line_protocol_roundtrip() {
    // Implementer fills in during Wave 2 Task 1.
}

#[test]
#[ignore = "wave 2 not yet shipped: context exposes function_name, aws_request_id, get_remaining_time_in_millis"]
fn python_context_surface_matches_bun_context() {
    // Implementer fills in during Wave 2 Task 3.
}

#[test]
#[ignore = "wave 2 not yet shipped: python adapter embedded in binary via include_str! written to ~/.riz/python-adapter.py on first run"]
fn python_adapter_extracted_to_riz_dir_on_first_run() {
    // Implementer fills in during Wave 2 Task 2.
}

#[test]
#[ignore = "wave 2 not yet shipped: examples/lambdas/echo-python/main.py ships with working function block in examples/riz.dev.toml"]
fn python_echo_example_exists_and_config_valid() {
    // Implementer fills in during Wave 2 Task 9.
}

#[test]
#[ignore = "wave 2 not yet shipped: integration test covers happy path + error path (gated on python3 presence)"]
fn python_integration_happy_and_error_paths() {
    // Implementer fills in during Wave 2 Task 7.
}
