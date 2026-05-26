//! Wave 7 — Code debt cleanup acceptance criteria.

#[test]
#[ignore = "wave 7 not yet shipped: 7.1 src/system/mcp.rs split into mcp/mod.rs + protocol.rs + tools.rs + encoding.rs"]
fn mcp_rs_split_into_submodules() {
    // Implementer fills in during Wave 7.1 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.2 src/process/mod.rs split into mod.rs + pool.rs + liveness.rs"]
fn process_mod_split_into_submodules() {
    // Implementer fills in during Wave 7.2 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.3 AppState.route_stats removed; RizState.functions is sole stats source"]
fn dual_stats_system_removed() {
    // Implementer fills in during Wave 7.3 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.4 ProcessHandler::invoke uses typed PoolError enum instead of string-contains classification"]
fn typed_pool_error_enum_in_process_handler() {
    // Implementer fills in during Wave 7.4 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.5 dispatch hot path reads FunctionState only, no config.read() per request"]
fn dispatch_hot_path_no_config_read_lock() {
    // Implementer fills in during Wave 7.5 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.6 multi_value_headers v1-flavor dropped; unified Response builder used everywhere"]
fn multi_value_headers_v1_flavor_dropped() {
    // Implementer fills in during Wave 7.6 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.7 src/runtime/response.rs provides json_response + text_response builders used by system handlers"]
fn response_builders_extracted_to_response_rs() {
    // Implementer fills in during Wave 7.7 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.8 hand-rolled format_aws_time replaced with chrono"]
fn format_aws_time_uses_chrono() {
    // Implementer fills in during Wave 7.8 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.9 cold-start bookkeeping extracted into spawn_with_cold_start_record helper"]
fn cold_start_bookkeeping_extracted_to_helper() {
    // Implementer fills in during Wave 7.9 tasks.
}

#[test]
#[ignore = "wave 7 not yet shipped: 7.10 TUI reads from watch channel snapshot instead of blocking RwLock on hot path"]
fn tui_reads_from_watch_channel_snapshot() {
    // Implementer fills in during Wave 7.10 tasks.
}
