//! #39: `herdsman serve --export <dir> --demo` from the committed `demo/`: the file set, an inert page
//! (no POST form, no absolute URL, no live endpoint, no pairing or alerts), canned transcripts, and no
//! call to the passed runner (herdr/gh/host are all canned in `demo/runner.json`).
mod common;
use common::*;
use herdsman::Ctx;
use serde_json::Value;
use std::path::PathBuf;

fn export() -> (PathBuf, Fake) {
    let demo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/demo"));
    let d = tmpdir("demo-export");
    let c = Ctx { config: demo.join("fleet.toml"), state: demo.join("events.jsonl"), worktrees: d.join("wt"), hermes: d.join("hermes"), exe: "herdsman".into(), now: 1, home: d.join("home"), hints: vec![] };
    let r = Fake::default();
    herdsman::serve::export(&c, &r, &d.join("site"), true).unwrap();
    (d.join("site"), r)
}

#[test]
fn demo_export_writes_the_file_set_without_running_anything() {
    let (site, r) = export();
    assert!(r.calls.borrow().is_empty(), "the passed runner was called: {:?}", r.calls.borrow());
    for f in ["index.html", "herd.json", "manifest.webmanifest", "sw.js", "icon-180.png", "icon-192.png", "icon-512.png"] {
        assert!(site.join(f).is_file(), "missing {f}");
    }
    let mut lanes: Vec<String> = std::fs::read_dir(site.join("lane")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    lanes.sort();
    assert_eq!(lanes.len(), 12, "{lanes:?}");
    for l in &lanes {
        let j: Value = serde_json::from_str(&std::fs::read_to_string(site.join("lane").join(l)).unwrap()).unwrap();
        assert!(!j["turns"].as_array().unwrap().is_empty(), "{l}: no turns");
    }
    let dsh: Value = serde_json::from_str(&std::fs::read_to_string(site.join("lane/acme-api-impl-17.json")).unwrap()).unwrap();
    assert_eq!(dsh["harness"], "dsh");
    // Herd: 3 repos plus Others, every lane state, a 6-item queue with one blocked.
    let h: Value = serde_json::from_str(&std::fs::read_to_string(site.join("herd.json")).unwrap()).unwrap();
    let fleets = h["fleets"].as_array().unwrap();
    assert_eq!(fleets.iter().map(|f| f["name"].as_str().unwrap()).collect::<Vec<_>>(), ["acme-api", "acme-web", "docs"]);
    let all: Vec<&Value> = fleets.iter().flat_map(|f| f["lanes"].as_array().unwrap()).chain(h["others"].as_array().unwrap()).collect();
    assert_eq!(all.len(), 13);
    for st in ["working", "idle", "done", "blocked", "dead"] {
        assert!(all.iter().any(|l| l["state"] == st), "no {st} lane");
    }
    assert!(h["others"].as_array().unwrap().iter().any(|l| l["role"] == "other"));
    let q: Vec<&Value> = fleets.iter().flat_map(|f| f["queue"].as_array().unwrap()).collect();
    assert_eq!((q.len(), q.iter().filter(|i| !i["blocked_by"].is_null()).count()), (7, 1));
}

#[test]
fn demo_page_is_inert_and_relative() {
    let (site, _) = export();
    let html = std::fs::read_to_string(site.join("index.html")).unwrap();
    // #57: the scripts and styles sit under a/<hash>/, linked relatively; checked with the page.
    let v = std::fs::read_dir(site.join("a")).unwrap().next().unwrap().unwrap().path();
    let page = ["serve.css", "herd.css", "serve.js", "herd.js", "horses.svg"].iter().fold(html.clone(), |p, f| p + &std::fs::read_to_string(v.join(f)).unwrap());
    assert!(html.contains(&format!("<script src=a/{}/serve.js>", v.file_name().unwrap().to_string_lossy())));
    assert!(page.contains("data-demo=1") && page.contains("id=demo>demo</span>"), "demo badge");
    assert!(page.contains("Demo with fake data. Install herdsman to run your own."));
    assert_eq!(page.matches("<span class=g>merged</span>").count(), 4, "4 merges today");
    assert!(!page.contains("<form method=post") && page.contains("<form inert"), "every form inert");
    for abs in ["href=/", "href=\"/", "src=/", "src=\"/", "action=/", "fetch('/", "register('/", "EventSource('/"] {
        assert!(!page.contains(abs), "absolute URL `{abs}`");
    }
    assert!(page.contains("rel=manifest href=manifest.webmanifest") && page.contains("serviceWorker.register(R + 'sw.js')"));
    // No pairing list, no "Enable alerts", no live stream, no POST, the transcript from lane/<agent>.json.
    assert!(!page.contains("Paired devices") && !page.contains("id=push"));
    for guard in ["function connect() {\n  if (DEMO) return;", "if (DEMO) return herdPull();", "o.method === 'POST' ? Promise.reject", "DEMO ? 'lane/' + encodeURIComponent(agent) + '.json'", "!B.dataset.ro && !DEMO) cleanable()"] {
        assert!(page.contains(guard), "missing demo guard `{guard}`");
    }
    let m = std::fs::read_to_string(site.join("manifest.webmanifest")).unwrap();
    assert!(!m.contains("\"/"), "manifest URLs relative: {m}");
    let sw = std::fs::read_to_string(site.join("sw.js")).unwrap();
    assert!(sw.contains("self.registration.scope") && !sw.contains("'/lane'"), "sw paths under its scope");
    assert!(sw.contains(&format!("P + 'a/{}/'", v.file_name().unwrap().to_string_lossy())), "sw precaches this build's assets");
}
