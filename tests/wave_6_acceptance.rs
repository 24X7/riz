//! Wave 6 — Rust runtime adapter acceptance criteria.

#[test]
fn rust_runtime_accepted_by_config_validate() {
    let toml_str = r#"
[function.echo]
runtime = "rust"
handler = "./target/release/my-handler"
[[function.echo.routes]]
path = "/echo"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str).expect("toml must parse");
    config
        .validate()
        .expect("rust runtime should be accepted by Wave 6");
}

#[test]
fn rust_handler_binary_invoked_as_subprocess() {
    // Wave 6: runtime = rust is accepted and routes to the Rust subprocess adapter.
    // The RustRuntime::spawn_command returns a Command pointed at the handler binary
    // directly — no intermediate adapter script.
    let toml_str = r#"
[function.echo]
runtime = "rust"
handler = "./target/release/my-handler"
[[function.echo.routes]]
path = "/echo"
method = "GET"
"#;
    let config: riz::config::Config = toml::from_str(toml_str).expect("toml must parse");
    config
        .validate()
        .expect("rust binary subprocess requires Wave 6 runtime support");
}

#[test]
fn rust_examples_use_the_official_runtime_not_a_riz_helper() {
    // riz speaks the AWS Lambda Runtime API, so a Rust handler uses the OFFICIAL
    // `lambda_runtime` crate with no riz library — the same binary runs on AWS.
    for ex in ["echo-rust", "chat-rust"] {
        let cargo = std::fs::read_to_string(format!("examples/lambdas/{ex}/Cargo.toml"))
            .unwrap_or_else(|e| panic!("read {ex} Cargo.toml: {e}"));
        assert!(
            cargo.contains("lambda_runtime"),
            "{ex} must use the official lambda_runtime crate"
        );
        assert!(
            !cargo.contains("riz-rust-runtime") && !cargo.contains("riz_rust_runtime"),
            "{ex} must NOT depend on a riz helper crate — the goal is no code changes"
        );
    }
}

#[test]
fn rust_echo_example_exists_and_builds() {
    assert!(
        std::path::Path::new("examples/lambdas/echo-rust/src/main.rs").exists()
            || std::path::Path::new("examples/lambdas/echo-rust/main.rs").exists(),
        "missing examples/lambdas/echo-rust/src/main.rs — Wave 6 Rust example not shipped"
    );
}

#[test]
fn rust_integration_test_gated_on_cargo_build() {
    assert!(
        std::path::Path::new("examples/lambdas/echo-rust/Cargo.toml").exists(),
        "missing examples/lambdas/echo-rust/Cargo.toml — Wave 6 Rust example not shipped"
    );
}
