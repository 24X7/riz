//! Wave 6 — Rust runtime adapter acceptance criteria.

#[test]
#[ignore = "wave 6 not yet shipped: runtime = rust accepted by Config::validate"]
fn rust_runtime_accepted_by_config_validate() {
    // Implementer fills in during Wave 6 tasks.
}

#[test]
#[ignore = "wave 6 not yet shipped: handler = ./target/release/my-handler invoked as subprocess speaking line-JSON protocol"]
fn rust_handler_binary_invoked_as_subprocess() {
    // Implementer fills in during Wave 6 tasks.
}

#[test]
#[ignore = "wave 6 not yet shipped: crates/riz-rust-runtime provides riz_rust_runtime::run(handler_fn) boilerplate"]
fn riz_rust_runtime_crate_provides_run_helper() {
    // Implementer fills in during Wave 6 tasks.
}

#[test]
#[ignore = "wave 6 not yet shipped: examples/lambdas/echo-rust ships with Cargo.toml + main.rs + sample build instructions"]
fn rust_echo_example_exists_and_builds() {
    // Implementer fills in during Wave 6 tasks.
}

#[test]
#[ignore = "wave 6 not yet shipped: integration test gated on cargo build succeeding for the echo-rust example"]
fn rust_integration_test_gated_on_cargo_build() {
    // Implementer fills in during Wave 6 tasks.
}
