//! Configuration fixture tests.
//!
//! Handwritten config files live in `tests/configs/`. A valid config is
//! parsed and compared verbatim against its expected JSON snapshot
//! (`<name>.json`) capturing the full effective config including serde
//! defaults. Configs that fail structural conversion are fed through
//! [`fyah::config_store::ConfigStore::new`] — the same path startup uses —
//! and must produce exactly the expected list of errors.

use fyah::config::{Config, ConfigStore, StartupError};

/// Parse a fixture file the way `Config::load` would, minus the XDG/default
/// config merge so tests stay hermetic.
fn fixture(name: &str) -> Config {
    let source = match name {
        "valid.toml" => include_str!("configs/valid.toml"),
        "empty.toml" => include_str!("configs/empty.toml"),
        "invalid-multiple-problems.toml" => include_str!("configs/invalid-multiple-problems.toml"),
        "invalid-two-workflows.toml" => include_str!("configs/invalid-two-workflows.toml"),
        _ => panic!("unknown fixture {name:?}"),
    };
    toml::from_str(source).expect("fixture must parse as a Config")
}

/// The expected JSON snapshot for a valid fixture.
fn expected_json(name: &str) -> serde_json::Value {
    let json = match name {
        "valid.toml" => include_str!("configs/valid.json"),
        "empty.toml" => include_str!("configs/empty.json"),
        _ => panic!("no JSON snapshot for fixture {name:?}"),
    };
    serde_json::from_str(json).expect("expected JSON snapshot must parse")
}

/// Latent from a failed startup: the structural problems of every failing
/// workflow, as user-facing strings. Sorted (HashMap iteration order is
/// arbitrary) so the comparison is deterministic.
fn error_list(err: &StartupError) -> Vec<(String, Vec<String>)> {
    let StartupError::InvalidConfig(workflows) = err else {
        panic!("expected InvalidConfig, got {err:?}");
    };
    let mut grouped: Vec<(String, Vec<String>)> = workflows
        .iter()
        .map(|wf| {
            let mut messages: Vec<String> = wf.errors().iter().map(ToString::to_string).collect();
            messages.sort();
            (wf.workflow().to_string(), messages)
        })
        .collect();
    grouped.sort_by(|a, b| a.0.cmp(&b.0));
    grouped
}

#[test]
fn valid_config_parses_to_expected_json() {
    let config = fixture("valid.toml");
    assert_eq!(
        serde_json::to_value(&config).unwrap(),
        expected_json("valid.toml")
    );
}

#[test]
fn empty_config_parses_to_expected_json() {
    let config = fixture("empty.toml");
    assert_eq!(
        serde_json::to_value(&config).unwrap(),
        expected_json("empty.toml")
    );
}

#[test]
fn invalid_configs_report_the_expected_errors() {
    let err = ConfigStore::new(fixture("invalid-multiple-problems.toml"))
        .expect_err("broken config must not build a store");
    assert_eq!(
        error_list(&err),
        vec![(
            "default".to_string(),
            vec![
                "step \"done\" may not be named \"done\"".to_string(),
                "step \"lint\": command step has an empty command".to_string(),
                "step \"plan\": unknown transition target \"nope\"".to_string(),
            ],
        )]
    );
}

#[test]
fn invalid_configs_report_every_workflow() {
    let err = ConfigStore::new(fixture("invalid-two-workflows.toml"))
        .expect_err("broken configs must not build a store");
    assert_eq!(
        error_list(&err),
        vec![
            (
                "a".to_string(),
                vec!["step \"plan\": unknown transition target \"ghost\"".to_string()],
            ),
            (
                "b".to_string(),
                vec!["unknown initial step \"nope\"".to_string()],
            ),
        ]
    );
}
