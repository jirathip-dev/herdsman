//! #37: Herd mode's `/herd.json` (2 fleets + Others, queue at the gate with a blocked item, host
//! weather), its Host/Origin check, no pane text, `?weather=` on loopback only; 390 px snapshots.
mod common;
use common::*;
use herdsman::events::{append, Event};
use herdsman::serve::Server;
use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;
const SCREEN: &str = "⏺ Bash(cargo test)\n  ⎿ PANE-SECRET-TEXT ok\n";

fn fixture(name: &str) -> (herdsman::Ctx, Fake) {
    let cfg = format!("[serve]\nallowed_hosts = [\"mac.example\"]\n\n{CONFIG}\n[fleet.synergy]\nrepo = \"o/synergy-apps\"\nlocal = \"/p/synergy-apps\"\nintegration = \"staging\"\n");
    let c = ctx(&tmpdir(name), &cfg);
    let spawn = |fleet: &str, issue: u64, ts: u64, agent: &str| {
        append(&c.state, &Event::new(ts, fleet, Some(issue), "spawn", json!({"role": "impl", "agent": agent, "harness": "claude"}))).unwrap();
    };
    spawn("sendmeter", 12, 900_100, "sendmeter-impl-12");
    spawn("synergy", 7, 900_400, "synergy-impl-7");
    spawn("synergy", 8, 900_500, "synergy-impl-8");
    for args in [&["queue", "sendmeter", "add", "18", "19", "20"][..], &["queue", "sendmeter", "block", "--on", "18", "19"]] {
        assert_eq!(run(&c, &Fake::default(), args).0, 0);
    }
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("issue-sendmeter-12.json"), json!({"title": "Send meter readings nightly"}).to_string()).unwrap();
    std::fs::write(cache.join("issue-sendmeter-18.json"), json!({"title": "Herd mode"}).to_string()).unwrap();
    let a = |n: &str, st: &str, p: &str| json!({"name": n, "agent_status": st, "pane_id": p, "cwd": "/x"});
    let list = json!({"result": {"agents": [
        a("sendmeter-impl-12", "working", "w1:p2"),
        a("synergy-impl-7", "blocked", "w2:p1"),
        a("synergy-impl-8", "done", "w2:p2"),
        {"agent": "codex", "agent_status": "idle", "pane_id": "w4:p1", "cwd": "/home/me/side", "workspace_id": "w4"},
    ]}});
    let ws = json!({"result": {"workspaces": [{"workspace_id": "w4", "label": "side-project"}]}});
    let r = Fake::default()
        .reply("herdr agent list", &list.to_string())
        .reply("herdr workspace list", &ws.to_string())
        .reply("herdr pane read", SCREEN)
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "2\n")
        .reply("sysctl -n vm.loadavg", "{ 3.90 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Swapouts: 100.\n");
    (c, r)
}

fn get(s: &Server, path: &str, host: &str, extra: &str) -> String {
    String::from_utf8_lossy(&s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}\r\n"), T)).into_owned()
}

fn body(res: &str) -> Value {
    assert!(res.starts_with("HTTP/1.1 200 OK") && res.contains("application/json"), "{res}");
    serde_json::from_str(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap()
}

#[test]
fn herd_json_shape_on_a_fixture() {
    let (c, r) = fixture("shape");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    *s.host.lock().unwrap() = herdsman::host::sample(&r);
    let v = body(&get(&s, "/herd.json", ADDR, ""));
    let names = |l: &Value| l.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let fleets = v["fleets"].as_array().unwrap();
    assert_eq!(fleets.iter().map(|f| f["name"].as_str().unwrap()).collect::<Vec<_>>(), ["sendmeter", "synergy"]);
    assert_eq!(names(&fleets[0]["lanes"]), ["sendmeter-impl-12"]);
    assert_eq!(names(&fleets[1]["lanes"]), ["synergy-impl-7", "synergy-impl-8"]);
    assert_eq!(names(&v["others"]), ["side-project"]);
    let l = &fleets[0]["lanes"][0];
    assert_eq!((&l["issue"], &l["title"], &l["state"], &l["shown"], &l["tone"], &l["role"]), (&json!(12), &json!("Send meter readings nightly"), &json!("working"), &json!("working"), &json!("g"), &json!("impl")));
    assert_eq!(l["age"], "27h");
    assert!(l["coat"].as_str().unwrap().starts_with("--c:#"));
    // The queue at the gate: queued issues only, in order, the blocked one with its blocker.
    let q = &fleets[0]["queue"];
    assert_eq!(q.as_array().unwrap().iter().map(|i| (i["issue"].as_u64().unwrap(), i["blocked_by"].as_u64())).collect::<Vec<_>>(), [(18, None), (19, Some(18)), (20, None)]);
    assert_eq!(q[0]["title"], "Herd mode");
    assert_eq!(fleets[1]["queue"], json!([]));
    // Host: load/cpu 0.39, limit 2.5, memory pressure warn (the gate refuses), swap-out 0.
    let h = &v["host"];
    assert_eq!((&h["limit"], &h["pressure"], &h["swapouts"], &h["gate"]), (&json!(2.5), &json!("warn"), &json!(0.0), &json!("memory pressure warn")));
    assert!((h["load"].as_f64().unwrap() - 0.39).abs() < 1e-9, "{h}");
}

#[test]
fn herd_json_has_no_pane_text() {
    let (c, r) = fixture("nopane");
    let res = get(&Server::new(&c, &r, Some(ADDR.into())), "/herd.json", ADDR, "");
    assert!(!res.contains("PANE-SECRET-TEXT") && !res.contains("cargo test") && !res.contains("turns"), "{res}");
    // Even for a blocked lane (whose Lanes card shows pane text to paired devices).
    assert_eq!(body(&res)["fleets"][1]["lanes"][0]["state"], "blocked");
}

#[test]
fn herd_json_refuses_a_foreign_host_or_origin() {
    let (c, r) = fixture("origin");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    assert!(get(&s, "/herd.json", "evil.example", "").starts_with("HTTP/1.1 403 "));
    assert!(get(&s, "/herd.json", ADDR, "Origin: https://evil.example\r\n").starts_with("HTTP/1.1 403 "));
    assert!(!r.called("herdr agent list"), "refused before any read");
    body(&get(&s, "/herd.json", "mac.example", "Origin: https://mac.example\r\n"));
}

#[test]
fn weather_param_works_on_loopback_only() {
    let (c, r) = fixture("weather");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    *s.host.lock().unwrap() = herdsman::host::sample(&r);
    let storm = body(&get(&s, "/herd.json?weather=storm", ADDR, ""));
    assert_eq!((&storm["host"]["load"], &storm["host"]["pressure"]), (&json!(3.25), &json!("normal")));
    assert!(storm["host"]["gate"].as_str().unwrap().starts_with("load/cpu 3.25 > 2.5"));
    let calm = body(&get(&s, "/herd.json?weather=calm", ADDR, ""));
    assert_eq!((&calm["host"]["gate"], &calm["host"]["pressure"]), (&Value::Null, &json!("normal")));
    // The tailnet name: the param is ignored, the real sample is served.
    let real = body(&get(&s, "/herd.json?weather=calm", "mac.example", ""));
    assert_eq!((&real["host"]["pressure"], &real["host"]["gate"]), (&json!("warn"), &json!("memory pressure warn")));
}

#[test]
fn page_has_the_herd_button_and_horse_symbols() {
    let (c, r) = fixture("page");
    let page = get(&Server::new(&c, &r, Some(ADDR.into())), "/", ADDR, "");
    assert!(page.contains("<button id=herd-on type=button aria-label=\"herd mode\">🐎</button></header>"));
    assert!(page.contains("/herd.js></script>") && std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/herd.js")).unwrap().contains("const H = $('#herd')"));
}

/// The PR's 390 px Herd snapshots (fixture data): calm, gusty, storm and fog; the queue at the gate,
/// a swipe to the next repo, the sheet from a horse; no horizontal page scroll. PNGs go to `$HERDSMAN_SNAPSHOTS`.
#[test]
fn herd_snapshots_fit_390px() {
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let (c, r) = fixture("shots");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/", ADDR, "");
    let herd = |w: &str| get(&s, &format!("/herd.json?weather={w}"), ADDR, "");
    let json = |w: &str| herd(w)[herd(w).find("\r\n\r\n").unwrap() + 4..].to_string();
    let d = tmpdir("herd-png");
    let file = d.join("page.html");
    std::fs::write(&file, &page[page.find("<!doctype").unwrap()..]).unwrap();
    let show = |w: &str| format!("window.fetch = async () => new Response('no pane output', {{status: 404}}); herd(true); herdData({})", json(w));
    let scenes = [
        ("herd-calm", show("calm")),
        ("herd-gusty", show("gusty")),
        ("herd-storm", show("storm")),
        ("herd-fog", show("fog")),
        ("herd-swipe", format!("{}; PG.scrollLeft = PG.clientWidth", show("gusty"))),
        ("herd-queue-tap", format!("{}; document.querySelector('.hqh[data-i=\"19\"]').click()", show("calm"))),
        ("herd-sheet", format!("{}; document.querySelector('.hh[data-agent=\"sendmeter-impl-12\"]').click()", show("calm"))),
    ];
    for (name, js) in scenes {
        let png = std::env::var("HERDSMAN_SNAPSHOTS").map_or(d.join(format!("{name}.png")), |p| std::path::Path::new(&p).join(format!("{name}.png")));
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
        let o = swift().args([script, file.to_str().unwrap(), "390", "lanes", "horse", png.to_str().unwrap(), &js]).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        let w: Vec<u32> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(o.status.success() && w.len() == 2, "{name}: {out} {}", String::from_utf8_lossy(&o.stderr));
        assert!(w[0] <= 390 && w[1] == 390, "{name}: scrollWidth {} at {}", w[0], w[1]);
    }
}

/// #58: the ranch is a ~22% strip, empty bands are not drawn, every lane horse is one size in every
/// band (#65: Corral's fixed 132 px) and queued ones are 53 px (40 %) in their own row above working. 390 px, `?weather=calm`:
/// (a) 0 blocked · 1 working · 13 queued · 7 idle · 11 done, (b) only 1 done, (c) every band.
#[test]
fn herd_horses_are_one_size_at_390px() {
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let (c, r) = fixture("sizes");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/", ADDR, "");
    let mut calm = body(&get(&s, "/herd.json?weather=calm", ADDR, ""));
    let fleet = calm["fleets"][0].clone();
    // One fleet with `n` lanes per state (cloned from the fixture's lane) and `q` queued issues.
    let mut herd = |states: &[(&str, &str, usize)], q: u64| {
        let mut lanes = vec![];
        for &(state, tone, n) in states {
            for _ in 0..n {
                let mut l = fleet["lanes"][0].clone();
                (l["name"], l["state"], l["shown"], l["tone"]) = (json!(format!("sendmeter-impl-{}", 100 + lanes.len())), json!(state), json!(state), json!(tone));
                lanes.push(l);
            }
        }
        let queue: Vec<Value> = (0..q).map(|i| json!({"issue": 200 + i, "title": null, "blocked_by": null, "coat": fleet["queue"][0]["coat"]})).collect();
        calm["fleets"] = json!([{"name": "sendmeter", "lanes": lanes, "queue": queue}]);
        calm["others"] = json!([]);
        calm.to_string()
    };
    let scenes = [
        ("herd-58-a", herd(&[("working", "g", 1), ("idle", "m", 7), ("done", "g", 11)], 13), 4.0, String::new()),
        ("herd-58-b", herd(&[("done", "g", 1)], 0), 1.0, String::new()),
        ("herd-58-c", herd(&[("working", "g", 3), ("idle", "m", 2), ("done", "g", 2), ("blocked", "r", 1), ("dead", "r", 1)], 5), 5.0, String::new()),
    ];
    // The lower bands of (a) and (c): scrolled to a band's header, clear of the sticky strip.
    let to = |band: &str| format!("; document.querySelector('.b-{band}').scrollIntoView({{behavior: 'instant'}}); here().scrollTop -= 64");
    let scenes = scenes.iter().cloned().chain([("herd-58-a2", scenes[0].1.clone(), 4.0, to("done")), ("herd-58-c2", scenes[2].1.clone(), 5.0, to("idle"))]);
    let d = tmpdir("herd-58-png");
    let file = d.join("page.html");
    std::fs::write(&file, &page[page.find("<!doctype").unwrap()..]).unwrap();
    let w = |sel: &str| format!("(document.querySelector('{sel} svg') || {{getBoundingClientRect: () => ({{width: -1}})}}).getBoundingClientRect().width");
    let measure = format!(
        "window.measure = [document.querySelector('.hr').clientHeight, document.querySelectorAll('.band').length, {}, {}, {}, {}, {}]",
        w(".b-working .hh"), w(".b-idle .hh"), w(".b-done .hh"), w(".b-blocked .hh"), w(".b-queue .hqh")
    );
    for (name, json, bands, then) in scenes {
        let js = format!("window.fetch = async () => new Response('', {{status: 404}}); herd(true); herdData({json}); {measure}{then}");
        let png = std::env::var("HERDSMAN_SNAPSHOTS").map_or(d.join(format!("{name}.png")), |p| std::path::Path::new(&p).join(format!("{name}.png")));
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
        let o = swift().args([script, file.to_str().unwrap(), "390", "lanes", "horse", png.to_str().unwrap(), &js]).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        let v: Vec<f64> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(o.status.success() && v.len() == 9, "{name}: {out} {}", String::from_utf8_lossy(&o.stderr));
        let [sw, vw, ranch, n, working, idle, done, blocked, queued] = v[..] else { unreachable!() };
        assert!(sw <= 390.0 && vw == 390.0, "{name}: scrollWidth {sw} at {vw}");
        // 22svh of the 844 px window (≥ 120 px), not 45%.
        assert!((ranch - 0.22 * 844.0).abs() <= 1.0, "{name}: ranch {ranch}px");
        assert_eq!(n, bands, "{name}: bands drawn");
        // #65: Corral's 132 px in every band present; queued 53 px.
        let lane: Vec<f64> = [working, idle, done, blocked].into_iter().filter(|&x| x >= 0.0).collect();
        assert!(lane.iter().all(|&x| (x - 132.0).abs() <= 1.0), "{name}: lane horse widths {lane:?}");
        if queued >= 0.0 {
            assert!((queued - 53.0).abs() <= 1.0, "{name}: queued {queued}");
        }
    }
}
