//! #343: the event log's folded checkpoint and its rotation out of the live log (#344: archived rows in `state.db`).
mod common;
use common::tmpdir;
use herdsman::events::{append, checkpoint, fold, history, read, rotate, states, Event, States};
use serde_json::{json, Value};
use std::path::Path;

/// A deterministic xorshift, so a failing case replays.
struct Rng(u64);
impl Rng {
    fn n(&mut self, k: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % k
    }
}

/// `n` events over 12 issues of two fleets: spawns, verdicts, merges, closes, kills, extensions, passive and queue rows.
fn log(seed: u64, n: u64, t0: u64) -> Vec<Event> {
    let mut r = Rng(seed);
    (0..n)
        .map(|i| {
            let (fleet, issue) = (["a", "b"][r.n(2) as usize], 1 + r.n(6));
            let role = ["impl", "rev"][r.n(2) as usize];
            let agent = format!("{fleet}-{role}-{issue}-{}", r.n(3));
            let sha = format!("{:040}", r.n(4));
            let (kind, d) = match r.n(11) {
                0..=2 => (
                    "spawn",
                    json!({"role": role, "agent": agent, "branch": format!("issue-{issue}")}),
                ),
                3..=4 => (
                    "verdict",
                    json!({"sha": sha, "verdict": (["PASS", "FAIL"][r.n(2) as usize])}),
                ),
                5 => (["merged", "closed", "needs-human"][r.n(3) as usize], json!({})),
                6 => ("clean", json!({"why": (["merged", "closed"][r.n(2) as usize])})),
                7 => ("lane-killed", json!({"role": role, "agent": agent})),
                8 => ("round-extended", json!({})),
                9 => ("stall", json!({"reason": "quiet"})),
                _ => ("queue", json!({"op": "add", "issues": [issue]})),
            };
            let issue = (kind != "queue").then_some(issue);
            Event::new(t0 + i * 600, fleet, issue, kind, d)
        })
        .collect()
}

fn same(a: &States, b: &States) {
    assert_eq!(
        serde_json::to_value(a.values().collect::<Vec<_>>()).unwrap(),
        serde_json::to_value(b.values().collect::<Vec<_>>()).unwrap()
    );
}

/// Bulk rows numbered on from the log's last, as an old JSONL log the next read imports (#344).
fn write(p: &Path, ev: &[Event]) {
    let n = read(p).unwrap().last().map_or(0, |e| e.seq);
    let rows: String = (ev.iter().zip(n + 1..))
        .map(|(e, seq)| serde_json::to_string(&Event { seq, ..e.clone() }).unwrap() + "\n")
        .collect();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
    std::io::Write::write_all(&mut f, rows.as_bytes()).unwrap();
}

#[test]
fn a_checkpoint_plus_its_tail_folds_like_the_whole_log() {
    for seed in 1..=40u64 {
        let d = tmpdir("cp");
        let p = d.join("events.jsonl");
        let ev = log(seed, 120, 1_000);
        let cut = (seed * 7 % 120) as usize;
        write(&p, &ev[..cut]);
        checkpoint(&p).unwrap();
        write(&p, &ev[cut..]);
        same(&states(&p).unwrap(), &fold(&read(&p).unwrap()));
        // A second checkpoint mid-way, then more rows: still the whole fold.
        checkpoint(&p).unwrap();
        write(&p, &log(seed + 99, 30, 200_000));
        same(&states(&p).unwrap(), &fold(&read(&p).unwrap()));
    }
}

#[test]
fn rotation_keeps_every_event_and_deleting_the_checkpoint_is_harmless() {
    for seed in 1..=10u64 {
        let d = tmpdir("rot");
        let p = d.join("events.jsonl");
        let now = 90 * 86_400;
        write(&p, &log(seed, 400, 0)); // 400 × 10 min: all older than 30 days
        let recent = (0..40).map(|i| Event::new(now - 86_400 + i, "a", Some(1), "stall", json!({})));
        write(&p, &recent.collect::<Vec<_>>());
        let before = read(&p).unwrap();
        let full = fold(&before);
        let size = herdsman::db::live_bytes(&p).unwrap();
        rotate(&p, size + 1, now).unwrap();
        assert_eq!(read(&p).unwrap().len(), before.len(), "under the limit nothing moves");
        rotate(&p, 1, now).unwrap();
        assert!(herdsman::db::live_bytes(&p).unwrap() < size, "the live log shrank");
        assert!(read(&p).unwrap().len() < before.len());
        let back = history(&p).unwrap();
        let rows = |v: &[Event]| {
            v.iter()
                .map(|e| serde_json::to_value(e).unwrap())
                .collect::<Vec<Value>>()
        };
        assert_eq!(
            rows(&back),
            rows(&before),
            "the archives and the live log hold every event once"
        );
        same(&states(&p).unwrap(), &full);
        // The live log alone still says what is open, queued and done.
        let live = fold(&read(&p).unwrap());
        for (k, s) in &full {
            let l = live.get(k).map(|s| (s.state.as_str(), s.done()));
            assert!(
                l.is_some_and(|l| l == (s.state.as_str(), s.done())) || !s.done() && l.is_none(),
                "{k:?}: {l:?} vs {}",
                s.state
            );
        }
        let queue = |v: &[Event]| v.iter().filter(|e| e.kind == "queue").count();
        assert_eq!(queue(&read(&p).unwrap()), queue(&before));
        // A new row continues the seq; deleting the checkpoint rebuilds the same states from the logs.
        append(&p, &Event::new(now, "a", Some(1), "stall", json!({}))).unwrap();
        assert_eq!(read(&p).unwrap().last().unwrap().seq, before.len() as u64 + 1);
        let with = states(&p).unwrap();
        herdsman::db::put(&p, "states", 0, &json!({})).unwrap(); // an unreadable checkpoint
        same(&states(&p).unwrap(), &with);
        checkpoint(&p).unwrap();
        same(&states(&p).unwrap(), &with);
    }
}

/// #560: `shared` (serve's read) is `read` at every point: rows appended, a rotation's archive, a new log.
#[test]
fn the_shared_read_follows_appends_and_rotations() {
    let d = tmpdir("shared");
    let p = d.join("events.jsonl");
    let now = 90 * 86_400;
    let seqs = |v: &[Event]| v.iter().map(|e| e.seq).collect::<Vec<_>>();
    let same = || assert_eq!(seqs(&herdsman::events::shared(&p).unwrap()), seqs(&read(&p).unwrap()));
    same(); // no database yet
    write(&p, &log(3, 400, 0));
    same();
    let first = herdsman::events::shared(&p).unwrap();
    append(&p, &Event::new(now - 10, "a", Some(1), "stall", json!({}))).unwrap();
    same();
    assert_eq!(herdsman::events::shared(&p).unwrap().len(), first.len() + 1);
    rotate(&p, 1, now).unwrap();
    assert!(read(&p).unwrap().len() < first.len(), "the rotation archived some");
    same();
    append(&p, &Event::new(now, "a", Some(1), "stall", json!({}))).unwrap();
    same();
}

/// #343 measure: `status` / `queue ls` fold a 50 MB log in under 20 ms from its checkpoint (release:
/// `cargo test --release --test compaction -- --ignored --nocapture`).
#[test]
#[ignore = "builds a 50 MB log; run in release"]
fn the_fold_of_a_50mb_log_takes_under_20ms() {
    let d = tmpdir("big");
    let p = d.join("events.jsonl");
    let pad = "x".repeat(400);
    let mut text = String::new();
    let mut seq = 0;
    while text.len() < 50 << 20 {
        for e in log(seq + 1, 1000, seq * 600) {
            seq += 1;
            let mut v = serde_json::to_value(Event { seq, ..e }).unwrap();
            v["pad"] = json!(pad);
            text += &(v.to_string() + "\n");
        }
    }
    std::fs::write(&p, text).unwrap();
    checkpoint(&p).unwrap();
    // #344: the rows after the checkpoint as live writers add them.
    log(7, 50, seq * 600).iter().for_each(|e| append(&p, e).unwrap());
    let t = std::time::Instant::now();
    let st = states(&p).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    eprintln!(
        "#343: fold of a {} MB log from its checkpoint: {ms:.2} ms",
        herdsman::db::live_bytes(&p).unwrap() >> 20
    );
    assert!(ms < 20.0, "{ms} ms");
    same(&st, &fold(&read(&p).unwrap()));
}
