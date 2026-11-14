mod common;
use common::*;
use drover::events::{append, read, Event};
use serde_json::json;
use std::path::Path;

const MAC: bool = cfg!(target_os = "macos");

fn host(r: Fake) -> Fake {
    r.reply("which hermes", "/opt/hermes/bin/hermes\n")
        .reply("which gh", "/opt/homebrew/bin/gh\n")
        .reply("which shed", "/opt/homebrew/bin/shed\n")
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.00 1.00 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "8\n")
        .reply("vm_stat", "Swapouts: 5.\n")
}

#[test]
fn job_files_match_their_goldens() {
    let d = tmpdir("timer-golden");
    let c = ctx(&d, CONFIG);
    let path = "/opt/hermes/bin:/usr/bin:/bin:/usr/sbin:/sbin";
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/timer");
    for mac in [true, false] {
        for (label, args, every) in [
            ("dev.drover.tick", &["/x/drover", "tick"][..], 300),
            (
                "dev.drover.serve",
                &["/x/drover", "serve", "--addr", "127.0.0.1:7777"][..],
                0,
            ),
        ] {
            for (p, text) in drover::timer::files(&c, label, args, path, every, mac) {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                let text = text.replace(&d.display().to_string(), "<dir>");
                if std::env::var_os("BLESS").is_some() {
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join(&name), &text).unwrap();
                }
                assert_eq!(text, std::fs::read_to_string(dir.join(&name)).unwrap(), "{name}");
            }
        }
    }
}

fn job(c: &drover::Ctx, label: &str) -> std::path::PathBuf {
    match MAC {
        true => c.home.join(format!("Library/LaunchAgents/{label}.plist")),
        false => c.home.join(format!(".config/systemd/user/{label}.service")),
    }
}

#[test]
fn install_no_load_writes_without_loading_and_remove_deletes() {
    let c = ctx(&tmpdir("timer-install"), CONFIG);
    let _sh = FakeShed::attached(&c);
    let r = host(Fake::default());
    let (code, out) = run(&c, &r, &["timer", "install", "--no-load"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("✓ wrote dev.drover.tick; load it: "), "{out}");
    let text = std::fs::read_to_string(job(&c, "dev.drover.tick")).unwrap();
    let path = "/opt/hermes/bin:/opt/homebrew/bin:/opt/drover/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
    assert!(text.contains(path) && text.contains("tick"), "{text}");
    assert!(!r.called("launchctl bootstrap") && !r.called("systemctl --user enable"));
    assert_eq!(
        run(&c, &r, &["timer", "install", "--no-load"]).1,
        "= already dev.drover.tick\n= already dev.drover.fast\n= already dev.drover.update\n= already dev.drover.clean\n"
    );
    let fast = std::fs::read_to_string(job(&c, "dev.drover.fast")).unwrap();
    let every = match MAC {
        true => fast.clone(),
        false => std::fs::read_to_string(c.home.join(".config/systemd/user/dev.drover.fast.timer")).unwrap(),
    };
    assert!(fast.contains("--fast") && every.contains("60"), "{fast}\n{every}");

    let (code, out) = run(&c, &r, &["timer", "remove"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "✓ removed dev.drover.tick\n✓ removed dev.drover.fast\n✓ removed dev.drover.update\n✓ removed dev.drover.clean\n")
    );
    assert!(!job(&c, "dev.drover.tick").exists() && !job(&c, "dev.drover.fast").exists());
    assert!(r.called("launchctl bootout") || r.called("systemctl --user disable --now dev.drover.tick.timer"));
    assert_eq!(
        run(&c, &r, &["timer", "remove"]).1,
        "= no dev.drover.tick job\n= no dev.drover.fast job\n= no dev.drover.update job\n= no dev.drover.clean job\n"
    );
}

#[test]
fn install_loads_and_prints_the_command() {
    let c = ctx(&tmpdir("timer-load"), CONFIG);
    let _sh = FakeShed::attached(&c);
    let r = host(Fake::default());
    let (code, out) = run(&c, &r, &["timer", "install"]);
    let cmd = if MAC {
        "launchctl bootstrap gui/"
    } else {
        "systemctl --user enable --now dev.drover.tick.timer"
    };
    assert_eq!(code, 1, "the fake has no {cmd}: {out}");
    assert!(
        out.starts_with("✗ dev.drover.tick: ") && out.contains(&format!("→ {cmd}")),
        "{out}"
    );
    let unit = |job: &str| {
        if MAC {
            cmd.to_string()
        } else {
            format!("systemctl --user enable --now dev.drover.{job}.timer")
        }
    };
    let jobs = ["fast", "update", "clean"];
    let r = jobs
        .iter()
        .fold(host(Fake::default()).reply(cmd, ""), |r, j| r.reply(&unit(j), ""));
    let (code, out) = run(&c, &r, &["timer", "install"]);
    assert_eq!((code, out.starts_with(&format!("✓ {cmd}"))), (0, true), "{out}");
    for (i, j) in jobs.iter().enumerate() {
        let line = out.lines().nth(i + 1).unwrap_or_default();
        assert!(line.starts_with(&format!("✓ {}", unit(j))), "{out}");
        assert!(line.contains(&format!("dev.drover.{j}")), "{out}");
    }
    let (_, out) = run(&c, &r, &["timer", "status"]);
    assert_eq!(out, "dev.drover.tick: not loaded · tick ran never\n");
}

#[test]
fn tick_runs_check_then_drive_and_exits_zero_on_refusals() {
    let c = ctx(&tmpdir("timer-tick"), CONFIG);
    let _sh = FakeShed::attached(&c);
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let r = host(Fake::default().once("sysctl -n kern.memorystatus_vm_pressure_level", 0, "4\n"))
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("git -C", "");
    let (code, out) = run(&c, &r, &["tick"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("sendmeter #12 start: REFUSED HOST_BUSY"), "{out}");
    for w in ["check.at", "drive.at"] {
        assert_eq!(
            std::fs::read_to_string(c.state.with_file_name(w)).unwrap(),
            "1000000",
            "{w}"
        );
    }
    assert!(!read(&c.state).unwrap().iter().any(|e| e.kind == "tick-failed"));
}

#[test]
fn tick_failed_is_logged_at_most_once_an_hour() {
    let d = tmpdir("timer-fail");
    let c = ctx(&d, "[fleet.x\nbroken");
    let fails = |c: &drover::Ctx| {
        let (code, out) = run(c, &host(Fake::default()), &["tick"]);
        assert_eq!(code, 1, "{out}");
        assert!(out.starts_with("tick failed: "), "{out}");
        read(&c.state)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "tick-failed")
            .count()
    };
    assert_eq!(fails(&c), 1);
    assert_eq!(
        fails(&drover::Ctx {
            now: c.now + 300,
            ..c.clone()
        }),
        1
    );
    assert_eq!(
        fails(&drover::Ctx {
            now: c.now + 3601,
            ..c.clone()
        }),
        2
    );
    let c = ctx(&tmpdir("timer-noshed"), CONFIG);
    let (code, out) = run(&c, &Fake::default(), &["tick"]);
    assert_eq!(code, 1, "{out}");
    let e = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .find(|e| e.kind == "tick-failed")
        .unwrap();
    assert!(e.str("reason").unwrap().starts_with("no shed runtime"), "{e:?}");
}

fn steps(out: &str) -> Vec<&str> {
    out.lines().take(5).collect()
}

#[test]
fn setup_twice_is_all_already_and_tailscale_absent_is_skipped() {
    let cfg = format!("{CONFIG}\n[serve]\ntailscale_https_port = 7443\n");
    let c = ctx(&tmpdir("timer-setup"), &cfg);
    let _sh = FakeShed::attached(&c);
    let probe = if MAC {
        "launchctl print"
    } else {
        "systemctl --user is-active"
    };
    let load = if MAC {
        "launchctl bootstrap"
    } else {
        "systemctl --user enable"
    };
    let r = host(Fake::default())
        .once(probe, 1, "")
        .once(probe, 1, "")
        .reply(probe, "")
        .reply(load, "");
    let (_, out) = run(&c, &r, &["setup"]);
    let s = steps(&out);
    assert!(s[0].starts_with("✓ ") && s[0].contains("dev.drover.tick"), "{out}");
    assert!(s[1].starts_with("✓ ") && s[1].contains("dev.drover.fast"), "{out}");
    assert!(s[2].starts_with("✓ ") && s[2].contains("dev.drover.serve"), "{out}");
    assert_eq!(s[3], "= tailscale serve skipped: tailscale is not on PATH");
    assert_eq!(s[4], "✓ profiles synced");
    assert!(
        out.lines()
            .skip(5)
            .any(|l| l.contains(" doctor") || l.contains(" host — ")),
        "{out}"
    );
    let serve = std::fs::read_to_string(job(&c, "dev.drover.serve")).unwrap();
    assert!(
        serve.contains("127.0.0.1:7777") && serve.contains("serve.log"),
        "{serve}"
    );

    let (_, out) = run(&c, &r, &["setup"]);
    assert!(steps(&out).iter().all(|l| l.starts_with("= ")), "{out}");
    assert!(!r.called("tailscale serve --bg"));
}

#[test]
fn setup_makes_a_missing_merge_label_once_then_says_already() {
    let cfg = CONFIG.replace(
        "integration = \"staging\"",
        "integration = \"staging\"\nmerge_label = \"merge-ready\"",
    );
    let c = ctx(&tmpdir("timer-label"), &cfg);
    let _sh = FakeShed::attached(&c);
    let list = "gh label list --repo jirathip-dev/sendmeter --search merge-ready --json name --limit 100";
    let create = "gh label create merge-ready --repo jirathip-dev/sendmeter --force";
    let r = host(Fake::default())
        .reply("launchctl print", "")
        .reply("systemctl --user is-active", "")
        .once(list, 0, r#"[{"name":"merge-ready-old"}]"#)
        .reply(list, r#"[{"name":"Merge-Ready"}]"#)
        .reply(create, "");
    let (_, out) = run(&c, &r, &["setup", "--no-load"]);
    let label = "label merge-ready in jirathip-dev/sendmeter";
    assert!(out.lines().any(|l| l == format!("✓ {label}")), "{out}");
    assert!(
        out.lines()
            .any(|l| l == "✓ label sendmeter — merge-ready in jirathip-dev/sendmeter"),
        "{out}"
    );
    let (_, out) = run(&c, &r, &["setup", "--no-load"]);
    assert!(out.lines().any(|l| l == format!("= already {label}")), "{out}");
    assert_eq!(r.count(create), 1);
}

#[test]
fn setup_adopts_a_serve_job_under_another_label() {
    let c = ctx(&tmpdir("timer-adopt"), CONFIG);
    let _sh = FakeShed::attached(&c);
    let theirs = c.home.join("Library/LaunchAgents/com.someone.drover-serve.plist");
    std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    std::fs::write(
        &theirs,
        "<plist><array><string>/x/drover</string><string>serve</string></array></plist>",
    )
    .unwrap();
    let (_, out) = run(
        &c,
        &host(Fake::default()).reply("launchctl print", ""),
        &["setup", "--no-load"],
    );
    if MAC {
        assert_eq!(
            steps(&out)[2],
            format!("= adopted {} (re-labelling is manual: docs/setup.md)", theirs.display())
        );
        assert!(!job(&c, "dev.drover.serve").exists());
    } else {
        assert!(steps(&out)[2].contains("dev.drover.serve"), "{out}");
    }
    assert_eq!(
        steps(&out)[3],
        "= tailscale serve skipped: [serve] tailscale_https_port is unset"
    );
}

fn rows(c: &drover::Ctx, r: &Fake) -> Vec<(String, &'static str, String, String)> {
    drover::timer::services(c, &drover::config::Config::load(&c.config).unwrap(), r, None)
}

#[test]
fn services_rows_go_green_amber_and_red() {
    let c = ctx(&tmpdir("timer-svc"), CONFIG);
    let _sh = FakeShed::attached(&c);
    let stamp = |w: &str, ago: u64| std::fs::write(c.state.with_file_name(w), (c.now - ago).to_string()).unwrap();
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    stamp("check.at", 60);
    stamp("drive-sendmeter.last", 60);
    std::fs::write(c.state.with_file_name("drive-fast.at"), format!("{} 2.1", c.now - 40)).unwrap();
    let fleet_disk = |gb: u64| {
        std::fs::write(
            c.state.with_file_name("worktrees.du"),
            format!("{} {}", gb << 30, c.now - 240),
        )
        .unwrap()
    };
    fleet_disk(3);
    assert_eq!(run(&c, &host(Fake::default()), &["timer", "install", "--no-load"]).0, 0);
    let df = |gb: u64| {
        format!(
            "Filesystem 1K-blocks Used Available\n/dev/disk1 1 1 {}\n",
            gb * 1_048_576
        )
    };
    let green = host(Fake::default())
        .reply("lsof ", "drover 1 u LISTEN\n")
        .reply("df -k /", &df(200))
        .reply("launchctl print", "")
        .reply("systemctl --user is-active", "");
    let all = rows(&c, &green);
    let names: Vec<_> = all.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "board",
            "tick",
            "drive sendmeter",
            "fast step",
            "kick",
            "phone alerts",
            "disk",
            "fleet disk"
        ]
    );
    for (item, tone, detail, _) in &all {
        let want = match item.as_str() {
            "phone alerts" => "a",
            "kick" => "m",
            _ => "g",
        };
        assert_eq!(*tone, want, "{item}: {detail}");
    }
    assert_eq!(all[1].2, "ran 60s ago");
    assert_eq!(all[3].2, "fast step 2.1 s · ran 40s ago");
    assert_eq!(all[4].2, "last kick never");
    assert_eq!(all[6].2, "200 GB free");
    assert_eq!(
        all[7].2, "▕█░░░░░░░░░▏ 3.0 / 25 GB · 4m ago",
        "#164: from the last walk, against the budget"
    );
    let k = serde_json::json!({"why": "push", "result": "sendmeter: started #2940"});
    let e = drover::events::Event::new(c.now - 12, "sendmeter", Some(2940), "kick", k);
    drover::events::append(&c.state, &e).unwrap();
    assert_eq!(rows(&c, &green)[4].2, "last kick 12s ago (push #2940)");

    stamp("check.at", 700);
    let amber = host(Fake::default())
        .reply("lsof ", "x")
        .reply("df -k /", &df(12))
        .reply("launchctl print", "")
        .reply("systemctl --user is-active", "");
    let all = rows(&c, &amber);
    fleet_disk(21);
    assert_eq!((all[1].1, all[6].1), ("a", "a"), "{all:?}");
    assert_eq!(rows(&c, &amber)[7].1, "a", "over 80 % of the budget");

    append(
        &c.state,
        &Event::new(
            c.now - 30,
            "drover",
            None,
            "tick-failed",
            json!({"key": "tick", "reason": "shed: gone"}),
        ),
    )
    .unwrap();
    stamp("drive-sendmeter.last", 1000);
    fleet_disk(26);
    stamp("drive-fast.at", 181);
    let red = Fake::default().reply("df -k /", &df(5));
    let all = rows(&c, &red);
    let tones: Vec<_> = all.iter().map(|r| r.1).collect();
    assert_eq!(tones, ["r", "r", "r", "r", "m", "a", "r", "r"], "{all:?}");
    assert_eq!(all[3].2, "ran 3m ago · job missing");
    assert_eq!(all[7].2, "▕██████████▏ 26.0 / 25 GB · 4m ago");
    assert_eq!(all[1].2, "failed: shed: gone · job missing");
    assert_eq!(
        (all[0].3.as_str(), all[6].3.as_str()),
        ("drover setup", "drover clean --yes")
    );

    let s = drover::serve::Server::new(&c, &red, Some("127.0.0.1:7777".into()));
    let page = host_tab(&s, "127.0.0.1:7777", c.now);
    assert!(page.contains("<span class=gn>SERVICES</span>"), "{page}");
    assert!(
        page.contains("/ 25 GB</span> (104%) <span class=\"mt fx\">· 4m ago</span>"),
        "{page}"
    );
    for (k, d) in [
        ("tick", "failed: shed: gone · job missing"),
        ("fast step", "ran 3m ago · job missing"),
    ] {
        let want = format!("<span class=k>{k}</span><span class=w><b class=r>{d}</b> <span class=\"mt fx\">→ drover");
        assert!(page.contains(&want), "{want}\n{page}");
    }
}

#[test]
fn slow_du_never_blocks_the_board() {
    struct SlowDu(Fake);
    impl drover::runner::Runner for SlowDu {
        fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
            if prog == "du" {
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
            self.0.run(prog, args)
        }
    }
    let c = ctx(&tmpdir("timer-slow-du"), CONFIG);
    let _sh = FakeShed::attached(&c);
    let r = SlowDu(host(Fake::default()).reply("df -k /", "F\n/dev/disk1 1 1 209715200\n"));
    let s = drover::serve::Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    for path in ["/?all=1", "/api/v1/host", "/api/v1/queue"] {
        let t = std::time::Instant::now();
        let res = s.respond(&format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n"), c.now);
        assert!(t.elapsed().as_millis() < 500, "{path} took {:?}", t.elapsed());
        assert!(String::from_utf8_lossy(&res).starts_with("HTTP/1.1 200"), "{path}");
    }
    assert_eq!(run(&c, &r, &["check"]).0, 0);
    let du = std::fs::read_to_string(c.state.with_file_name("worktrees.du")).unwrap();
    assert!(du.lines().next().unwrap().ends_with(&format!(" {}", c.now)), "{du}");
}

const PROBE: &str = if MAC {
    "launchctl print"
} else {
    "systemctl --user is-active"
};
const LOAD: &str = if MAC {
    "launchctl bootstrap"
} else {
    "systemctl --user enable"
};

#[test]
fn drift_is_missing_outdated_or_loaded() {
    let c = ctx(&tmpdir("timer-drift"), CONFIG);
    let _sh = FakeShed::attached(&c);
    let cfg = drover::config::Config::load(&c.config).unwrap();
    let up = host(Fake::default()).reply(PROBE, "");
    let drift = |r: &Fake| drover::timer::drift(&c, &cfg, r);
    let both = |a, b| vec![("dev.drover.tick", a), ("dev.drover.fast", b)];
    assert_eq!(drift(&up), both("missing", "missing"));
    assert_eq!(run(&c, &up, &["timer", "install", "--no-load"]).0, 0);
    assert_eq!(drift(&up), both("loaded", "loaded"));
    assert_eq!(drift(&host(Fake::default())), both("missing", "missing"), "not loaded");
    let edit = |label: &str, from: &str, to: &str| {
        let p = match MAC || !from.starts_with("OnUnit") {
            true => job(&c, label),
            false => c.home.join(format!(".config/systemd/user/{label}.timer")),
        };
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains(from), "{text}");
        std::fs::write(&p, text.replace(from, to)).unwrap();
    };
    edit("dev.drover.tick", "/usr/sbin:/sbin", "/usr/sbin:/sbin:/opt/x");
    assert_eq!(drift(&up), both("loaded", "loaded"));
    let (from, to) = if MAC {
        (">60<", ">120<")
    } else {
        ("OnUnitActiveSec=60", "OnUnitActiveSec=120")
    };
    edit("dev.drover.fast", from, to);
    assert_eq!(drift(&up), both("loaded", "outdated (args or interval differ)"));
    let (from, to) = if MAC {
        (">tick<", ">check<")
    } else {
        (" tick\n", " check\n")
    };
    edit("dev.drover.tick", from, to);
    assert_eq!(drift(&up)[0].1, "outdated (args or interval differ)");
    if MAC {
        edit("dev.drover.fast", "<key>AbandonProcessGroup</key><true/>\n", "");
        assert_eq!(drift(&up)[1].1, "outdated (no AbandonProcessGroup)");
    }
}

#[test]
fn tick_repairs_only_the_missing_job() {
    let c = ctx(&tmpdir("timer-repair"), CONFIG);
    let _sh = FakeShed::attached(&c);
    let r = host(Fake::default())
        .reply(PROBE, "")
        .reply(LOAD, "")
        .reply("git -C", "");
    assert_eq!(run(&c, &r, &["timer", "install", "--no-load"]).0, 0);
    std::fs::remove_file(job(&c, "dev.drover.fast")).unwrap();
    let (code, out) = run(&c, &r, &["tick"]);
    assert_eq!(code, 0, "{out}");
    let loads: Vec<_> = r
        .calls
        .borrow()
        .iter()
        .filter(|l| l.starts_with(LOAD))
        .cloned()
        .collect();
    assert!(loads.len() == 1 && loads[0].contains("dev.drover.fast"), "{loads:?}");
    assert!(job(&c, "dev.drover.fast").exists());
    let events = read(&c.state).unwrap().to_vec();
    let fixed: Vec<_> = events.iter().filter(|e| e.kind == "timer-repaired").collect();
    assert_eq!(fixed.len(), 1, "{events:?}");
    assert_eq!(
        (fixed[0].str("label"), fixed[0].str("was")),
        (Some("dev.drover.fast"), Some("missing"))
    );

    std::fs::remove_file(job(&c, "dev.drover.fast")).unwrap();
    let r = host(Fake::default()).reply(PROBE, "").reply("git -C", "");
    for _ in 0..2 {
        assert_eq!(run(&c, &r, &["tick"]).0, 0);
    }
    let events = read(&c.state).unwrap().to_vec();
    let count = |k: &str| events.iter().filter(|e| e.kind == k).count();
    assert_eq!(
        (count("timer-repaired"), count("timer-repair-failed")),
        (1, 1),
        "{events:?}"
    );
}

#[test]
fn install_beside_an_old_job_prints_the_swap_and_runs_none_of_it() {
    let c = ctx(&tmpdir("timer-swap"), CONFIG);
    let old = job(&c, "dev.herdsman.tick");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "old").unwrap();
    let r = host(Fake::default());
    let (_, out) = run(&c, &r, &["timer", "install"]);
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with("✓ wrote dev.drover.tick; swap it in: ")
            && first.contains("dev.herdsman.tick")
            && first.contains(&format!("rm {} && ", old.display())),
        "{out}"
    );
    assert!(
        job(&c, "dev.drover.tick").exists() && old.exists(),
        "written, the old job left"
    );
    let calls = r.calls.borrow().join("\n");
    let ran = |a: &str, b: &str| calls.lines().any(|l| l.contains(a) && l.contains(b));
    assert!(
        !calls.contains("dev.herdsman") && !ran("bootstrap", "dev.drover.tick.plist"),
        "{calls}"
    );
    assert!(!ran("enable", "dev.drover.tick"), "{calls}");
}
