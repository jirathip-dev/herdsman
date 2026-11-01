//! #9: set-but-empty path variables resolve like unset ones. One test: it mutates the
//! process env, which only this test binary sees.
use herdsman::Ctx;
use std::path::Path;

#[test]
fn empty_values_are_unset() {
    let h = std::env::temp_dir().join(format!("fleet-test-empty-env-{}", std::process::id()));
    std::env::set_var("HOME", &h);
    for k in [
        "FLEET_CONFIG",
        "FLEET_STATE",
        "FLEET_WORKTREES",
        "FLEET_HERMES_PROFILES",
        "HERDSMAN_HOME",
    ] {
        std::env::set_var(k, "");
    }
    let c = Ctx::from_env();
    assert_eq!(c.config, h.join(".config/herdsman/fleet.toml"));
    assert_eq!(c.state, h.join(".local/state/herdsman/events.jsonl"));
    assert_eq!(c.worktrees, h.join(".herdr/worktrees"));
    assert_eq!(c.hermes, h.join(".hermes/profiles"));
    assert_eq!(c.home, h);
    assert!(c.hints.is_empty() && !Path::new(&h).exists(), "{:?}", c.hints);
}
