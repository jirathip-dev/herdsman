//! #592: a remote host keeps no fleet.toml: spawn resolves the lane's hook rules on the main host and ships them beside
//! the worktree, so the hook there judges a fleet that host never heard of as the main host would.
mod common;
use common::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const NOW: u64 = 1_000_000;
const SSH: &str = "ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/herdsman-ssh-%C -oControlPersist=10m bazzite -- ";

/// CONFIG on Claude, mini (this host) and bazzite (remote), both probed.
fn setup(name: &str) -> herdsman::Ctx {
    let base = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    let c = ctx(
        &tmpdir(name),
        &format!("{base}\n[hosts.mini]\n\n[hosts.bazzite]\nmachine = \"bazzite\"\n"),
    );
    let st = c.state.parent().unwrap();
    std::fs::create_dir_all(st).unwrap();
    let cache = json!({
        "mini": {"tags": ["os:macos"], "home": "/Users/me", "probed": NOW, "seen": NOW, "up": true},
        "bazzite": {"tags": ["os:linux"], "home": "/home/me", "probed": NOW, "seen": NOW, "up": true,
                    "load": 0.1, "mem": 12.0, "disk": 200.0},
    });
    std::fs::write(st.join("hosts.json"), cache.to_string()).unwrap();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    c
}

/// A fresh lane on bazzite: its clone, worktree, files, pane and agent start all answer.
fn remote_fake() -> Fake {
    let ws = r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#;
    Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply("herdr --machine bazzite agent list", NO_AGENTS)
        .reply("sysctl -n vm.loadavg", "{ 0.4 1 1 }")
        .reply("sysctl -n hw.ncpu", "4")
        .reply(&format!("{SSH}'cat' '/proc/loadavg'"), "0.4 1 1 1/9 42")
        .reply(&format!("{SSH}'nproc'"), "4")
        .reply(
            &format!("{SSH}'/opt/herdsman/bin/herdsman' 'profiles' 'sync' '--profile'"),
            "",
        )
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply(&format!("{SSH}'git' "), "")
        .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "")
        .reply(&format!("{SSH}'sh' '-c' 'umask 077"), "")
        .reply(&format!("{SSH}'rm' '-f'"), "")
        .reply("herdr --machine bazzite worktree create", ws)
        .reply(
            "herdr --machine bazzite pane list --workspace w9",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr --machine bazzite pane run w9:p1 export", "{}")
        .reply("herdr --machine bazzite agent start sendmeter-impl-12", "{}")
        .reply("herdr --machine bazzite agent wait sendmeter-impl-12", "{}")
}

/// The lane's environment on bazzite as the spawn left it: each `export`ed var, a value naming a file the spawn wrote
/// there pointing at that file's copy under `remote`.
fn lane_env(r: &Fake, remote: &Path) -> BTreeMap<String, String> {
    let written: BTreeMap<String, String> = r
        .inputs
        .borrow()
        .iter()
        .filter_map(|(cmd, text)| {
            let path = cmd.strip_suffix('\'')?.rsplit_once("'sh' '")?.1;
            Some((path.to_string(), text.clone()))
        })
        .collect();
    let calls = r.calls.borrow();
    let export = calls.iter().find(|c| c.contains("pane run w9:p1 export")).unwrap();
    let mut env = BTreeMap::new();
    for (i, (k, v)) in export.split_whitespace().filter_map(|w| w.split_once('=')).enumerate() {
        let v = v.trim_matches('\'');
        let v = match written.get(v) {
            Some(text) => {
                let copy = remote.join(format!("file{i}"));
                std::fs::write(&copy, text).unwrap();
                copy.display().to_string()
            }
            None => v.to_string(),
        };
        env.insert(k.to_string(), v);
    }
    env
}

/// `herdsman hook --role impl` on bazzite: the lane's env, that host's own `fleet.toml` (`None`: none at all).
fn hook_there(env: &BTreeMap<String, String>, remote: &Path, toml: Option<&str>, cmd: &str) -> String {
    let conf = remote.join("fleet.toml");
    let _ = std::fs::remove_file(&conf);
    if let Some(t) = toml {
        std::fs::write(&conf, t).unwrap();
    }
    let mut c = Command::new(env!("CARGO_BIN_EXE_herdsman"));
    c.env_remove("HERDSMAN_SCRATCH_TARGET").env_remove("FLEET_LANE");
    for k in ["HERDSMAN_LANE", "HERDSMAN_HOST", "HERDSMAN_RULES"] {
        c.env_remove(k);
    }
    for (k, v) in env
        .iter()
        .filter(|(k, _)| !["HERDSMAN_OUTBOX", "HERDSMAN_SERVICE"].contains(&k.as_str()))
    {
        c.env(k, v);
    }
    let mut child = c
        .args(["hook", "--role", "impl"])
        .current_dir(remote)
        .env("FLEET_CONFIG", &conf)
        .env("FLEET_STATE", remote.join("state/events.jsonl"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input =
        json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "cwd": "/", "tool_input": {"command": cmd}});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_remote_lanes_hook_judges_a_fleet_its_host_never_heard_of() {
    let c = setup("remote-rules");
    let r = remote_fake();
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let remote = tmpdir("remote-rules-bazzite");
    let env = lane_env(&r, &remote);
    // bazzite was bootstrapped before sendmeter was added: its own fleet.toml knows only another fleet.
    let stale = "[fleet.other]\nrepo = \"o/other\"\nlocal = \"/tmp/other\"\nintegration = \"staging\"\n";
    for toml in [Some(stale), None] {
        let allowed = hook_there(&env, &remote, toml, "git status");
        assert_eq!(allowed, "", "{toml:?}: {allowed}");
        let push = hook_there(&env, &remote, toml, "git push origin staging");
        assert!(
            push.contains("\"deny\"") && push.contains("staging") && !push.contains("HOOK_FAILED_CLOSED"),
            "{toml:?}: {push}"
        );
        let docker = hook_there(&env, &remote, toml, "docker ps");
        assert!(docker.contains("NO_CONTAINERS"), "{toml:?}: {docker}");
    }
}

/// `herdsman hook` in a remote lane whose `HERDSMAN_RULES` names `rules`, its host's fleet.toml knowing its fleet.
fn hook_with_rules(remote: &Path, rules: &Path) -> String {
    let env = BTreeMap::from([
        ("HERDSMAN_LANE".to_string(), "sendmeter:12:impl".to_string()),
        ("HERDSMAN_HOST".to_string(), "bazzite".to_string()),
        ("HERDSMAN_RULES".to_string(), rules.display().to_string()),
    ]);
    hook_there(&env, remote, Some(CONFIG), "git status")
}

#[test]
fn a_missing_or_unreadable_rules_file_fails_closed_naming_it() {
    let remote = tmpdir("remote-rules-closed");
    let rules = remote.join("issue-12.rules.json");
    for text in [None, Some("not json"), Some("{\"branches\": 3}")] {
        let _ = std::fs::remove_file(&rules);
        if let Some(t) = text {
            std::fs::write(&rules, t).unwrap();
        }
        let out = hook_with_rules(&remote, &rules);
        let why = format!(
            "REFUSED HOOK_FAILED_CLOSED: rules file {} is unreadable (",
            rules.display()
        );
        assert!(out.contains("\"deny\"") && out.contains(&why), "{text:?}: {out}");
    }
}

#[test]
fn the_rules_file_holds_no_secrets_and_the_lane_may_not_edit_it() {
    let c = setup("remote-rules-secrets");
    let extra = "\n[fleet.sendmeter.env]\nPLAIN = \"plain-env-value\"\nAPI = \"secret:API_KEY\"\n\
                 \n[[fleet.sendmeter.notify]]\ncmd = \"curl https://api.telegram.org/botSEKRIT/send\"\n";
    let text = std::fs::read_to_string(&c.config).unwrap();
    std::fs::write(&c.config, text + extra).unwrap();
    let r = remote_fake().stored("herdsman/sendmeter/API_KEY", "tok-VALUE-123");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let wt = "/home/me/.local/share/herdsman/work/sendmeter/issue-12"; // #630
    let inputs = r.inputs.borrow();
    let rules = inputs
        .iter()
        .find(|i| i.0.ends_with(&format!("'{wt}.rules.json'")))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&rules.1).unwrap();
    let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["branches", "builds", "deny", "groups", "reviewer"]);
    for secret in [
        "SEKRIT",
        "telegram",
        "plain-env-value",
        "API_KEY",
        "tok-VALUE-123",
        "secret:",
    ] {
        assert!(!rules.1.contains(secret), "{secret}: {}", rules.1);
    }
    // The lane's own rules are as protected as fleet.toml.
    let remote = tmpdir("remote-rules-edit");
    let file = tmpdir("remote-rules-wt").join("issue-12.rules.json");
    std::fs::write(&file, &rules.1).unwrap();
    let env = BTreeMap::from([
        ("HERDSMAN_LANE".to_string(), "sendmeter:12:impl".to_string()),
        ("HERDSMAN_RULES".to_string(), file.display().to_string()),
    ]);
    let edit = hook_there(&env, &remote, None, &format!("echo '{{}}' > {}", file.display()));
    assert!(
        edit.contains(&format!("writing under {} is blocked", file.display())),
        "{edit}"
    );
}

#[test]
fn a_config_change_marks_a_live_remote_lane_rules_stale_until_respawned() {
    let c = setup("remote-rules-stale");
    let r = remote_fake();
    assert_eq!(
        run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]).0,
        0
    );
    let (_, board) = run(&c, &r, &["board", "sendmeter"]);
    assert!(!board.contains("rules stale"), "{board}");
    // A config apply that leaves its rules alone marks nothing.
    let text = std::fs::read_to_string(&c.config).unwrap();
    std::fs::write(&c.config, text.replace("stall_minutes = 30", "stall_minutes = 40")).unwrap();
    let (_, board) = run(&c, &r, &["board", "sendmeter"]);
    assert!(!board.contains("rules stale"), "{board}");
    let text = std::fs::read_to_string(&c.config).unwrap();
    std::fs::write(&c.config, text + "deny_commands_add = [\"terraform apply\"]\n").unwrap();
    let (_, board) = run(&c, &r, &["board", "sendmeter"]);
    assert!(board.contains("rules stale"), "{board}");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--supersede"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let (_, board) = run(&c, &r, &["board", "sendmeter"]);
    assert!(!board.contains("rules stale"), "{board}");
}

/// #592 (q659): a host with no fleet.toml gets each lane's profile from spawn, by name.
#[test]
fn spawn_syncs_the_lanes_profile_on_a_host_with_no_fleet_toml() {
    let c = setup("remote-rules-profile");
    let r = remote_fake();
    assert_eq!(
        run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]).0,
        0
    );
    let sync = format!("{SSH}'/opt/herdsman/bin/herdsman' 'profiles' 'sync' '--profile' 'fleet-impl' '--role' 'impl'");
    let (at, start) = (r.index(&sync), r.index("herdr --machine bazzite agent start"));
    assert!(at.is_some() && at < start, "{:?}", r.calls.borrow());
    // There: that one profile is written with no fleet.toml at all.
    let there = ctx(&tmpdir("remote-rules-profile-there"), "");
    std::fs::remove_file(&there.config).unwrap();
    let (code, out) = run(
        &there,
        &Fake::default(),
        &["profiles", "sync", "--profile", "fleet-rev", "--role", "rev"],
    );
    assert_eq!(code, 0, "{out}");
    let settings = std::fs::read_to_string(there.config.with_file_name("claude/fleet-rev/settings.json")).unwrap();
    assert!(settings.contains("hook --role rev"), "{settings}");
    assert_ne!(
        run(&there, &Fake::default(), &["profiles", "sync"]).0,
        0,
        "a full sync still needs fleet.toml"
    );
}
