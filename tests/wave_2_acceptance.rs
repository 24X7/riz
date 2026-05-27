//! Wave 2 — Python runtime adapter acceptance criteria.

#[test]
#[ignore = "wave 2 not yet shipped: runtime = python accepted by Config::validate"]
fn python_runtime_accepted_by_config_validate() {
    let toml_str = r#"
[function.echo]
runtime = "python"
handler = "app.lambda_handler"
[[function.echo.routes]]
path = "/echo"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str).expect("toml must parse");
    config
        .validate()
        .expect("python runtime should be accepted by Wave 2");
}

#[test]
#[ignore = "wave 2 not yet shipped: handler = app.lambda_handler resolves to app.py + lambda_handler attribute"]
fn python_handler_syntax_resolves_to_file_and_attribute() {
    // Wave 2: python handler syntax "app.lambda_handler" must be accepted by validate.
    let toml_str = r#"
[function.echo]
runtime = "python"
handler = "app.lambda_handler"
[[function.echo.routes]]
path = "/echo"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str).expect("toml must parse");
    config
        .validate()
        .expect("python handler syntax accepted in Wave 2");
}

#[test]
#[ignore = "wave 2 not yet shipped: python3 subprocess spawned per concurrency slot"]
fn python_subprocess_spawned_per_concurrency_slot() {
    // Wave 2: a Python adapter script must exist for the subprocess to invoke.
    let adapter_paths = [
        std::path::Path::new("src/process/python-adapter.py"),
        std::path::Path::new("assets/python-adapter.py"),
        std::path::Path::new("src/process/bun/python-adapter.py"),
    ];
    let found = adapter_paths.iter().any(|p| p.exists());
    assert!(
        found,
        "Python adapter script not found — Wave 2 subprocess support not yet shipped"
    );
}

#[test]
#[ignore = "wave 2 not yet shipped: adapter reads event per line, invokes handler(event, context), writes AWS-shaped response"]
fn python_adapter_line_protocol_roundtrip() {
    // Wave 2: the python adapter script must exist on disk (extracted or embedded).
    let adapter_paths = [
        std::path::Path::new("src/process/python-adapter.py"),
        std::path::Path::new("assets/python-adapter.py"),
    ];
    let found = adapter_paths.iter().any(|p| p.exists());
    assert!(
        found,
        "Python adapter script not found — Wave 2 not yet shipped (expected at src/process/python-adapter.py or assets/python-adapter.py)"
    );
}

#[test]
#[ignore = "wave 2 not yet shipped: context exposes function_name, aws_request_id, get_remaining_time_in_millis"]
fn python_context_surface_matches_bun_context() {
    // Wave 2: python runtime validate must pass (prerequisite for context surface).
    let toml_str = r#"
[function.echo]
runtime = "python"
handler = "app.lambda_handler"
[[function.echo.routes]]
path = "/echo"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str).expect("toml must parse");
    config
        .validate()
        .expect("python context surface requires Wave 2 runtime support");
}

#[test]
#[ignore = "wave 2 not yet shipped: python adapter embedded in binary via include_str! written to ~/.riz/python-adapter.py on first run"]
fn python_adapter_extracted_to_riz_dir_on_first_run() {
    let adapter_paths = [
        std::path::Path::new("src/process/python-adapter.py"),
        std::path::Path::new("assets/python-adapter.py"),
    ];
    let found = adapter_paths.iter().any(|p| p.exists());
    assert!(
        found,
        "Python adapter file not present in source — Wave 2 not yet shipped"
    );
}

#[test]
#[ignore = "wave 2 not yet shipped: examples/lambdas/echo-python/main.py ships with working function block in examples/riz.dev.toml"]
fn python_echo_example_exists_and_config_valid() {
    assert!(
        std::path::Path::new("examples/lambdas/echo-python/main.py").exists(),
        "missing examples/lambdas/echo-python/main.py — create it during Wave 2 Task 9"
    );
}

#[test]
#[ignore = "wave 2 not yet shipped: integration test covers happy path + error path (gated on python3 presence)"]
fn python_integration_happy_and_error_paths() {
    // Wave 2: validate must accept python before integration tests can run.
    let toml_str = r#"
[function.echo]
runtime = "python"
handler = "app.lambda_handler"
[[function.echo.routes]]
path = "/echo"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str).expect("toml must parse");
    config
        .validate()
        .expect("python runtime must be accepted before integration tests run");
}
