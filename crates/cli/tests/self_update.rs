//! #557: no Hermes. `self-update` (the `dev.herdsman.update` job) installs only a green head, through the signing
//! and doctor gates, with a fake git, gh, cargo and codesign; `clean --scheduled` (the daily job) keeps its 3-day,
//! 10-per-run and live/dirty rules and closes orphan workspaces; a leftover `[telegram]` only warns.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use serde_json::json;
use std::path::PathBuf;

const SHA: &str = "abcdef1234567890abcdef1234567890abcdef12";
const FLEET: &str = "[fleet.herdsman]\nrepo = \"jirathip-dev/herdsman\"\nlocal = \"/x/herdsman\"\nintegration = \"staging\"\n\n[fleet.herdsman.role.impl]\nharness = \"claude\"\n";

/// A ctx whose live binary (`OLD`) and a finished build (`NEW`) are real files; the live path, canonical.
fn setup(name: &str) -> (herdsman::Ctx, PathBuf) {
    let d = tmpdir(name);
    let mut c = ctx(&d, FLEET);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::write(d.join("bin/herdsman"), "OLD").unwrap();
    c.exe = d.join("bin/herdsman").to_string_lossy().into();
    let built = c.state.with_file_name("update/target/release/herdsman");
    std::fs::create_dir_all(built.parent().unwrap()).unwrap();
    std::fs::write(&built, "NEW").unwrap();
    std::fs::create_dir_all(&c.home).unwrap();
    let live = std::fs::canonicalize(&c.exe).unwrap();
    (c, live)
}

/// git answers `SHA` for staging, gh its check runs (`conclusion`), the rest of the install succeeds.
fn host(conclusion: &str) -> Fake {
    let checks = json!([{"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": conclusion}]);
    Fake::default()
        .reply("git -C /x/herdsman rev-parse origin/staging", &format!("{SHA}\n"))
        .reply("git -C", "")
        .reply(
            &format!("gh api repos/jirathip-dev/herdsman/commits/{SHA}/check-runs"),
            &checks.to_string(),
        )
        .reply("cargo build --release --locked -p herdsman", "")
        .reply("which codesign", "/usr/bin/codesign\n")
        .reply("sh ", "")
        .reply("launchctl kickstart -k gui/", "")
        .reply("systemctl --user restart dev.herdsman.serve.service", "")
}

/// #568: the build's own script signs it by hash.
fn sign_script(c: &herdsman::Ctx) -> String {
    let s = c.state.with_file_name("update/src/scripts/signing-identity.sh");
    s.to_string_lossy().into()
}

/// #631: the new build's token check, through the helper beside the live binary (`ext`: `.new` when staged).
fn tokens(live: &std::path::Path, ext: &str) -> String {
    let keys = live.with_file_name("herdsman-keys");
    format!(
        "env HERDSMAN_KEYS={}{ext} {}.new doctor --tokens",
        keys.display(),
        live.display()
    )
}

fn kinds(c: &herdsman::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state).unwrap().into_iter().filter(|e| e.kind == kind).collect()
}

#[test]
fn red_ci_installs_nothing() {
    let (c, live) = setup("upd-red");
    let r = host("FAILURE");
    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("CI failed: check `rust` is failed; not installed"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "OLD");
    assert!(!r.called("cargo") && !r.called("sh "), "no build for a red head");
}

#[test]
fn green_is_built_signed_gated_swapped_synced_and_restarted() {
    let (c, live) = setup("upd-green");
    let new = format!("{}.new", live.display());
    let r = host("SUCCESS")
        .reply(&format!("{new} doctor --config-only"), "✓ config — ok\n")
        .reply(&tokens(&live, ""), "✓ account A\n")
        .reply(&format!("{} profiles sync", live.display()), "");
    let (code, out) = run(&c, &r, &["self-update", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("would build abcdef1") && !r.called("cargo"), "{out}");

    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("✓ installed abcdef1 at "), "{out}");
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "NEW", "swapped in");
    assert!(!PathBuf::from(&new).exists());
    // Signed, then gated, then synced, then serve restarted, in that order.
    let at = |p: &str| {
        r.index(p)
            .unwrap_or_else(|| panic!("{p} not called: {:?}", r.calls.borrow()))
    };
    let order = [
        at("cargo build"),
        at(&format!("sh {} sign {new}", sign_script(&c))),
        at(&format!("{new} doctor --config-only")),
        at(&tokens(&live, "")),
        at(&format!("{} profiles sync", live.display())),
        at(if cfg!(target_os = "macos") {
            "launchctl kickstart -k gui/"
        } else {
            "systemctl --user restart"
        }),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert_eq!(kinds(&c, "self-updated")[0].str("sha"), Some(SHA));

    let r2 = host("SUCCESS");
    assert_eq!(run(&c, &r2, &["self-update"]).1, "= up to date @ abcdef1\n");
    assert!(!r2.called("cargo"));
}

#[test]
fn a_failed_doctor_keeps_the_old_binary_and_alerts_once_per_head() {
    let (c, live) = setup("upd-doctor");
    let new = format!("{}.new", live.display());
    let bad = "✗ config — limits.effort: \"bogus\" must be one of …\n";
    let r = || host("SUCCESS").once(&format!("{new} doctor --config-only"), 1, bad);
    let (code, out) = run(&c, &r(), &["self-update"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("doctor --config-only on the live config: ✗ config — limits.effort"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "OLD");
    assert!(!PathBuf::from(&new).exists(), "the staged build is dropped");
    let failed = kinds(&c, "self-update-failed");
    assert_eq!(failed.len(), 1);
    assert!(failed[0].str("reason").unwrap().contains("limits.effort"));
    // The same head fails again: no second alert.
    let r2 = r();
    assert_eq!(run(&c, &r2, &["self-update"]).0, 1);
    assert!(!r2.called("profiles sync"));
    assert_eq!(kinds(&c, "self-update-failed").len(), 1);
}

/// #631: a build whose token check fails (its helper read would prompt) is never swapped in, serve keeps running.
#[test]
fn a_build_that_cannot_read_the_tokens_keeps_the_old_binary_alerts_once_and_leaves_serve() {
    let (c, live) = setup("upd-tokens");
    let new = format!("{}.new", live.display());
    let bad = "✗ account A — herdsman/all/T_A prompts for Keychain access → authorise /k/herdsman-keys once: …\n";
    let r = || {
        host("SUCCESS")
            .reply(&format!("{new} doctor --config-only"), "✓ config — ok\n")
            .once(&tokens(&live, ""), 1, bad)
    };
    for _ in 0..2 {
        let r = r();
        let (code, out) = run(&c, &r, &["self-update"]);
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("doctor --tokens, the new build can't read the tokens: ✗ account A"),
            "{out}"
        );
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "OLD");
        assert!(!PathBuf::from(&new).exists());
        assert!(!r.called("launchctl") && !r.called("systemctl") && !r.called(&format!("{} profiles", live.display())));
    }
    let failed = kinds(&c, "self-update-failed");
    assert_eq!(failed.len(), 1, "once per head");
    assert!(failed[0].str("reason").unwrap().contains("authorise /k/herdsman-keys"));
}

/// #631: the helper is reinstalled only when its source hash changes: kept as is while it matches, else staged as
/// `.new`, gated, then swapped in with its new hash.
#[test]
fn the_keys_helper_is_reinstalled_only_when_its_source_changes() {
    let (c, live) = setup("upd-keys");
    let src = c.state.with_file_name("update/src/crates/keys");
    std::fs::create_dir_all(src.join("src")).unwrap();
    std::fs::write(src.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(src.join("src/main.rs"), "fn main() {}\n").unwrap();
    let built = c.state.with_file_name("update/target/release/herdsman-keys");
    std::fs::write(&built, "KEYS v2").unwrap();
    let hash = |main: &str| {
        use sha2::Digest;
        let h = sha2::Sha256::digest(format!("[package]\n{main}"));
        h.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let (keys, stamp) = (
        live.with_file_name("herdsman-keys"),
        live.with_file_name("herdsman-keys.src"),
    );
    std::fs::write(&keys, "KEYS v1").unwrap();
    std::fs::write(&stamp, hash("fn main() {}\n")).unwrap();
    let r = |ext: &str| {
        host("SUCCESS")
            .reply(
                &format!("{}.new doctor --config-only", live.display()),
                "✓ config — ok\n",
            )
            .reply(&tokens(&live, ext), "✓ account A\n")
            .reply(&format!("{} profiles sync", live.display()), "")
    };
    let r1 = r("");
    let (code, out) = run(&c, &r1, &["self-update"]);
    assert_eq!(code, 0, "{out}");
    assert!(r1.called("cargo build --release --locked -p herdsman -p herdsman-keys"));
    assert!(r1.called(&tokens(&live, "")), "gated through the installed helper");
    assert_eq!(
        std::fs::read_to_string(&keys).unwrap(),
        "KEYS v1",
        "same source: not reinstalled"
    );

    std::fs::remove_file(c.state.with_file_name("update/sha")).unwrap();
    std::fs::write(built.with_file_name("herdsman"), "NEW").unwrap();
    std::fs::write(src.join("src/main.rs"), "fn main() { v2() }\n").unwrap();
    let r2 = r(".new");
    let (code, out) = run(&c, &r2, &["self-update"]);
    assert_eq!(code, 0, "{out}");
    assert!(r2.called(&tokens(&live, ".new")), "gated through the new helper");
    assert_eq!(std::fs::read_to_string(&keys).unwrap(), "KEYS v2");
    assert_eq!(std::fs::read_to_string(&stamp).unwrap(), hash("fn main() { v2() }\n"));
    assert!(!live.with_file_name("herdsman-keys.new").exists());
}

#[test]
fn a_missing_signing_identity_keeps_the_old_binary_and_alerts() {
    let (c, live) = setup("upd-identity");
    let r = host("SUCCESS").once_err(&format!("sh {} sign", sign_script(&c)), 2, "");
    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("no \"herdsman-local\" identity") && out.contains("signing-identity.sh"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "OLD");
    assert!(
        !r.called(&format!("{}.new doctor", live.display())),
        "an unsigned build is never gated in"
    );
    assert!(kinds(&c, "self-update-failed")[0]
        .str("reason")
        .unwrap()
        .contains("no \"herdsman-local\" identity"));
}

#[test]
fn pause_and_resume_are_logged_and_pause_stops_the_job() {
    let (c, live) = setup("upd-pause");
    assert_eq!(
        run(&c, &Fake::default(), &["self-update", "--pause"]).1,
        "✓ self-update paused\n"
    );
    let r = host("SUCCESS");
    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "= self-update paused → herdsman self-update --resume\n")
    );
    assert!(r.calls.borrow().is_empty());
    assert_eq!(kinds(&c, "self-update-paused")[0].str("by"), Some("owner"));
    run(&c, &Fake::default(), &["self-update", "--resume"]);
    assert_eq!(kinds(&c, "self-update-resumed").len(), 1);
    let r = host("FAILURE");
    assert!(run(&c, &r, &["self-update"]).1.contains("not installed"));
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "OLD");
}

#[test]
fn scheduled_clean_keeps_its_age_cap_and_live_dirty_rules_and_closes_orphan_workspaces() {
    let d = tmpdir("clean-sched");
    let c = ctx(&d, CONFIG);
    let day = 86_400;
    let lane = |issue: u64, merged_at: u64| {
        let w = c.worktrees.join("sendmeter").join(format!("issue-{issue}"));
        std::fs::create_dir_all(&w).unwrap();
        let w = w.to_string_lossy().into_owned();
        let d = json!({"role": "impl", "agent": format!("impl-{issue}"), "worktree": w, "pane": format!("w{issue}:p1"), "workspace": format!("w{issue}")});
        append(&c.state, &Event::new(10, "sendmeter", Some(issue), "spawn", d)).unwrap();
        let m = json!({"pr": "1", "merge_sha": "m", "into": "staging"});
        append(&c.state, &Event::new(merged_at, "sendmeter", Some(issue), "merged", m)).unwrap();
        w
    };
    let old = c.now - 4 * day;
    let removable: Vec<String> = (1..=12).map(|n| lane(n, old)).collect();
    let fresh = lane(20, c.now - day);
    let live = lane(21, old);
    let dirty = lane(22, old);
    let gone = d.join("no-such-checkout").to_string_lossy().into_owned();
    let ws = json!({"result": {"workspaces": [
        {"workspace_id": "wX", "worktree": {"checkout_path": gone}},
        {"workspace_id": "wY", "worktree": {"checkout_path": d.to_string_lossy()}},
    ]}});
    // Every worktree pushed and clean (no status lines, 0 unpushed) but `dirty`, whose wip save fails.
    let all: Vec<&String> = removable.iter().chain([&fresh, &live, &dirty]).collect();
    let git = |f: Fake| {
        let f = all
            .iter()
            .fold(f, |f, w| f.reply(&format!("git -C {w} rev-list"), "0\n"));
        f.reply("git -C", "").reply("herdr", "{}")
    };
    let r = git(Fake::default()
        .reply("herdr agent list", &agents(&[("impl-21", "idle", &live, "w21:p1")]))
        .reply("herdr workspace list", &ws.to_string())
        .once(&format!("git -C {dirty} status"), 0, "?? notes.md\n")
        .once_err(&format!("git -C {dirty} add -A"), 1, "add failed"));
    let (code, out) = run(&c, &r, &["clean", "--scheduled"]);
    assert_eq!(code, 0, "{out}");
    let cleaned: Vec<String> = kinds(&c, "clean")
        .iter()
        .map(|e| e.str("worktree").unwrap().into())
        .collect();
    assert_eq!(cleaned.len(), 10, "10 a run: {out}");
    assert!(cleaned.iter().all(|w| removable.contains(w)), "{cleaned:?}");
    assert!(out.contains("stop: 10 removed this run"), "{out}");
    assert!(
        !out.contains(&fresh) && !cleaned.contains(&fresh),
        "merged a day ago stays: {out}"
    );
    assert!(out.contains(&format!("keep {live}: live agent")), "{out}");
    assert!(out.contains(&format!("keep {dirty}: wip save failed")), "{out}");
    assert!(
        r.called("herdr workspace close wX") && !r.called("herdr workspace close wY"),
        "{out}"
    );
    assert!(kinds(&c, "clean-failed").is_empty(), "a passing run alerts nothing");
    cleaned.iter().for_each(|w| std::fs::remove_dir_all(w).unwrap()); // what the faked `herdr worktree remove` does

    // The next run takes the rest (the live one has stopped, the dirty one is clean now); a failure alerts once, not
    // again while it repeats.
    let r = || {
        git(Fake::default()
            .reply("herdr agent list", NO_AGENTS)
            .reply("herdr workspace list", &ws.to_string())
            .once_err("herdr workspace close wX", 1, "server gone"))
    };
    let (code, out) = run(&c, &r(), &["clean", "--scheduled"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(kinds(&c, "clean").len(), 14, "{out}");
    assert!(!kinds(&c, "clean").iter().any(|e| e.str("worktree") == Some(&fresh)));
    assert_eq!(run(&c, &r(), &["clean", "--scheduled"]).0, 1);
    let failed = kinds(&c, "clean-failed");
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0].str("reason").unwrap().contains("close workspace wX"),
        "{failed:?}"
    );
}

#[test]
fn setup_and_timer_write_the_update_and_daily_clean_jobs() {
    let c = ctx(&tmpdir("upd-jobs"), FLEET);
    let r = Fake::default().reply("which", "/opt/bin/x\n");
    let (code, out) = run(&c, &r, &["timer", "install", "--no-load"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("✓ wrote dev.herdsman.update") && out.contains("✓ wrote dev.herdsman.clean"),
        "{out}"
    );
    let (update, clean) = if cfg!(target_os = "macos") {
        let p = |l: &str| std::fs::read_to_string(c.home.join(format!("Library/LaunchAgents/{l}.plist"))).unwrap();
        (p("dev.herdsman.update"), p("dev.herdsman.clean"))
    } else {
        let p = |l: &str| std::fs::read_to_string(c.home.join(format!(".config/systemd/user/{l}.timer"))).unwrap();
        (p("dev.herdsman.update"), p("dev.herdsman.clean"))
    };
    assert!(update.contains("300") && !update.contains("03:30"), "{update}");
    let daily = [
        "<key>Hour</key><integer>3</integer><key>Minute</key><integer>30</integer>",
        "OnCalendar=*-*-* 03:30:00",
    ];
    assert!(daily.iter().any(|d| clean.contains(d)), "{clean}");
    let (_, out) = run(&c, &r, &["timer", "remove"]);
    assert!(
        out.contains("✓ removed dev.herdsman.update") && out.contains("✓ removed dev.herdsman.clean"),
        "{out}"
    );
}

#[test]
fn a_config_with_telegram_keys_loads_with_a_warning() {
    let text = format!("{FLEET}\n[telegram]\nchat = -100123\nowner = 4242\n");
    let c = ctx(&tmpdir("upd-telegram"), &text);
    let (code, out) = run(&c, &Fake::default(), &["doctor", "--config-only"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("✓ config — "), "{out}");
    let warn = "! config — [telegram] is removed (#557): herdsman has no Telegram bridge now; drop the table";
    assert!(out.lines().any(|l| l == warn), "{out}");
}
