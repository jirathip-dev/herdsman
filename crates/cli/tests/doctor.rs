mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const TOKEN: &str = concat!("sk", "-ant-oat01-FAKEtokenFAKEtokenFAKEtoken0123456789");
const PLIST: &str = "<plist><dict><key>ProgramArguments</key><array><string>/x/drover</string>\
<string>serve</string><string>--addr</string><string>127.0.0.1:7777</string></array></dict></plist>";
const SUB: &str = r#"[{"endpoint":"https://push.example/x","p256dh":"k","auth":"a","label":"phone","created":1}]"#;

fn config(d: &Path, harness: &str) -> String {
    format!(
        r#"[fleet.demo]
repo = "o/demo"
local = "{local}"
integration = "staging"

[fleet.demo.role.impl]
harness = "{harness}"
profile = "fleet-impl"

[accounts]
main = "TOKEN_MAIN"

[accounts.usage]
main = "{accounts}/main"

[serve]
allowed_hosts = ["mac.tail.ts.net"]
webhook_secret_file = "{hook}"

[[notify]]
type = "webpush"
sub = "mailto:a@b.c"
"#,
        local = d.join("local").display(),
        accounts = d.join("accounts").display(),
        hook = d.join("config/secrets/GITHUB_WEBHOOK_SECRET").display(),
    )
}

fn write(p: &Path, text: &str, mode: u32) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn setup(name: &str, harness: &str) -> (drover::Ctx, PathBuf) {
    let d = tmpdir(name);
    let c = ctx(&d, &config(&d, harness));
    std::fs::create_dir_all(d.join("local")).unwrap();
    std::fs::create_dir_all(&c.worktrees).unwrap();
    write(&d.join("accounts/main/.claude.json"), r#"{"oauthAccount": {}}"#, 0o600);
    write(&d.join("config/secrets/GITHUB_WEBHOOK_SECRET"), "hook", 0o600);
    write(
        &c.home.join("Library/LaunchAgents/dev.drover.serve.plist"),
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
    assert_eq!(run(&c, &good(), &["timer", "install", "--no-load"]).0, 0);
    (c, d)
}

fn good() -> Fake {
    Fake::default()
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1")
        .reply("vm_stat", "Swapouts: 5.\n")
        .reply("sysctl -n vm.loadavg", "{ 1.00 1.00 1.00 }")
        .reply("sysctl -n hw.ncpu", "8")
        .reply("gh auth status", "")
        .reply("gh repo view o/demo", "{}")
        .reply("git -C ", "abc\n")
        .reply("which ", "/bin/x\n")
        .reply("lsof ", "drover 1 u LISTEN\n")
        .reply("tailscale serve status", "|-- / proxy http://127.0.0.1:7777\n")
        .reply("launchctl print gui/", "state = running\n")
        .reply("systemctl --user is-active", "active\n")
        .reply("codesign -v -R", "")
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
    for check in ["host", "gh", "config", "worktrees", "gh demo", "local demo"] {
        assert!(line(&out, check).starts_with('✓'), "{out}");
    }
    for check in [
        "accounts",
        "harness",
        "serve hosts",
        "serve",
        "tailscale",
        "launchd",
        "phone",
        "webhook",
        "signature",
    ] {
        if ["launchd", "signature"].contains(&check) && !cfg!(target_os = "macos") {
            assert!(!out.contains(&format!(" {check} — ")), "{out}");
            continue;
        }
        assert!(line(&out, check).starts_with('✓'), "{out}");
    }
    for check in ["vapid.key", "push", "watchdog check", "watchdog drive", "timer"] {
        assert!(line(&out, check).starts_with('✓'), "{out}");
    }
    assert!(
        line(&out, "accounts").contains("sign in from each account's own folder") && !out.contains("TOKEN_MAIN"),
        "{out}"
    );
    assert!(!out.contains(" account main — "), "{out}");
    assert!(
        !r.calls.borrow().iter().any(|c| c.starts_with("security ")),
        "{:?}",
        r.calls.borrow()
    );
    if cfg!(target_os = "macos") {
        assert!(
            line(&out, "signature").contains(&format!("{} signed by \"drover-local\"", c.exe)),
            "{out}"
        );
        assert!(r.called(&format!(
            "codesign -v -R =certificate leaf[subject.CN] = \"drover-local\" {}",
            c.exe
        )));
    }
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

    let text = std::fs::read_to_string(&c.config).unwrap();
    std::fs::write(
        &c.config,
        text.replace(
            "integration = \"staging\"",
            "integration = \"staging\"\ndriver = \"drover\"",
        ),
    )
    .unwrap();
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("driver"), "{out}");
}

#[test]
fn a_missing_merge_label_is_red_with_its_create_command_then_green() {
    let (c, _) = setup("doc-label", "claude");
    let text = std::fs::read_to_string(&c.config).unwrap();
    let text = text.replace(
        "integration = \"staging\"",
        "integration = \"staging\"\nmerge_label = \"merge-ready\"",
    );
    std::fs::write(&c.config, text).unwrap();
    let list = "gh label list --repo o/demo --search merge-ready";
    let r = good().once(list, 0, "[]");
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(
        line(&out, "label demo"),
        "✗ label demo — merge-ready in o/demo → gh label create merge-ready --repo o/demo --force"
    );
    assert!(!r.called("gh label create"));
    let r = good().reply(list, r#"[{"name":"merge-ready"}]"#);
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(line(&out, "label demo"), "✓ label demo — merge-ready in o/demo");
}

#[test]
fn secrets_never_echo() {
    let (c, _) = setup("doc-secret", "claude");
    for args in [&["doctor"][..], &["doctor", "--json"]] {
        let (code, out) = run(&c, &good(), args);
        assert_eq!(code, 0, "{out}");
        assert!(
            !out.contains(TOKEN) && !out.contains("FAKEtoken") && !out.contains("S3CRET"),
            "{out}"
        );
    }
    let (_, out) = run(&c, &good(), &["doctor"]);
    assert!(
        line(&out, "webhook").starts_with('✓') && !out.contains("— hook"),
        "{out}"
    );
}

#[test]
fn each_check_fails_with_its_fix_and_exit_one() {
    type Break = fn(&drover::Ctx, &Path, Fake) -> Fake;
    let cases: Vec<(Break, &str, &str)> = vec![
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
            "|| git clone https://github.com/o/demo.git",
        ),
        (
            |c, _, r| {
                std::fs::remove_dir_all(&c.worktrees).unwrap();
                r
            },
            "worktrees",
            "→ mkdir -p",
        ),
        (|_, _, r| r.once("which claude", 1, ""), "harness", "→ install claude"),
        (
            |_, _, r| r.once("lsof ", 1, ""),
            "serve",
            "→ drover serve --addr 127.0.0.1:7777",
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
                std::fs::remove_file(c.home.join("Library/LaunchAgents/dev.drover.serve.plist")).unwrap();
                r
            },
            "launchd",
            "no plist runs drover serve",
        ),
        (
            |_, _, r| r.once("codesign -v -R", 3, ""),
            "signature",
            "→ sh scripts/signing-identity.sh in the drover checkout (once), then sh scripts/install-gate.sh ",
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
            "missing → drover push test",
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
            "→ drover timer install",
        ),
        (
            |_, _, r| match cfg!(target_os = "macos") {
                true => r.once("launchctl print", 0, "").once("launchctl print", 1, ""),
                false => r.once("systemctl --user is-active", 3, ""),
            },
            "timer",
            "dev.drover.tick missing → drover timer install",
        ),
    ];
    for (i, (brk, check, fix)) in cases.into_iter().enumerate() {
        if ["launchd", "signature"].contains(&check) && !cfg!(target_os = "macos") {
            continue;
        }
        let (c, d) = setup(&format!("doc-fail-{i}"), "claude");
        let r = brk(&c, &d, good());
        let (code, out) = run(&c, &r, &["doctor"]);
        assert_eq!(code, 1, "{check}: {out}");
        let l = line(&out, check);
        assert!(l.starts_with('✗') && l.contains(fix), "{check}: {l}");
        assert_eq!(out.matches('✗').count(), 1, "{check}: {out}");
    }
}

#[test]
fn warnings_do_not_fail() {
    let (c, _) = setup("doc-warn", "claude");
    for t in ["devices", "push"] {
        drover::db::store::<serde_json::Value>(&c.state, t, &[]).unwrap();
    }
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 0, "{out}");
    for check in ["phone", "push"] {
        assert!(line(&out, check).starts_with('!'), "{out}");
    }
}

#[test]
fn tailscale_missing_is_a_warning() {
    struct NoTailscale(Fake);
    impl drover::runner::Runner for NoTailscale {
        fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
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
    assert!(line(&out, "gh").starts_with('✓'), "{out}");

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

    assert!(line(&out, "hermes lane-orchestrator").starts_with('✗'), "{out}");
    for p in ["fleet-impl", "lane-orchestrator"] {
        write(&c.homes["hermes"].join(p).join("config.yaml"), "", 0o644);
    }
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(line(&out, "hermes fleet-impl").starts_with('✓'), "{out}");
    assert!(line(&out, "hermes hooks").contains("→ drover profiles sync"), "{out}");

    let hook = |role: &str| format!("'{}' hook --role {role} ✓ allowed", c.exe);
    let (code, out) = run(
        &c,
        &good()
            .reply("env DROVER_LANE=demo:-:impl hermes", &hook("impl"))
            .reply("env DROVER_LANE=demo:-:orchestrator hermes", &hook("orchestrator")),
        &["doctor"],
    );
    assert_eq!(code, 0, "{out}");
}

#[test]
fn retired_accounts_env_warns_until_the_line_is_dropped() {
    let (c, d) = setup("doc-retired", "claude");
    let env = d.join("tokens.env");
    write(&env, &format!("TOKEN_MAIN={TOKEN}\n"), 0o600);
    let text = std::fs::read_to_string(&c.config).unwrap();
    let line_ = format!("accounts_env = \"{}\"\n", env.display());
    let old = text.replace("[accounts]\n", &format!("[accounts]\n{line_}"));
    std::fs::write(&c.config, &old).unwrap();
    let warn = "! config — [accounts].accounts_env is retired (#441): drop the line (#683: accounts sign in with their own login)";

    let r = good();
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.lines().any(|l| l == warn), "{out}");
    assert!(line(&out, "accounts").starts_with('✓'), "{out}");
    assert!(!out.contains(TOKEN), "{out}");

    std::fs::write(&c.config, &text).unwrap();
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains('!'), "{out}");
}

#[test]
fn a_hook_naming_another_binary_is_named() {
    let (mut c, d) = setup("doc-stray-hook", "claude");
    let other = d.join("other/drover");
    write(&other, "", 0o755);
    let real = c.exe.clone();
    c.exe = other.display().to_string();
    assert_eq!(run(&c, &good(), &["profiles", "sync"]).0, 0);
    c.exe = real;
    let (code, out) = run(&c, &good(), &["doctor"]);
    let row = line(&out, "profile hooks");
    assert!(
        code == 1 && row.starts_with('✗') && row.contains(&*other.to_string_lossy()),
        "{out}"
    );
    let (code, out) = run(&c, &good(), &["profiles", "sync", "--check"]);
    assert!(code == 1 && out.contains(&*other.to_string_lossy()), "{out}");
    assert_eq!(run(&c, &good(), &["profiles", "sync"]).0, 0);
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert!(code == 0 && line(&out, "profile hooks").starts_with('✓'), "{out}");
}

#[test]
fn launchd_row_follows_the_configured_label() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let (c, _) = setup("doc-launchd", "claude");
    let agents = c.home.join("Library/LaunchAgents");
    for old in ["com.a.drover.plist", "local.drover.plist", "zz.drover.old.plist"] {
        write(&agents.join(old), PLIST, 0o644);
    }
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 0, "{out}");
    let l = line(&out, "launchd");
    assert!(l.starts_with('✓') && l.ends_with("/dev.drover.serve loaded"), "{l}");

    let text = std::fs::read_to_string(&c.config).unwrap();
    let mine = text.replace("[serve]\n", "[serve]\nlaunchd_label = \"dev.mine.serve\"\n");
    std::fs::write(&c.config, mine).unwrap();
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 1, "{out}");
    let l = line(&out, "launchd");
    assert!(l.starts_with('✗') && l.contains("dev.mine.serve.plist missing"), "{l}");
    assert!(l.contains("found") && l.contains("dev.drover.serve.plist"), "{l}");
    write(&agents.join("dev.mine.serve.plist"), PLIST, 0o644);
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(line(&out, "launchd").ends_with("/dev.mine.serve loaded"), "{out}");
}

#[test]
fn spawns_row_dedupes_stale_names() {
    let (c, _) = setup("doc-spawns", "claude");
    let mut names: Vec<String> = (1..=8).map(|n| format!("spawn-demo-{n}.pending")).collect();
    names.extend(std::iter::repeat_n("spawn-demo-1.pending".to_string(), 299));
    for (i, f) in names.iter().enumerate() {
        let ev = drover::events::Event::new(
            c.now - 600 + i as u64,
            "demo",
            Some(1),
            "stale-marker",
            serde_json::json!({ "file": f }),
        );
        drover::events::append(&c.state, &ev).unwrap();
    }
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 0, "{out}");
    let l = out.lines().find(|l| l.contains("spawns starting")).unwrap();
    assert!(l.starts_with("! spawns starting 0/"), "{l}");
    assert_eq!(l.matches("spawn-demo-1.pending").count(), 1, "{l}");
    assert!(l.contains("spawn-demo-1.pending ×300"), "{l}");
    assert!(l.contains("+3 more"), "{l}");
    assert!(l.len() < 300, "{} chars: {l}", l.len());
}

#[test]
fn a_paused_self_update_shows_in_doctor_and_the_board() {
    let (c, _) = setup("doc-paused", "claude");
    let r = good();
    assert_eq!(run(&c, &r, &["self-update", "--pause"]).0, 0);
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    let row = out
        .lines()
        .find(|l| l.contains(" self-update — "))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(row.starts_with("! self-update — paused since "), "{row}");
    assert!(row.ends_with(" → drover self-update --resume"), "{row}");
    let (_, board) = run(&c, &r, &["board"]);
    assert!(board.lines().any(|l| l == row), "{board}");

    assert_eq!(run(&c, &r, &["self-update", "--resume"]).0, 0);
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(!out.contains("self-update"), "{out}");
    let (_, board) = run(&c, &r, &["board"]);
    assert!(!board.contains("self-update"), "{board}");
}

#[test]
fn a_login_shell_exporting_the_token_warns() {
    let (c, _) = setup("doc-login", "claude");
    let warn = "! a login shell exports CLAUDE_CODE_OAUTH_TOKEN (it overrides each lane's account) → remove it from";
    let (_, out) = run(&c, &good(), &["doctor"]);
    assert!(!out.contains("login shell"), "{out}");
    let shell = format!("Welcome {TOKEN}\ntoken:set\nfile:.zshrc\nfile:{TOKEN}\n");
    let r = good().reply("sh -c t=$(env -i HOME=", &shell);
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    let l = out
        .lines()
        .find(|l| l.starts_with(warn))
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(l, format!("{warn} ~/.zshrc"), "{out}");
    assert!(!out.contains("FAKEtoken") && !out.contains("Welcome"), "{out}");
}

#[test]
fn a_long_tick_with_a_fresh_heartbeat_is_running_not_stuck() {
    let (c, _) = setup("doc-beat", "claude");
    let st = c.state.parent().unwrap().to_path_buf();
    write(&st.join("check.at"), &(c.now - 1080).to_string(), 0o644);
    write(&st.join("tick.beat"), &format!("{} {}", c.now - 360, c.now - 30), 0o644);
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 0, "{out}");
    let l = line(&out, "watchdog check");
    assert!(l.starts_with('✓') && l.contains("running for 6m"), "{l}");
    write(
        &st.join("tick.beat"),
        &format!("{} {}", c.now - 1080, c.now - 660),
        0o644,
    );
    let (code, out) = run(&c, &good(), &["doctor"]);
    assert_eq!(code, 1, "{out}");
    let l = line(&out, "watchdog check");
    assert!(l.starts_with('✗') && l.contains("ran 18m"), "{l}");
}

#[test]
fn an_archived_fleet_repo_is_named_with_its_fix() {
    let (c, _) = setup("doc-archived", "claude");
    let r = good().once("gh repo view", 0, r#"{"isArchived":true,"name":"demo"}"#);
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert!(line(&out, "gh demo").starts_with('✓'), "{out}");
    let fix = "! gh demo — o/demo is archived → remove [fleet.demo] from fleet.toml";
    assert!(out.contains(fix), "{out}");
}
