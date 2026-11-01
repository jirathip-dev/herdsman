//! #97: `herdsman doctor` is read-only, prints ✓/✗/! with a fix per line, exits 1 on any ✗, never echoes a secret.
mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const TOKEN: &str = "sk-ant-oat01-FAKEtokenFAKEtokenFAKEtoken0123456789";
const HERDR: &str = "client:\n  version: 0.9.5\n\nserver:\n  status: running\n  version: 0.9.5\n";
const PLIST: &str = "<plist><dict><key>ProgramArguments</key><array><string>/x/herdsman</string>\
<string>serve</string><string>--addr</string><string>127.0.0.1:7777</string></array></dict></plist>";
const SUB: &str = r#"[{"endpoint":"https://push.example/x","p256dh":"k","auth":"a","label":"phone","created":1}]"#;

fn config(d: &Path, harness: &str) -> String {
    format!(
        r#"[fleet.demo]
repo = "o/demo"
local = "{local}"
integration = "staging"
driver = "herdsman"

[fleet.demo.role.impl]
harness = "{harness}"
profile = "fleet-impl"

[accounts]
accounts_env = "{env}"
main = "TOKEN_MAIN"

[serve]
allowed_hosts = ["mac.tail.ts.net"]

[[notify]]
type = "webpush"
sub = "mailto:a@b.c"
"#,
        local = d.join("local").display(),
        env = d.join("accounts.env").display(),
    )
}

fn write(p: &Path, text: &str, mode: u32) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A host where every check passes.
fn setup(name: &str, harness: &str) -> (herdsman::Ctx, PathBuf) {
    let d = tmpdir(name);
    let c = ctx(&d, &config(&d, harness));
    std::fs::create_dir_all(d.join("local")).unwrap();
    std::fs::create_dir_all(&c.worktrees).unwrap();
    write(&d.join("accounts.env"), &format!("export TOKEN_MAIN={TOKEN}\n"), 0o600);
    write(
        &c.home.join("Library/LaunchAgents/dev.herdsman.serve.plist"),
        PLIST,
        0o644,
    );
    let st = c.state.parent().unwrap().to_path_buf();
    write(
        &st.join("serve/devices.json"),
        r#"[{"hash":"h","label":"phone"}]"#,
        0o600,
    );
    write(
        &st.join("webpush/vapid.key"),
        "S3CRETvapidKEYvalue0123456789abcdefABCDEF",
        0o600,
    );
    write(&st.join("webpush/subs.json"), SUB, 0o600);
    for w in ["check", "drive"] {
        write(&st.join(format!("{w}.at")), &(c.now - 60).to_string(), 0o644);
    }
    (c, d)
}

fn good() -> Fake {
    Fake::default()
        .reply("herdr status", HERDR)
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1")
        .reply("vm_stat", "Swapouts: 5.\n")
        .reply("sysctl -n vm.loadavg", "{ 1.00 1.00 1.00 }")
        .reply("sysctl -n hw.ncpu", "8")
        .reply("gh auth status", "")
        .reply("gh repo view o/demo", "{}")
        .reply("git -C ", "abc\n")
        .reply("which ", "/bin/x\n")
        .reply("lsof ", "herdsman 1 u LISTEN\n")
        .reply("tailscale serve status", "|-- / proxy http://127.0.0.1:7777\n")
        .reply("launchctl print gui/", "state = running\n")
}

fn line<'a>(out: &'a str, check: &str) -> &'a str {
    let pre = format!(" {check} — ");
    out.lines()
        .find(|l| l[l.find(' ').unwrap()..].starts_with(&pre))
        .unwrap_or_else(|| panic!("{check}: {out}"))
}

#[test]
fn all_good_exits_zero_and_never_writes() {
    let (c, _) = setup("doc-good", "claude");
    let r = good();
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains('✗') && !out.contains('!'), "{out}");
    for check in [
        "herdr",
        "host",
        "gh",
        "config",
        "worktrees",
        "gh demo",
        "local demo",
        "accounts_env",
    ] {
        assert!(line(&out, check).starts_with('✓'), "{out}");
    }
    for check in [
        "account main",
        "harness",
        "serve hosts",
        "serve",
        "tailscale",
        "launchd",
        "phone",
    ] {
        // The launchd job is macOS only; Linux has no row for it.
        if check == "launchd" && !cfg!(target_os = "macos") {
            assert!(!out.contains(" launchd — "), "{out}");
            continue;
        }
        assert!(line(&out, check).starts_with('✓'), "{out}");
    }
    for check in ["vapid.key", "push", "watchdog check", "watchdog drive"] {
        assert!(line(&out, check).starts_with('✓'), "{out}");
    }
    assert!(line(&out, "account main").contains("TOKEN_MAIN set"), "{out}");
    let writes = [
        "gh pr",
        "gh issue",
        "gh api",
        "git push",
        "launchctl bootstrap",
        "tailscale serve --bg",
    ];
    assert!(
        r.calls
            .borrow()
            .iter()
            .all(|c| !writes.iter().any(|w| c.starts_with(w))),
        "{:?}",
        r.calls.borrow()
    );

    let (code, json) = run(&c, &r, &["doctor", "--json"]);
    assert_eq!(code, 0, "{json}");
    let rows: Vec<String> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.join("\n") + "\n", out);
}

#[test]
fn secrets_never_echo() {
    let (c, d) = setup("doc-secret", "claude");
    // Wrong mode and a fake token: the line names the file and mode, never its content.
    std::fs::set_permissions(d.join("accounts.env"), std::fs::Permissions::from_mode(0o644)).unwrap();
    for args in [&["doctor"][..], &["doctor", "--json"]] {
        let (code, out) = run(&c, &good(), args);
        assert_eq!(code, 1, "{out}");
        assert!(
            !out.contains(TOKEN) && !out.contains("FAKEtoken") && !out.contains("S3CRET"),
            "{out}"
        );
    }
    let (_, out) = run(&c, &good(), &["doctor"]);
    let l = line(&out, "accounts_env");
    assert!(
        l.starts_with("✗")
            && l.contains("mode 644")
            && l.ends_with(&format!("&& chmod 600 {}", d.join("accounts.env").display())),
        "{l}"
    );
}

/// Each failure: (what breaks, the check, its fix).
#[test]
fn each_check_fails_with_its_fix_and_exit_one() {
    type Break = fn(&herdsman::Ctx, &Path, Fake) -> Fake;
    let cases: Vec<(Break, &str, &str)> = vec![
        (|_, _, r| r.once("herdr status", 1, ""), "herdr", "→ herdr"),
        (|_, _, r| r.once("vm_stat", 1, ""), "host", "→ put /usr/sbin"),
        (|_, _, r| r.once("gh auth status", 1, ""), "gh", "→ gh auth login"),
        (|_, _, r| r.once("gh repo view", 1, ""), "gh demo", "→ gh auth refresh"),
        (|_, _, r| r.once("git -C ", 1, ""), "local demo", "fetch origin staging"),
        (
            |_, d, r| {
                std::fs::remove_dir_all(d.join("local")).unwrap();
                r.once("git -C ", 128, "")
            },
            "local demo",
            "|| gh repo clone o/demo",
        ),
        (
            |c, _, r| {
                std::fs::remove_dir_all(&c.worktrees).unwrap();
                r
            },
            "worktrees",
            "→ mkdir -p",
        ),
        (
            |_, d, r| {
                std::fs::remove_file(d.join("accounts.env")).unwrap();
                r
            },
            "accounts_env",
            "missing",
        ),
        (
            |_, d, r| {
                write(&d.join("accounts.env"), "OTHER=x\n", 0o600);
                r
            },
            "account main",
            "→ add TOKEN_MAIN=…",
        ),
        (|_, _, r| r.once("which claude", 1, ""), "harness", "→ install claude"),
        (
            |_, _, r| r.once("lsof ", 1, ""),
            "serve",
            "→ herdsman serve --addr 127.0.0.1:7777",
        ),
        (
            |_, _, r| r.once("tailscale serve status", 0, "No serve config\n"),
            "tailscale",
            "→ tailscale serve --bg",
        ),
        (
            |_, _, r| r.once("launchctl print", 113, ""),
            "launchd",
            "→ launchctl bootstrap gui/$(id -u)",
        ),
        (
            |c, _, r| {
                std::fs::remove_dir_all(c.home.join("Library")).unwrap();
                r
            },
            "launchd",
            "no plist runs herdsman serve",
        ),
        (
            |c, _, r| {
                let k = c.state.with_file_name("webpush/vapid.key");
                std::fs::set_permissions(k, std::fs::Permissions::from_mode(0o644)).unwrap();
                r
            },
            "vapid.key",
            "&& chmod 600",
        ),
        (
            |c, _, r| {
                std::fs::remove_file(c.state.with_file_name("webpush/vapid.key")).unwrap();
                r
            },
            "vapid.key",
            "missing → herdsman push test",
        ),
        (
            |c, _, r| {
                std::fs::remove_file(c.state.with_file_name("check.at")).unwrap();
                r
            },
            "watchdog check",
            "ran never",
        ),
        (
            |c, _, r| {
                write(&c.state.with_file_name("drive.at"), &(c.now - 3600).to_string(), 0o644);
                r
            },
            "watchdog drive",
            "| crontab -",
        ),
    ];
    for (i, (brk, check, fix)) in cases.into_iter().enumerate() {
        if check == "launchd" && !cfg!(target_os = "macos") {
            continue;
        }
        let (c, d) = setup(&format!("doc-fail-{i}"), "claude");
        let r = brk(&c, &d, good());
        let (code, out) = run(&c, &r, &["doctor"]);
        assert_eq!(code, 1, "{check}: {out}");
        let l = line(&out, check);
        assert!(l.starts_with('✗') && l.contains(fix), "{check}: {l}");
        // A missing accounts file also leaves its variables unset.
        let n = if check == "accounts_env" { 2 } else { 1 };
        assert_eq!(out.matches('✗').count(), n, "{check}: {out}");
    }
}

#[test]
fn warnings_do_not_fail() {
    let (c, _) = setup("doc-warn", "claude");
    let st = c.state.parent().unwrap().to_path_buf();
    for f in ["serve/devices.json", "webpush/subs.json"] {
        std::fs::remove_file(st.join(f)).unwrap();
    }
    let old = HERDR.replacen("version: 0.9.5\n\n", "version: 0.9.6\n\n", 1);
    let r = good().once("herdr status", 0, &old);
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        line(&out, "herdr").starts_with("! herdr — client 0.9.6, server 0.9.5 → herdr update --handoff"),
        "{out}"
    );
    for check in ["phone", "push"] {
        assert!(line(&out, check).starts_with('!'), "{out}");
    }
}

#[test]
fn tailscale_missing_is_a_warning() {
    struct NoTailscale(Fake);
    impl herdsman::runner::Runner for NoTailscale {
        fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<herdsman::runner::Out> {
            anyhow::ensure!(prog != "tailscale", "cannot run tailscale");
            self.0.run(prog, args)
        }
        fn sleep(&self, _: u64) {}
    }
    let (c, _) = setup("doc-nots", "claude");
    let (code, out) = run(&c, &NoTailscale(good()), &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        line(&out, "tailscale").starts_with("! tailscale — serve maps to 127.0.0.1:7777 →"),
        "{out}"
    );
}

#[test]
fn bad_config_and_hermes_profiles() {
    let (c, _) = setup("doc-badcfg", "claude");
    std::fs::write(&c.config, "[fleet.x]\nrepo = 1\n").unwrap();
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(line(&out, "config").starts_with('✗'), "{out}");
    assert!(line(&out, "herdr").starts_with('✓'), "{out}");

    let (c, _) = setup("doc-hermes", "hermes");
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        line(&out, "hermes fleet-impl").contains("→ hermes profile create fleet-impl"),
        "{out}"
    );
    assert!(
        line(&out, "hermes hooks").starts_with('✓'),
        "no profile, nothing to check: {out}"
    );

    write(&c.hermes.join("fleet-impl/config.yaml"), "", 0o644);
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(line(&out, "hermes fleet-impl").starts_with('✓'), "{out}");
    assert!(line(&out, "hermes hooks").contains("→ herdsman profiles sync"), "{out}");

    let hook = format!("'{}' hook --role impl ✓ allowed", c.exe);
    let (code, out) = run(
        &c,
        &good().reply("env HERDSMAN_LANE=demo:-:impl hermes", &hook),
        &["doctor"],
    );
    assert_eq!(code, 0, "{out}");
}
