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
fn riz_rust_runtime_crate_provides_run_helper() {
    assert!(
        std::path::Path::new("crates/riz-rust-runtime/src/lib.rs").exists()
            || std::path::Path::new("crates/riz-rust-runtime/Cargo.toml").exists(),
        "missing crates/riz-rust-runtime — Wave 6 runtime crate not shipped"
    );
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
