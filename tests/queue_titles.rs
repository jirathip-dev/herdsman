//! #36: Queue rows carry the issue + PR titles from the caches only (page and `?frag=all`);
//! the lane display mode button lives in the Lanes tab; sw.js never masks a non-200 page.
mod common;
use common::*;
use herdsman::serve::Server;
use serde_json::json;

fn get(s: &Server, path: &str) -> String {
    String::from_utf8_lossy(&s.respond(&format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n"), 1_000_000)).into_owned()
}

fn queued(name: &str) -> herdsman::Ctx {
    let c = ctx(&tmpdir(name), CONFIG);
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12", "17"]).0, 0);
    c
}

#[test]
fn cached_spec_and_pr_titles_show_in_the_queue_row() {
    let c = queued("titles");
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("issue-sendmeter-12.json"), json!({"title": "Queue titles"}).to_string()).unwrap();
    std::fs::write(cache.join("pr-sendmeter-12.json"), json!({"ts": 1, "branch": "issue-12", "pr": {"number": 33, "title": "Lanes like Corral"}}).to_string()).unwrap();
    std::fs::write(cache.join("issue-sendmeter-17.json"), json!({"title": "A very long issue title that goes well past one phone line"}).to_string()).unwrap();
    let r = Fake::default();
    let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    for out in [get(&s, "/"), get(&s, "/?frag=all")] {
        assert!(out.contains("Queue titles · PR #33 · Lanes like Corral"), "{out}");
        assert!(out.contains("A very long issue title that goes well past one …"), "cut at a word: {out}");
    }
    assert_eq!(r.count("gh"), 0, "no gh call on render: {:?}", r.calls.borrow());
}

#[test]
fn uncached_rows_have_no_title_and_make_no_gh_call() {
    let c = queued("no-titles");
    let r = Fake::default();
    let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let page = get(&s, "/");
    assert!(page.contains("<details class=\"q\">") && !page.contains("class=tl"), "{page}");
    assert_eq!(r.count("gh"), 0, "{:?}", r.calls.borrow());
}

#[test]
fn mode_button_only_in_the_lanes_tab() {
    let c = queued("mode");
    let r = Fake::default();
    let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let page = get(&s, "/");
    let section = |id: &str| {
        let i = page.find(&format!("<section id={id}>")).unwrap();
        page[i..i + page[i..].find("</section>").unwrap()].to_string()
    };
    assert!(section("lanes").contains("id=mode"));
    for id in ["queue", "host", "events"] {
        assert!(!section(id).contains("id=mode"), "{id}");
    }
    assert_eq!(page.matches("id=mode").count(), 1, "not in the global header either");
}

/// sw.js `/`: a non-200 network answer is returned as is (not cached, not swapped for the cache);
/// only a failed fetch falls back. Runs where node is at hand.
#[test]
fn sw_never_serves_the_cache_over_a_non_200() {
    if std::process::Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: no node here");
        return;
    }
    let sw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sw.js")).unwrap();
    let harness = r#"
const on = {}, put = [];
globalThis.self = { addEventListener: (k, f) => on[k] = f };
globalThis.location = { origin: 'http://h' };
globalThis.caches = { open: async () => ({ put: (k) => put.push(k) }), match: async () => 'CACHED' };
"#;
    let run = r#"
const go = async (net) => {
  globalThis.fetch = net;
  let p; on.fetch({ request: { url: 'http://h/', method: 'GET' }, respondWith: x => p = x });
  return p;
};
(async () => {
  const r = await go(async () => ({ ok: false, status: 502, clone() { return this; } }));
  const f = await go(async () => { throw new Error('down'); });
  await new Promise(s => setTimeout(s, 10));
  console.log(JSON.stringify([r.status, f, put.length]));
})();
"#;
    let d = tmpdir("sw");
    std::fs::write(d.join("t.js"), format!("{harness}\n{sw}\n{run}")).unwrap();
    let o = std::process::Command::new("node").arg(d.join("t.js")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), r#"[502,"CACHED",0]"#, "{}", String::from_utf8_lossy(&o.stderr));
}
