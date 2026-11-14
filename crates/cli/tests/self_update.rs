mod common;
use common::*;
use drover::events::{append, read, Event};
use serde_json::json;
use std::path::PathBuf;

const SHA: &str = "abcdef1234567890abcdef1234567890abcdef12";
const FLEET: &str = "[fleet.drover]\nrepo = \"jirathip-dev/herdsman\"\nlocal = \"/x/drover\"\nintegration = \"staging\"\n\n[fleet.drover.role.impl]\nharness = \"claude\"\n";

fn setup(name: &str) -> (drover::Ctx, PathBuf) {
    setup_with(name, FLEET)
}

fn setup_with(name: &str, config: &str) -> (drover::Ctx, PathBuf) {
    let d = tmpdir(name);
    let mut c = ctx(&d, config);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::write(d.join("bin/drover"), "OLD").unwrap();
    c.exe = d.join("bin/drover").to_string_lossy().into();
    let built = c.state.with_file_name("update/target/release/drover");
    std::fs::create_dir_all(built.parent().unwrap()).unwrap();
    std::fs::write(&built, "NEW").unwrap();
    std::fs::create_dir_all(&c.home).unwrap();
    let live = std::fs::canonicalize(&c.exe).unwrap();
    (c, live)
}

fn host(conclusion: &str) -> Fake {
    let checks = json!([{"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": conclusion}]);
    Fake::default()
        .reply("git -C /x/drover rev-parse origin/main", &format!("{SHA}\n"))
        .reply("git -C", "")
        .reply(
            &format!("gh api repos/jirathip-dev/herdsman/commits/{SHA}/check-runs"),
            &checks.to_string(),
        )
        .reply("cargo build --release --locked -p drover", "")
        .reply("which codesign", "/usr/bin/codesign\n")
        .reply("sh ", "")
        .reply("launchctl kickstart -k gui/", "")
        .reply("systemctl --user restart dev.drover.serve.service", "")
}

fn sign_script(c: &drover::Ctx) -> String {
    let s = c.state.with_file_name("update/src/scripts/signing-identity.sh");
    s.to_string_lossy().into()
}

fn kinds(c: &drover::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn heads(c: &drover::Ctx, live: &std::path::Path, heads: &[(&str, &str)]) -> Fake {
    let checks = json!([{"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": "SUCCESS"}]);
    let f = heads.iter().fold(Fake::default(), |f, (b, sha)| {
        f.reply(&format!("git -C /x/drover rev-parse origin/{b}"), &format!("{sha}\n"))
            .reply(
                &format!("gh api repos/jirathip-dev/herdsman/commits/{sha}/check-runs"),
                &checks.to_string(),
            )
    });
    let staging = c.state.with_file_name("update-staging/bin/drover.new");
    f.reply("git -C", "")
        .reply("cargo build --release --locked -p drover", "")
        .reply("which codesign", "/usr/bin/codesign\n")
        .reply("sh ", "")
        .reply(&format!("{}.new doctor --config-only", live.display()), "")
        .reply(&format!("{} doctor --config-only", staging.display()), "")
        .reply(&format!("{} profiles sync", live.display()), "")
        .reply("launchctl", "")
        .reply("systemctl", "")
}

const MAIN: &str = "1111111111111111111111111111111111111111";
const MAIN2: &str = "2222222222222222222222222222222222222222";
const STAGING: &str = "3333333333333333333333333333333333333333";

#[test]
fn branch_main_ignores_a_newer_green_staging_head_and_installs_a_new_green_main_head() {
    let (c, live) = setup_with("upd-main", &format!("{FLEET}\n[self_update]\nbranch = \"main\"\n"));
    let installed = || std::fs::read_to_string(c.state.with_file_name("update/sha")).unwrap();
    let r = heads(&c, &live, &[("staging", STAGING), ("main", MAIN)]);
    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("✓ installed 1111111 at "), "{out}");
    assert!(
        r.called("git -C /x/drover fetch -q origin main"),
        "{:?}",
        r.calls.borrow()
    );
    assert!(
        !r.called("git -C /x/drover fetch -q origin staging") && !r.called("git -C /x/drover rev-parse origin/staging")
    );
    assert!(!r.called(&format!("gh api repos/jirathip-dev/herdsman/commits/{STAGING}")));
    assert_eq!(installed(), MAIN);

    std::fs::write(&live, "OLD").unwrap();
    let r = heads(&c, &live, &[("staging", MAIN2), ("main", MAIN)]);
    assert_eq!(
        run(&c, &r, &["self-update"]).1,
        "= up to date @ 1111111\n",
        "a newer staging head"
    );
    assert!(!r.called("cargo") && std::fs::read_to_string(&live).unwrap() == "OLD");

    let r = heads(&c, &live, &[("staging", MAIN2), ("main", STAGING)]);
    let (code, out) = run(&c, &r, &["self-update"]);
    assert!(code == 0 && out.starts_with("✓ installed 3333333 at "), "{out}");
    assert_eq!(
        (installed(), std::fs::read_to_string(&live).unwrap()),
        (STAGING.into(), "NEW".into())
    );
    let ev = kinds(&c, "self-updated");
    let got: Vec<_> = ev
        .iter()
        .map(|e| (e.str("branch").unwrap(), e.str("sha").unwrap()))
        .collect();
    assert_eq!(got, [("main", MAIN), ("main", STAGING)]);
}

fn job_args(c: &drover::Ctx, label: &str) -> Vec<String> {
    if cfg!(target_os = "macos") {
        let p = std::fs::read_to_string(c.home.join(format!("Library/LaunchAgents/{label}.plist"))).unwrap();
        let a = p.split("<array>").nth(1).unwrap().split("</array>").next().unwrap();
        a.split("<string>")
            .skip(1)
            .map(|s| s.split("</string>").next().unwrap().to_string())
            .collect()
    } else {
        let p = std::fs::read_to_string(c.home.join(format!(".config/systemd/user/{label}.service"))).unwrap();
        let l = p.lines().find_map(|l| l.strip_prefix("ExecStart=")).unwrap();
        l.split(' ').map(String::from).collect()
    }
}

#[test]
fn the_staging_serve_runs_the_staging_head_and_cannot_write_the_live_state() {
    let text = format!("{FLEET}\n[self_update]\nstaging_serve = true\nstaging_addr = \"127.0.0.1:0\"\n");
    let (c, live) = setup_with("upd-stg", &text);
    let built = c.state.with_file_name("update-staging/target/release/drover");
    std::fs::create_dir_all(built.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_drover"), &built).unwrap();
    let r = heads(&c, &live, &[("staging", STAGING), ("main", MAIN)]);
    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(code, 0, "{out}");
    let bin = c.state.with_file_name("update-staging/bin/drover");
    assert!(
        out.contains(&format!(
            "✓ staging serve: installed 3333333 at {}; staging serve on 127.0.0.1:0",
            bin.display()
        )),
        "{out}"
    );
    assert!(
        r.called(&format!(
            "git -C {} checkout -q --detach {STAGING}",
            c.state.with_file_name("update-staging/src").display()
        )) || r.called("git -C /x/drover worktree add")
    );
    assert_eq!(
        std::fs::read_to_string(c.state.with_file_name("update-staging/sha")).unwrap(),
        STAGING
    );
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "NEW", "main installed apart");
    let args = job_args(&c, "dev.drover.serve-staging");
    assert_eq!(
        args,
        [
            bin.to_string_lossy().as_ref(),
            "serve",
            "--read-only",
            "--addr",
            "127.0.0.1:0"
        ]
    );

    append(&c.state, &Event::new(c.now, "drover", None, "note", json!({}))).unwrap();
    let before = read(&c.state).unwrap().len();
    let mut p = std::process::Command::new(&args[0])
        .args(&args[1..])
        .env("FLEET_CONFIG", &c.config)
        .env("FLEET_STATE", &c.state)
        .env("DROVER_HOME", &c.home)
        .env("PATH", "/usr/bin:/bin")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = std::io::BufReader::new(p.stderr.take().unwrap());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut err, &mut line).unwrap();
    let addr = line
        .trim()
        .strip_prefix("drover serve: read-only view on http://")
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    let post = |path: &str| {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        let body = "fleet=drover";
        let req = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        std::io::Write::write_all(&mut s, req.as_bytes()).unwrap();
        let mut res = String::new();
        let _ = std::io::Read::read_to_string(&mut s, &mut res);
        res
    };
    let paused = post("/pause");
    let queued = post("/queue");
    let _ = p.kill();
    let _ = p.wait();
    assert!(paused.starts_with("HTTP/1.1 405"), "{paused}");
    assert!(queued.starts_with("HTTP/1.1 405"), "{queued}");
    assert_eq!(
        read(&c.state).unwrap().len(),
        before,
        "nothing written to the live state"
    );
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
        .reply(&format!("{} profiles sync", live.display()), "");
    let (code, out) = run(&c, &r, &["self-update", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("would build abcdef1") && !r.called("cargo"), "{out}");

    let old = live.with_file_name("drover.running");
    std::fs::hard_link(&live, &old).unwrap();
    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("✓ installed abcdef1 at "), "{out}");
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "NEW", "swapped in");
    use std::os::unix::fs::MetadataExt;
    let ino = |p: &std::path::Path| std::fs::metadata(p).unwrap().ino();
    assert_ne!(ino(&live), ino(&old), "renamed over, never rewritten in place");
    assert_eq!(
        std::fs::read_to_string(&old).unwrap(),
        "OLD",
        "a running serve keeps its file"
    );
    assert!(!PathBuf::from(&new).exists());
    let at = |p: &str| {
        r.index(p)
            .unwrap_or_else(|| panic!("{p} not called: {:?}", r.calls.borrow()))
    };
    let order = [
        at("cargo build"),
        at(&format!("sh {} sign {new}", sign_script(&c))),
        at(&format!("{new} doctor --config-only")),
        at(&format!("{} profiles sync", live.display())),
        at(if cfg!(target_os = "macos") {
            "launchctl kickstart -k gui/"
        } else {
            "systemctl --user restart"
        }),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    let calls = r.calls.borrow().join("\n");
    assert!(!calls.contains("security ") && !calls.contains("--tokens"), "{calls}");
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
    let r2 = r();
    assert_eq!(run(&c, &r2, &["self-update"]).0, 1);
    assert!(!r2.called("profiles sync"));
    assert_eq!(kinds(&c, "self-update-failed").len(), 1);
}

#[test]
fn a_missing_signing_identity_keeps_the_old_binary_and_alerts() {
    let (c, live) = setup("upd-identity");
    let r = host("SUCCESS").once_err(&format!("sh {} sign", sign_script(&c)), 2, "");
    let (code, out) = run(&c, &r, &["self-update"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("no \"drover-local\" identity") && out.contains("signing-identity.sh"),
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
        .contains("no \"drover-local\" identity"));
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
        (0, "= self-update paused → drover self-update --resume\n")
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
fn scheduled_clean_keeps_its_age_cap_and_live_dirty_rules_and_alerts_a_failure_once() {
    let d = tmpdir("clean-sched");
    let c = ctx(&d, CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    let day = 86_400;
    let lane = |issue: u64, merged_at: u64| {
        let w = c.worktrees.join("sendmeter").join(format!("issue-{issue}"));
        std::fs::create_dir_all(&w).unwrap();
        let w = w.to_string_lossy().into_owned();
        let d = json!({"role": "impl", "agent": format!("impl-{issue}"), "worktree": w, "pane": format!("impl-{issue}"), "runtime": "shed"});
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
    let all: Vec<&String> = removable.iter().chain([&fresh, &live, &dirty]).collect();
    let git = |f: Fake| {
        let f = all
            .iter()
            .fold(f, |f, w| f.reply(&format!("git -C {w} rev-list"), "0\n"));
        f.reply("git -C", "")
    };
    sh.lane("impl-21", "idle", &live);
    let r = git(Fake::default()
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
    for w in &cleaned {
        assert!(
            r.called(&format!("git -C /tmp/does-not-matter/sendmeter worktree remove {w}")),
            "{out}"
        );
    }
    assert!(
        !r.called(&format!("git -C /tmp/does-not-matter/sendmeter worktree remove {live}")),
        "{out}"
    );
    assert!(kinds(&c, "clean-failed").is_empty(), "a passing run alerts nothing");
    cleaned.iter().for_each(|w| std::fs::remove_dir_all(w).unwrap());

    sh.remove("impl-21");
    let r = || git(Fake::default());
    let (code, out) = run(&c, &r(), &["clean", "--scheduled"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(kinds(&c, "clean").len(), 14, "{out}");
    assert!(!kinds(&c, "clean").iter().any(|e| e.str("worktree") == Some(&fresh)));
    drop(sh);
    let (code, out) = run(&c, &r(), &["clean", "--scheduled"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(run(&c, &r(), &["clean", "--scheduled"]).0, 1);
    let failed = kinds(&c, "clean-failed");
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0].str("reason").unwrap().contains("refusing to clean blind"),
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
        out.contains("✓ wrote dev.drover.update") && out.contains("✓ wrote dev.drover.clean"),
        "{out}"
    );
    let (update, clean) = if cfg!(target_os = "macos") {
        let p = |l: &str| std::fs::read_to_string(c.home.join(format!("Library/LaunchAgents/{l}.plist"))).unwrap();
        (p("dev.drover.update"), p("dev.drover.clean"))
    } else {
        let p = |l: &str| std::fs::read_to_string(c.home.join(format!(".config/systemd/user/{l}.timer"))).unwrap();
        (p("dev.drover.update"), p("dev.drover.clean"))
    };
    assert!(update.contains("300") && !update.contains("03:30"), "{update}");
    let daily = [
        "<key>Hour</key><integer>3</integer><key>Minute</key><integer>30</integer>",
        "OnCalendar=*-*-* 03:30:00",
    ];
    assert!(daily.iter().any(|d| clean.contains(d)), "{clean}");
    let (_, out) = run(&c, &r, &["timer", "remove"]);
    assert!(
        out.contains("✓ removed dev.drover.update") && out.contains("✓ removed dev.drover.clean"),
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
    let warn = "! config — [telegram] is removed (#557): drover has no Telegram bridge now; drop the table";
    assert!(out.lines().any(|l| l == warn), "{out}");
}
