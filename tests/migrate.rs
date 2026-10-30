//! #27: the one-time move of `~/.config/fleet` and `~/.local/state/fleet` to their
//! herdsman names on first run, under a fake HOME.
mod common;
use common::tmpdir;
use herdsman::events::Event;
use std::path::Path;
use std::process::Command;

const CONFIG: &str = "[fleet.s]\nrepo = \"a/s\"\nlocal = \"/tmp/x\"\nintegration = \"staging\"\n";

/// `herdsman <args>` with HOME = `home` and no path overrides: (exit, stdout, stderr).
fn run(home: &Path, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_herdsman"))
        .args(args)
        .env("HOME", home)
        .env_remove("FLEET_CONFIG")
        .env_remove("FLEET_STATE")
        .output()
        .unwrap();
    (o.status.code().unwrap(), String::from_utf8(o.stdout).unwrap(), String::from_utf8(o.stderr).unwrap())
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

#[test]
fn old_dirs_move_once_and_status_keeps_history() {
    let h = tmpdir("migrate-move");
    seed(&h, "fleet");
    let (code, out, err) = run(&h, &["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("12") && out.contains("merged"), "{out}");
    assert!(err.contains("moved") && err.contains(".config/herdsman"), "{err}");
    assert!(!h.join(".config/fleet").exists() && !h.join(".local/state/fleet").exists());
    assert_eq!(std::fs::read_to_string(h.join(".config/herdsman/fleet.toml")).unwrap(), CONFIG);
    assert!(h.join(".local/state/herdsman/events.jsonl").exists());
    // Second run: nothing left to move.
    let (code, again, err) = run(&h, &["status"]);
    assert_eq!((code, again.as_str()), (0, out.as_str()));
    assert!(!err.contains("moved"), "{err}");
}

#[test]
fn existing_new_dirs_win_and_old_ones_are_left_alone() {
    let h = tmpdir("migrate-both");
    seed(&h, "fleet");
    seed(&h, "herdsman");
    std::fs::write(h.join(".config/fleet/fleet.toml"), "# old\n").unwrap();
    let (code, _, err) = run(&h, &["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("moved"), "{err}");
    assert_eq!(std::fs::read_to_string(h.join(".config/fleet/fleet.toml")).unwrap(), "# old\n");
    assert_eq!(std::fs::read_to_string(h.join(".config/herdsman/fleet.toml")).unwrap(), CONFIG);
    assert!(h.join(".local/state/fleet/events.jsonl").exists());
}

#[test]
fn neither_dir_exists_so_new_ones_are_created_lazily() {
    let h = tmpdir("migrate-none");
    run(&h, &["status"]);
    assert!(!h.join(".config/fleet").exists() && !h.join(".config/herdsman").exists());
    std::fs::create_dir_all(h.join(".config/herdsman")).unwrap();
    std::fs::write(h.join(".config/herdsman/fleet.toml"), CONFIG).unwrap();
    let (code, _, err) = run(&h, &["queue", "s", "add", "12"]);
    assert_eq!(code, 0, "{err}");
    assert!(h.join(".local/state/herdsman/events.jsonl").exists());
    assert!(!h.join(".local/state/fleet").exists());
}

#[test]
fn symlinked_old_dir_moves_as_a_link() {
    let h = tmpdir("migrate-link");
    seed(&h, "real");
    std::os::unix::fs::symlink(h.join(".config/real"), h.join(".config/fleet")).unwrap();
    std::os::unix::fs::symlink(h.join(".local/state/real"), h.join(".local/state/fleet")).unwrap();
    let (code, out, err) = run(&h, &["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("merged"), "{out}");
    assert_eq!(std::fs::read_to_string(h.join(".config/herdsman/fleet.toml")).unwrap(), CONFIG);
    assert!(h.join(".config/real/fleet.toml").exists(), "link target untouched");
}

#[test]
fn unmovable_old_dir_is_reported_and_the_run_goes_on() {
    use std::os::unix::fs::PermissionsExt;
    let h = tmpdir("migrate-ro");
    seed(&h, "fleet");
    let parent = h.join(".config");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (code, _, err) = run(&h, &["status"]);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("moving ~/.config/fleet to ~/.config/herdsman failed"), "{err}");
    assert_eq!(std::fs::read_to_string(parent.join("fleet/fleet.toml")).unwrap(), CONFIG, "old dir intact");
    assert!(!parent.join("herdsman").exists());
}
