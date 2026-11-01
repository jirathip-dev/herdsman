//! #115: the Queue tab as a git-log style graph. `serial_label` config, the rows' graph data, and (WebKit, 390 px) the
//! drawn graph on a fixture with chains, a fork, a clamped chain and a blocker out of view, the [?] legend sheet and
//! the control audit on both. Screenshots go to `$HERDSMAN_SNAPSHOTS`.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, Event};
use herdsman::serve::Server;
use serde_json::json;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

/// `acme` names its serial mark, `beta` keeps the default.
fn config(label: &str) -> String {
    format!(
        r#"[fleet.acme]
repo = "acme/app"
local = "/tmp/does-not-matter/app"
integration = "staging"
serial_paths = ["db/migrations/**", "db/seed.sql"]
{label}

[fleet.beta]
repo = "acme/beta"
local = "/tmp/does-not-matter/beta"
integration = "staging"
serial_paths = ["schema/**"]
"#
    )
}

fn load(label: &str) -> anyhow::Result<Config> {
    let d = tmpdir("qg-config");
    std::fs::write(d.join("fleet.toml"), config(label)).unwrap();
    Config::load(&d.join("fleet.toml"))
}

#[test]
fn serial_label_defaults_to_serial_and_takes_an_override() {
    let c = load("").unwrap();
    assert_eq!(c.fleet["acme"].serial_label(), "serial");
    let c = load("serial_label = \"db\"").unwrap();
    assert_eq!(c.fleet["acme"].serial_label(), "db");
    assert_eq!(c.fleet["beta"].serial_label(), "serial");
    for bad in ["serial_label = \"\"", "serial_label = \"migrations\""] {
        let e = format!("{:#}", load(bad).unwrap_err());
        assert!(
            e.contains("fleet.acme.serial_label: must be 1 to 8 characters"),
            "{bad}: {e}"
        );
    }
}

fn req(s: &Server, path: &str, extra: &str) -> String {
    String::from_utf8_lossy(&s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}\r\n"), T)).into_owned()
}

/// acme: 1 → 2 → 3 → 4 → 5 (5 clamps at depth 4), 6 also on 1 (a fork), 7 on #99 (not queued), 8 serial.
/// beta: 22 on 21, both serial. #1 has an impl lane.
fn fixture() -> (herdsman::Ctx, Fake) {
    let c = ctx(&tmpdir("queue-graph"), &config("serial_label = \"db\""));
    let ok = |args: &[&str]| assert_eq!(run(&c, &Fake::default(), args).0, 0, "{args:?}");
    ok(&["queue", "acme", "add", "1", "2", "3", "4", "5", "6", "7"]);
    ok(&["queue", "acme", "add", "--serial", "8"]);
    for (n, on) in [("2", "1"), ("3", "2"), ("4", "3"), ("5", "4"), ("6", "1"), ("7", "99")] {
        ok(&["queue", "acme", "block", "--on", on, n]);
    }
    ok(&["queue", "beta", "add", "--serial", "21", "22"]);
    ok(&["queue", "beta", "block", "--on", "21", "22"]);
    let spawn = json!({"role": "impl", "agent": "acme-impl-1", "harness": "claude"});
    append(&c.state, &Event::new(900_000, "acme", Some(1), "spawn", spawn)).unwrap();
    (c, Fake::default())
}

#[test]
fn rows_carry_the_graph_data_and_the_fleet_its_serial_paths() {
    let (c, r) = fixture();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = req(&s, "/", "");
    assert!(page.contains("<button type=button class=qhelp aria-label=\"what the marks mean\">?</button>"));
    assert!(page.contains("<h2 data-s=\"db\" data-sp=\"db/migrations/**, db/seed.sql\"><span>acme</span>"));
    assert!(page.contains("<h2 data-s=\"serial\" data-sp=\"schema/**\"><span>beta</span>"));
    assert!(page.contains("<details class=\"q\" data-q><summary><span>#1</span>"));
    assert!(page.contains("<details class=\"q\" data-q data-b=99><summary><span>#7</span>"));
    assert!(page.contains("<details class=\"q\" data-q data-s=\"db\"><summary><span>#8</span>"));
    assert!(page.contains("<details class=\"q\" data-q data-b=21 data-s=\"serial\"><summary><span>#22</span>"));
    // Change 1 of the approval: the card no longer repeats `#n state · age`; the lane stays.
    assert!(!page.contains("<p>#1 impl"), "{page}");
    assert!(page.contains("<p>acme-impl-1</p>"));
}

#[test]
fn the_graph_and_its_legend_pass_the_audit_at_390px() {
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let (c, r) = fixture();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = req(&s, "/", &pair(&s, &c, "Mozilla/5.0 (iPhone)", T));
    let d = tmpdir("queue-graph-png");
    let file = d.join("page.html");
    std::fs::write(&file, &page[page.find("<!doctype").unwrap()..]).unwrap();
    // Per row in page order: #n, its gutter, its state cell with the tags, its moves.
    let want = [
        "#1 ● impl ▼after7",
        "#2 ├─○ queued ▼after6",
        "#3 │ └─○ queued",
        "#4 │   └─○ queued",
        "#5 │   ┆─○ queued on #4",
        "#6 └─○ queued ▲before2",
        "#7 ○ queued ! #99 ▲before1 ▼after8",
        "#8 ○ queued 🔒db ▲before7",
        "#21 ○ queued 🔒serial",
        "#22 └─○ queued 🔒serial",
    ]
    .join("|");
    let graph = format!(
        "const got = [...document.querySelectorAll('#queue details.q')].map(d => [d.querySelector('summary>span').textContent, \
         d.querySelector('.gt').textContent + ' ' + d.querySelector('summary>span:nth-of-type(2)').textContent, \
         ...[...d.querySelectorAll('[name=before],[name=after]')].map(i => (i.name === 'before' ? '▲' : '▼') + i.name + i.value)].join(' ')).join('|'); \
         if (got !== '{want}') throw new Error('graph: ' + got); \
         if (document.querySelector('details.q[data-s] .tag').getAttribute('aria-label') !== 'serial: one lane at a time') throw new Error('lock label'); \
         if (document.querySelector('details.q summary>span:nth-of-type(2)>span').title !== 'impl = being built') throw new Error('no state hint'); \
         document.querySelectorAll('#queue details').forEach((d, i) => {{ d.open = i === 0; }})"
    );
    let legend = "legend(); const l = [...document.querySelectorAll('.top p')].map(p => p.textContent).join('|'); \
         for (const w of ['queued = waiting its turn', 'review(n) = being checked, round n', 'needs you = waiting for your answer', \
         '! needs you', '🔒db one at a time: touches db/migrations/**, db/seed.sql', '🔒serial one at a time: touches schema/**']) \
         if (!l.includes(w)) throw new Error('legend: ' + l); \
         sh.style.willChange = 'auto'; sh.style.transition = 'none'";
    let stub = "window.fetch = async () => new Response('no pane output', {status: 404})";
    let audit = include_str!("ui/audit.js");
    for (name, root, then) in [
        ("ui-queue-graph", "#queue", graph),
        ("ui-queue-legend", "#sheet", legend.to_string()),
    ] {
        let js =
            format!("{stub}; {audit}; {{ {then} }}; window.measure = [audit('{root}')]; tab('queue'); {{ {then} }}");
        let png = std::env::var("HERDSMAN_SNAPSHOTS").map_or(d.join(format!("{name}.png")), |p| {
            std::path::Path::new(&p).join(format!("{name}.png"))
        });
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
        let o = swift()
            .args([
                script,
                file.to_str().unwrap(),
                "390",
                "queue",
                "horse",
                png.to_str().unwrap(),
                &js,
            ])
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        let v: Vec<f64> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(
            o.status.success() && v.len() == 3,
            "{name}: {out} {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            v[0] <= 390.0 && v[1] == 390.0,
            "{name}: scrollWidth {} at {}",
            v[0],
            v[1]
        );
        // The legend sheet has no controls of its own (the lane parts are hidden); [?] is audited on the queue.
        assert!(v[2] > 0.0 || name == "ui-queue-legend", "{name}: no controls audited");
    }
}
