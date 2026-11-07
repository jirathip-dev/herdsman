//! #539: a retired config key is a warning, never a load error. Each `upgrade/*.toml` is a fleet.toml from before a
//! retired-key change; this build loads it. `check` and the board warn; the install gate refuses a binary that can't.
mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const RETIRED: &str = include_str!("upgrade/441-accounts-env.toml");

fn fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/upgrade");
    let mut all: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    all.sort();
    all
}

#[test]
fn a_fleet_toml_from_before_each_retired_key_change_loads() {
    let all = fixtures();
    assert!(all.len() >= 5, "{all:?}");
    for (name, text) in all {
        let c = ctx(&tmpdir("upgrade"), &text);
        let (code, out) = run(&c, &Fake::default(), &["doctor", "--config-only"]);
        assert_eq!(code, 0, "{name}: {out}");
        assert!(out.starts_with("✓ config — ") && !out.contains('✗'), "{name}: {out}");
    }
    let c = ctx(&tmpdir("upgrade-441"), RETIRED);
    let (_, out) = run(&c, &Fake::default(), &["doctor", "--config-only"]);
    let warn = "! config — [accounts].accounts_env is retired (#441): run `herdsman accounts import \
                ~/.config/claude-accounts/tokens.env`, then drop the line";
    assert!(out.lines().any(|l| l == warn), "{out}");
}

#[test]
fn config_only_fails_on_a_config_this_build_cannot_load() {
    let c = ctx(
        &tmpdir("upgrade-bad"),
        &format!("{RETIRED}\n[limits]\neffort = \"bogus\"\n"),
    );
    let (code, out) = run(&c, &Fake::default(), &["doctor", "--config-only"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("✗ config — ") && out.contains("limits.effort"), "{out}");
}

#[test]
fn check_alerts_a_retired_key_once_and_the_board_shows_it() {
    let c = ctx(&tmpdir("retired-check"), RETIRED);
    let r = Fake::default();
    let (code, out) = run(&c, &r, &["check"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[accounts].accounts_env is retired (#441)"), "{out}");
    let log = herdsman::events::read(&c.state).unwrap();
    assert_eq!(log.iter().filter(|e| e.kind == "config-warning").count(), 1);
    let (_, out) = run(&c, &r, &["check"]);
    assert!(!out.contains("accounts_env"), "once per key: {out}");
    let (code, out) = run(&c, &r, &["board"]);
    assert_eq!(code, 0, "{out}");
    let first = out.lines().next().unwrap_or_default();
    assert_eq!(first, "! [accounts].accounts_env is retired → herdsman doctor");
    assert!(first.chars().count() <= 60);
}

/// The gate runs this build's binary on a config: bad keeps the installed one, good replaces it.
#[test]
fn install_gate_keeps_the_old_binary_when_the_new_one_cannot_load_the_config() {
    let d = tmpdir("gate");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install-gate.sh");
    let new = env!("CARGO_BIN_EXE_herdsman");
    let dest = d.join("bin/herdsman");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, "old").unwrap();
    // #631: the installed keys helper is current, and has the token.
    fake_keys(dest.parent().unwrap(), "printf tok", Some(&keys_src_hash()));
    // #556: a fake `codesign` logs how the install signs (the real one would reach for the keychain); #568: a fake
    // `security` holds the identity, and `codesign` sleeps (a keychain dialog) while HANG is set.
    let fake = d.join("fake");
    std::fs::create_dir_all(&fake).unwrap();
    let log = d.join("codesign.log");
    let sh = format!(
        "#!/bin/sh\n[ -n \"$HANG\" ] && exec sleep 30\necho \"$@\" >> '{}'\n",
        log.display()
    );
    let sec = "#!/bin/sh\necho 'SHA-256 hash: 00'\necho 'SHA-1 hash: AC01B47A'\n";
    for (name, body) in [("codesign", sh.as_str()), ("security", sec)] {
        std::fs::write(fake.join(name), body).unwrap();
        std::fs::set_permissions(fake.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:{}", fake.display(), std::env::var("PATH").unwrap());
    let gate = |config: &str, hang: &str| {
        let cfg = d.join("fleet.toml");
        std::fs::write(&cfg, config).unwrap();
        Command::new("sh")
            .arg(&script)
            .args([Path::new(new), &dest])
            .env("FLEET_CONFIG", &cfg)
            .env("FLEET_STATE", d.join("state/events.jsonl"))
            .env("PATH", &path)
            .env("HOME", &d)
            .env("HANG", hang)
            .env("HERDSMAN_SIGN_TIMEOUT", if hang.is_empty() { "60" } else { "1" })
            .output()
            .unwrap()
    };
    let o = gate("[limits]\neffort = \"bogus\"\n", "");
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("kept ") && out.contains("✗ config — "), "{out}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
    assert!(!log.exists(), "a refused build is never signed");

    // #568: a codesign stuck on a keychain dialog times out: the old binary stays, the fix is named, exit 1.
    let o = gate(RETIRED, "1");
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("kept ") && out.contains("fix: security set-key-partition-list"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");

    let o = gate(RETIRED, "");
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(new).unwrap());
    // #568: by the identity's SHA-1 hash in the login keychain (by name, the untrusted cert is "no identity").
    let signed = format!(
        "--force --sign AC01B47A --keychain {}/Library/Keychains/login.keychain-db --identifier herdsman {}.new\n",
        d.display(),
        dest.display()
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap(), signed);
    // #631: the new binary read the token through the installed helper, and the current helper is not reinstalled.
    let bin = dest.parent().unwrap();
    let calls = std::fs::read_to_string(bin.join("keys.log")).unwrap();
    assert!(calls.lines().count() >= 1, "{calls}");
    assert!(
        calls
            .lines()
            .all(|l| l == "get herdsman/all/CLAUDE_CODE_OAUTH_TOKEN_MAIN"),
        "{calls}"
    );
    assert!(std::fs::read_to_string(bin.join("herdsman-keys"))
        .unwrap()
        .contains("printf tok"));
    assert!(!bin.join("herdsman-keys.new").exists());

    // #631: a helper the Keychain would prompt (exit 3): the token check fails, the old binary stays.
    fake_keys(bin, "exit 3", None);
    std::fs::write(&dest, "old").unwrap();
    let o = gate(RETIRED, "");
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("can't read the tokens") && out.contains("prompts for Keychain access → authorise "),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
}
