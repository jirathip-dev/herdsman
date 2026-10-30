//! #21 AC3: `herdsman profiles sync` writes the Hermes `hooks:` key and nothing else.
mod common;
use common::{ctx, run, tmpdir, Fake, CONFIG};

const PROFILE: &str = "# fleet reviewer profile\n\
model:\n  default: opus\n  provider: anthropic\n\
hooks:\n  post_tool_call:\n    - command: /old/hook.sh\n\n\
# keep me\n\
terminal:\n  backend: local   # trailing comment\n\
agent:\n  max_turns: 90";

/// The lines outside the top-level `hooks:` block.
fn without_hooks(text: &str) -> Vec<&str> {
    let mut skip = false;
    text.lines()
        .filter(|l| {
            if !l.trim().is_empty() && !l.starts_with(' ') {
                skip = l.starts_with("hooks:");
            }
            !skip && !l.trim().is_empty()
        })
        .collect()
}

#[test]
fn sync_writes_only_the_hooks_key_idempotently() {
    let d = tmpdir("hermes-hooks");
    let c = ctx(&d, CONFIG);
    let rev = c.hermes.join("fleet-rev/config.yaml");
    let impl_ = c.hermes.join("fleet-impl/config.yaml");
    std::fs::create_dir_all(rev.parent().unwrap()).unwrap();
    std::fs::create_dir_all(impl_.parent().unwrap()).unwrap();
    std::fs::write(&rev, PROFILE).unwrap();
    std::fs::write(&impl_, "model:\n  default: x\n").unwrap();

    let (code, out) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0, "{out}");
    let once = std::fs::read_to_string(&rev).unwrap();
    assert_eq!(without_hooks(&once), without_hooks(PROFILE), "other settings unchanged:\n{once}");
    assert!(once.contains(
        "hooks:\n  pre_tool_call:\n    - matcher: \"terminal|patch|write_file\"\n      command: \"'/opt/herdsman/bin/herdsman' hook --role rev\"\n# keep me\n"
    ), "{once}");
    assert!(!once.contains("/old/hook.sh"));
    assert!(std::fs::read_to_string(&impl_).unwrap().ends_with(
        "model:\n  default: x\nhooks:\n  pre_tool_call:\n    - matcher: \"terminal|patch|write_file\"\n      command: \"'/opt/herdsman/bin/herdsman' hook --role impl\"\n"
    ));
    // A role without a Hermes profile on disk gets none created.
    assert!(!c.hermes.join("fleet-orch").exists());

    let (code, _) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&rev).unwrap(), once, "second sync is a no-op");
}

/// #24 AC1/AC3: sync records Hermes' consent for the herdsman hook: exactly one approval
/// per fleet role, other approvals and non-herdsman profiles untouched, second sync a no-op.
#[test]
fn sync_approves_the_hook_once_per_role() {
    let d = tmpdir("hermes-allow");
    let c = ctx(&d, CONFIG);
    for p in ["fleet-rev", "fleet-impl", "synergy"] {
        std::fs::create_dir_all(c.hermes.join(p)).unwrap();
        std::fs::write(c.hermes.join(p).join("config.yaml"), "model:\n  default: x\n").unwrap();
    }
    let other = r#"{"approvals":[{"event":"post_tool_call","command":"/x.sh"}],"note":"keep"}"#;
    std::fs::write(c.hermes.join("fleet-rev/shell-hooks-allowlist.json"), other).unwrap();
    std::fs::write(c.hermes.join("synergy/shell-hooks-allowlist.json"), other).unwrap();

    let (code, out) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0, "{out}");
    let allow = |p: &str| std::fs::read_to_string(c.hermes.join(p).join("shell-hooks-allowlist.json")).unwrap();
    for (p, role) in [("fleet-rev", "rev"), ("fleet-impl", "impl")] {
        let v: serde_json::Value = serde_json::from_str(&allow(p)).unwrap();
        let cmd = format!("'/opt/herdsman/bin/herdsman' hook --role {role}");
        let fleet: Vec<_> = v["approvals"].as_array().unwrap().iter().filter(|e| e["event"] == "pre_tool_call").collect();
        assert_eq!(fleet, [&serde_json::json!({"event": "pre_tool_call", "command": cmd})], "{p}");
    }
    let rev: serde_json::Value = serde_json::from_str(&allow("fleet-rev")).unwrap();
    assert_eq!((rev["approvals"][0]["command"].as_str(), rev["note"].as_str()), (Some("/x.sh"), Some("keep")));
    assert_eq!(allow("synergy"), other, "non-fleet profile untouched");
    assert!(!c.hermes.join("fleet-orch").exists());

    let once = (allow("fleet-rev"), allow("fleet-impl"));
    let (code, _) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0);
    assert_eq!((allow("fleet-rev"), allow("fleet-impl")), once, "second sync changes nothing");
}

/// #24 AC2: `--check` reads `hermes hooks list` as a fleet lane runs it (HERDSMAN_LANE set)
/// and fails unless every fleet profile's hook is `✓ allowed`.
#[test]
fn sync_check_requires_allowed_hooks() {
    let d = tmpdir("hermes-check");
    let c = ctx(&d, CONFIG);
    for p in ["fleet-rev", "fleet-impl"] {
        std::fs::create_dir_all(c.hermes.join(p)).unwrap();
        std::fs::write(c.hermes.join(p).join("config.yaml"), "model:\n  default: x\n").unwrap();
    }
    let line = |role: &str, status: &str| {
        format!("  [pre_tool_call]\n    - '/opt/herdsman/bin/herdsman' hook --role {role} (timeout=60s, {status})\n")
    };
    let rev = "env HERDSMAN_LANE=sendmeter:-:rev hermes -p fleet-rev hooks list";
    let impl_ = "env HERDSMAN_LANE=sendmeter:-:impl hermes -p fleet-impl hooks list";
    let r = Fake::default().reply(rev, &line("rev", "✓ allowed")).reply(impl_, &line("impl", "✗ not allowlisted"));
    let (code, out) = run(&c, &r, &["profiles", "sync", "--check"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("✗ not allowed fleet-impl") && out.contains("✓ allowed fleet-rev"), "{out}");
    assert!(!c.hermes.join("fleet-rev/shell-hooks-allowlist.json").exists(), "--check writes nothing");

    let r = Fake::default().reply(rev, &line("rev", "✓ allowed")).reply(impl_, &line("impl", "✓ allowed"));
    let (code, out) = run(&c, &r, &["profiles", "sync", "--check"]);
    assert_eq!(code, 0, "{out}");
}

/// #2 AC2: sync drops `agent.reasoning_effort` (fleet.toml owns effort now); nested
/// per-auxiliary values and every other line are unchanged.
#[test]
fn sync_removes_agent_reasoning_effort_only() {
    let d = tmpdir("hermes-effort");
    let c = ctx(&d, CONFIG);
    let rev = c.hermes.join("fleet-rev/config.yaml");
    std::fs::create_dir_all(rev.parent().unwrap()).unwrap();
    let before = "model:\n  default: opus\n\
agent:\n  max_turns: 90\n  reasoning_effort: high   # owned by fleet now\n  auxiliary:\n    vision:\n      reasoning_effort: low\n    compression:\n      reasoning_effort: minimal\n\
auxiliary:\n  web_extract:\n    reasoning_effort: medium\n";
    std::fs::write(&rev, before).unwrap();
    let (code, out) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0, "{out}");
    let after = std::fs::read_to_string(&rev).unwrap();
    let want = before.replace("  reasoning_effort: high   # owned by fleet now\n", "");
    assert_eq!(without_hooks(&after), without_hooks(&want), "{after}");
    assert!(!after.contains("reasoning_effort: high"));
    for v in ["      reasoning_effort: low\n", "      reasoning_effort: minimal\n", "    reasoning_effort: medium\n"] {
        assert!(after.contains(v), "auxiliary {v:?} kept:\n{after}");
    }
}
