//! Guard: every example config in examples/*.toml must (a) parse as a riz
//! Config, (b) pass Config::validate(), and (c) contain no unknown top-level
//! tables. (c) is the drift guard — serde tolerates unknown keys, which is
//! exactly how a removed subsystem's block (e.g. the old `[datadog]` StatsD
//! config) rotted silently in the examples for weeks.

use std::path::PathBuf;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_configs() -> Vec<PathBuf> {
    let dir = workspace().join("examples");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read examples/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    out.sort();
    assert!(
        !out.is_empty(),
        "no examples/*.toml found — did the examples move?"
    );
    out
}

/// Top-level tables Config actually deserializes. Must be updated when a new
/// section ships — that's the point: an example using a key not in this list
/// is either ahead of the code or (worse) behind it.
const KNOWN_TOP_LEVEL: &[&str] = &[
    "server",
    "cache",
    "telemetry",
    "deploy",
    "aws",
    "auth",
    "cors",
    "gateway",
    "function",
];

#[test]
fn every_example_config_parses_and_validates() {
    for path in example_configs() {
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let cfg: riz::config::Config = toml::from_str(&src)
            .unwrap_or_else(|e| panic!("{} must parse as a riz Config: {e}", path.display()));
        cfg.validate()
            .unwrap_or_else(|e| panic!("{} must validate: {e}", path.display()));
    }
}

#[test]
fn no_example_config_carries_unknown_top_level_tables() {
    for path in example_configs() {
        let src = std::fs::read_to_string(&path).unwrap();
        let raw: toml::Value = toml::from_str(&src)
            .unwrap_or_else(|e| panic!("{} must be valid TOML: {e}", path.display()));
        let table = raw.as_table().expect("top level is a table");
        for key in table.keys() {
            assert!(
                KNOWN_TOP_LEVEL.contains(&key.as_str()),
                "{}: unknown top-level table [{key}] — either a stale block from a \
                 removed subsystem (delete it) or a new Config section missing from \
                 KNOWN_TOP_LEVEL in this guard (add it)",
                path.display()
            );
        }
    }
}
