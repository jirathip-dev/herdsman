//! #1: only `herdsman migrate` moves the pre-rename `~/.config/fleet` and
//! `~/.local/state/fleet`; other commands read them in place, under a fake HOME.
mod common;
use common::tmpdir;
use herdsman::events::Event;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CONFIG: &str = "[fleet.s]\nrepo = \"a/s\"\nlocal = \"/tmp/x\"\nintegration = \"staging\"\n";

/// `herdsman <args>` with HOME = HERDSMAN_HOME = `home`, no path overrides, `stdin` piped:
/// (exit, stdout, stderr).
fn run_in(home: &Path, args: &[&str], envs: &[(&str, &str)], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdsman"))
        .args(args)
        .env("HOME", home)
        .env("HERDSMAN_HOME", home)
        .env_remove("FLEET_CONFIG")
        .env_remove("FLEET_STATE")
        .env_remove("HERDSMAN_LANE")
        .env_remove("FLEET_LANE")
        .envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    (o.status.code().unwrap(), String::from_utf8(o.stdout).unwrap(), String::from_utf8(o.stderr).unwrap())
}

fn run(home: &Path, args: &[&str]) -> (i32, String, String) {
    run_in(home, args, &[], "")
}

/// The pre-rename layout: config + a log holding one merged issue.
fn seed(home: &Path, name: &str) {
    let (cfg, state) = (home.join(".config").join(name), home.join(".local/state").join(name));
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(cfg.join("fleet.toml"), CONFIG).unwrap();
    let ev = Event::new(1, "s", Some(12), "merged", serde_json::json!({"sha": "abc"}));
    std::fs::write(state.join("events.jsonl"), serde_json::to_string(&ev).unwrap() + "\n").unwrap();
}

/// Every path under `dir` with its mtime and (for files) contents.
fn snapshot(dir: &Path) -> Vec<(PathBuf, std::time::SystemTime, Vec<u8>)> {
    let md = std::fs::symlink_metadata(dir).unwrap();
    let body = if md.is_file() { std::fs::read(dir).unwrap() } else { vec![] };
    let mut v = vec![(dir.to_path_buf(), md.modified().unwrap(), body)];
    if md.is_dir() {
        let mut kids: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        kids.sort();
        for k in kids {
            v.extend(snapshot(&k));
        }
    }
    v
}

fn moved(h: &Path) -> bool {
    h.join(".config/herdsman").exists() || h.join(".local/state/herdsman").exists()
}

#[test]
fn version_and_help_touch_nothing() {
    let h = tmpdir("migrate-pure");
    seed(&h, "fleet");
    let before = snapshot(&h);
    for args in [&["--version"][..], &["--help"], &["status", "--help"]] {
        let (code, out, err) = run(&h, args);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(!out.is_empty() && err.is_empty(), "{args:?}: {out} / {err}");
    }
    assert_eq!(snapshot(&h), before, "home byte- and mtime-identical");
    assert!(!moved(&h));
}

#[test]
fn status_reads_old_paths_in_place_and_hints_once() {
    let h = tmpdir("migrate-fallback");
    seed(&h, "fleet");
    let before = snapshot(&h);
    let (code, out, err) = run(&h, &["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("12") && out.contains("merged"), "real history, not an empty board: {out}");
    let hint = |what: &str, d: &str| {
        format!("herdsman: found pre-rename {what} at {}; run 'herdsman migrate' to move it", h.join(d).display())
    };
    assert_eq!(err.matches(&hint("config", ".config/fleet")).count(), 1, "{err}");
    assert_eq!(err.matches(&hint("state", ".local/state/fleet")).count(), 1, "{err}");
    assert_eq!(err.lines().count(), 2, "{err}");
    assert_eq!(snapshot(&h), before, "nothing moved");
    assert!(!moved(&h));
}

#[test]
fn status_with_neither_home_is_an_empty_board_without_hint() {
    let h = tmpdir("migrate-none");
    let (code, out, err) = run(&h, &["status"]);
    assert_eq!((code, out.trim(), err.as_str()), (0, "no tracked issues", ""));
    assert!(!h.join(".config").exists() && !h.join(".local").exists());
}

#[test]
fn existing_new_dirs_win_and_old_ones_are_left_alone() {
    let h = tmpdir("migrate-both");
    seed(&h, "fleet");
    seed(&h, "herdsman");
    std::fs::write(h.join(".config/fleet/fleet.toml"), "# old\n").unwrap();
    let (code, _, err) = run(&h, &["status"]);
    assert_eq!((code, err.as_str()), (0, ""));
    let (code, out, _) = run(&h, &["migrate"]);
    assert_eq!(code, 0);
    assert_eq!(out.matches("already exists").count(), 2, "{out}");
    assert_eq!(std::fs::read_to_string(h.join(".config/fleet/fleet.toml")).unwrap(), "# old\n");
    assert_eq!(std::fs::read_to_string(h.join(".config/herdsman/fleet.toml")).unwrap(), CONFIG);
    assert!(h.join(".local/state/fleet/events.jsonl").exists());
}

#[test]
fn migrate_moves_both_once() {
    let h = tmpdir("migrate-move");
    seed(&h, "fleet");
    let (code, out, err) = run(&h, &["migrate"]);
    assert_eq!(code, 0, "{err}");
    for d in [".config", ".local/state"] {
        let line = format!("moved {} -> {}", h.join(d).join("fleet").display(), h.join(d).join("herdsman").display());
        assert!(out.contains(&line), "{out}");
    }
    assert!(!h.join(".config/fleet").exists() && !h.join(".local/state/fleet").exists());
    assert_eq!(std::fs::read_to_string(h.join(".config/herdsman/fleet.toml")).unwrap(), CONFIG);
    assert!(h.join(".local/state/herdsman/events.jsonl").exists());
    // Second run: a no-op.
    let before = snapshot(&h);
    let (code, again, err) = run(&h, &["migrate"]);
    assert_eq!(code, 0, "{err}");
    assert!(!again.contains("moved") && again.contains("nothing to move"), "{again}");
    assert_eq!(snapshot(&h), before);
    // And status reads the new paths, no hint.
    let (code, out, err) = run(&h, &["status"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("merged"), "{out}");
}

#[test]
fn migrate_with_nothing_to_move_says_so() {
    let h = tmpdir("migrate-empty");
    let (code, out, err) = run(&h, &["migrate"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.matches("nothing to move").count(), 2, "{out}");
    assert!(!h.join(".config").exists());
}

#[test]
fn failed_state_move_rolls_the_config_move_back() {
    use std::os::unix::fs::PermissionsExt;
    let h = tmpdir("migrate-rollback");
    seed(&h, "fleet");
    let parent = h.join(".local/state");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (code, out, err) = run(&h, &["migrate"]);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(code, 0, "{out}");
    let (old, new) = (parent.join("fleet"), parent.join("herdsman"));
    assert!(err.contains(&format!("moving {} -> {} failed", old.display(), new.display())), "{err}");
    let (cold, cnew) = (h.join(".config/fleet"), h.join(".config/herdsman"));
    assert!(err.contains(&format!("rolled back {} -> {}", cnew.display(), cold.display())), "{err}");
    assert_eq!(std::fs::read_to_string(cold.join("fleet.toml")).unwrap(), CONFIG, "config back in place");
    assert!(!cnew.exists() && !new.exists());
    assert!(old.join("events.jsonl").exists());
}

#[test]
fn symlinked_old_dir_migrates_as_a_link() {
    let h = tmpdir("migrate-link");
    seed(&h, "real");
    std::os::unix::fs::symlink(h.join(".config/real"), h.join(".config/fleet")).unwrap();
    std::os::unix::fs::symlink(h.join(".local/state/real"), h.join(".local/state/fleet")).unwrap();
    let (code, _, err) = run(&h, &["migrate"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = run(&h, &["status"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("merged"), "{out}");
    assert_eq!(std::fs::read_to_string(h.join(".config/herdsman/fleet.toml")).unwrap(), CONFIG);
    assert!(h.join(".config/real/fleet.toml").exists(), "link target untouched");
}

#[test]
fn hook_with_pre_rename_home_prints_only_its_json_decision() {
    // A denied call; spelled in pieces so this file's own lane hook lets it be written.
    let cmd = ["gh", "pr", "merge", "5"].join(" ");
    let input = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash",
        "cwd": std::env::temp_dir(), "tool_input": {"command": cmd}});
    let lane = [("HERDSMAN_LANE", "s:12:impl")];
    let decide = |h: &Path| run_in(h, &["hook", "--role", "impl"], &lane, &input.to_string());
    let clean = tmpdir("migrate-hook-clean");
    let (code, want, _) = decide(&clean);
    assert_eq!(code, 0);
    let h = tmpdir("migrate-hook");
    seed(&h, "fleet");
    let before = snapshot(&h);
    let (code, out, err) = decide(&h);
    assert_eq!((code, err.as_str()), (0, ""), "no stderr at all");
    assert_eq!(out, want, "decision unchanged");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("stdout is exactly one JSON value");
    assert!(v.to_string().contains("deny"), "{v}");
    assert_eq!(snapshot(&h), before, "hook never migrates");
}

/// #9: a set-but-empty path variable is unset, never the empty (cwd-relative) path.
const EMPTY: [(&str, &str); 4] = [("FLEET_CONFIG", ""), ("FLEET_STATE", ""), ("FLEET_WORKTREES", ""), ("FLEET_HERMES_PROFILES", "")];

#[test]
fn empty_path_vars_resolve_to_the_defaults() {
    let h = tmpdir("migrate-empty-vars");
    seed(&h, "herdsman");
    for home in ["", h.to_str().unwrap()] {
        let envs: Vec<_> = EMPTY.iter().copied().chain([("HERDSMAN_HOME", home)]).collect();
        let (code, out, err) = run_in(&h, &["status"], &envs, "");
        assert_eq!((code, err.as_str()), (0, ""), "HERDSMAN_HOME={home:?}");
        assert!(out.contains("12") && out.contains("merged"), "real history, not an empty board: {out}");
    }
    // Empty HERDSMAN_HOME falls back to $HOME, pre-rename hint included.
    let old = tmpdir("migrate-empty-vars-old");
    seed(&old, "fleet");
    let envs: Vec<_> = EMPTY.iter().copied().chain([("HERDSMAN_HOME", "")]).collect();
    let (code, out, err) = run_in(&old, &["status"], &envs, "");
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("merged") && err.contains("found pre-rename config"), "{out} / {err}");
}

#[test]
fn migrate_refuses_an_empty_herdsman_home_and_leaves_the_cwd_alone() {
    let (h, cwd) = (tmpdir("migrate-empty-home"), tmpdir("migrate-empty-cwd"));
    seed(&h, "fleet");
    seed(&cwd, "fleet");
    let (bh, bc) = (snapshot(&h), snapshot(&cwd));
    let o = Command::new(env!("CARGO_BIN_EXE_herdsman"))
        .arg("migrate")
        .current_dir(&cwd)
        .env("HOME", &h)
        .env("HERDSMAN_HOME", "")
        .envs(EMPTY)
        .env_remove("HERDSMAN_LANE")
        .env_remove("FLEET_LANE")
        .output()
        .unwrap();
    let err = String::from_utf8(o.stderr).unwrap();
    assert_ne!(o.status.code(), Some(0), "{err}");
    assert!(err.contains("HERDSMAN_HOME is set but empty"), "{err}");
    assert!(o.stdout.is_empty(), "nothing moved or reported");
    assert_eq!((snapshot(&h), snapshot(&cwd)), (bh, bc), "neither $HOME nor ./.config touched");
}
