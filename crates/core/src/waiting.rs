//! #46: what an idle lane waits on (drive | owner | ci | none), worked out from the log, git and the cached PR.
use crate::config::{Config, Fleet};
use crate::events::{age, fold, last_spawn, open_asks, Event, IssueState};
use crate::runner::{ok, Runner};
use crate::{gh, Ctx};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields = nullable))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Waiting {
    pub fleet: String,
    pub issue: u64,
    /// The lane this row is for; every lane of an issue gets one.
    pub lane: String,
    /// drive | owner | ci | none
    #[serde(deserialize_with = "who")]
    pub who: Who,
    pub what: String,
    pub since: u64,
    pub next_cmd: Option<String>,
}

/// #513: an in-flight issue held only by `max_parallel_spawns`.
pub const SLOT: &str = "waiting for a spawn slot";

/// #447: a `who` read back from the snapshot (an alias: serde borrows a field spelled `&str`).
type Who = &'static str;
fn who<'de, D: Deserializer<'de>>(d: D) -> Result<Who, D::Error> {
    let w = String::deserialize(d)?;
    Ok((["drive", "owner", "ci", "reviewer"].into_iter())
        .find(|x| *x == w)
        .unwrap_or("none"))
}

impl Waiting {
    /// The same row for another lane of the issue.
    fn on(&self, lane: &str) -> Waiting {
        Waiting {
            lane: lane.into(),
            ..self.clone()
        }
    }
    /// `idle · <what> (<age>)`
    pub fn label(&self, now: u64) -> String {
        format!("idle · {} ({})", self.what, age(now.saturating_sub(self.since)))
    }
    /// Page tone: owner red, drive amber past 5 min, an impl idle without pushing amber at once, the rest dim.
    pub fn tone(&self, now: u64) -> &'static str {
        match self.who {
            "owner" => "r",
            "drive" if now.saturating_sub(self.since) > 300 => "a",
            "none" if self.what.contains("idle without") => "a",
            _ => "m",
        }
    }
}

/// #217: an extra round is recorded for a lane that has not started yet (`extend --no-spawn` asks for none).
pub fn unspawned(events: &[Event], s: &IssueState) -> bool {
    let last = events
        .iter()
        .rev()
        .find(|e| e.fleet == s.fleet && e.issue == Some(s.issue));
    s.last_event == "extend-failed"
        || last.is_some_and(|e| e.kind == "round-extended" && e.data.get("spawn") == Some(&Value::Bool(true)))
}

/// #327: the fix lane died (`lane-dead`) or its supersede never spawned, with no new head: the fix is still due.
fn lost(events: &[Event], s: &IssueState, pushed: &str) -> bool {
    let build = |e: &&Event| e.is("spawn", &s.fleet, s.issue) && !crate::events::design(e);
    let mut since = events.iter().rev().take_while(|e| !build(e));
    let dead = |e: &Event| e.is("lane-dead", &s.fleet, s.issue) || e.is("supersede", &s.fleet, s.issue);
    (pushed.is_empty() || s.reviewed(pushed)) && since.any(dead)
}

/// #445: the `fix-done` (`done --no-change`) logged since the issue's last verdict: its head is reviewed again.
pub fn fix_done<'a>(events: &'a [Event], f: &str, n: u64) -> Option<&'a Event> {
    let mut since = events.iter().rev().take_while(|e| !e.is("verdict", f, n));
    since.find(|e| e.is("fix-done", f, n))
}

/// #444: a gone impl/fix lane's wait (`lane gone, no push: respawning impl`); drive respawns no more often than this.
pub const NO_PUSH: &str = ", no push: ";
/// #517: a lesson lane ended without a push or `done`: drive prompts it once, then closes the issue.
pub const LESSON_PROMPT: &str = "lesson lane ended without a push: prompting it once";
pub const LESSON_CLOSE: &str = "lesson lane ended without a push: closing as no repeated class";
const RESPAWN_SECS: u64 = 1800;
/// #511: a build lane's typed turn end, as drive acts on it (`turn`): nudged once, prompted to go on, or blocked.
pub const TURN_NUDGE: &str = "reported done, nothing new pushed: nudging it once";
pub const TURN_GO_ON: &str = "reported working, idle: prompting it to continue";
pub const TURN_BLOCKED: &str = "reported blocked: ";
/// #511: a nudged `done` with still no push is blocked after this long (or at its next `done`).
const NUDGE_SECS: u64 = 600;

/// #511: the build lane's latest `herdsman turn` since its spawn and any extra round the owner gave.
pub fn turn<'a>(events: &'a [Event], s: &IssueState) -> Option<&'a Event> {
    let sp = last_spawn(events, s.build.as_ref().and_then(|b| s.lanes.get(b))?)?;
    let extended = events.iter().rfind(|e| e.is("round-extended", &s.fleet, s.issue));
    let at = extended.map_or(sp.ts, |e| e.ts.max(sp.ts));
    let role = |e: &&Event| e.str("role") == s.build.as_deref() && e.ts >= at;
    events
        .iter()
        .rev()
        .filter(|e| e.is("turn-end", &s.fleet, s.issue))
        .find(role)
}

/// #511: an idle build lane's row from its reported turn `t` (no push at the head), `stall` secs its idle threshold:
/// (who, what) and the prompt drive sends (`turn-prompted`) once due.
fn turned(events: &[Event], t: &Event, lane: &str, now: u64, stall: u64) -> (Who, String) {
    let why = t.str("why").map_or(String::new(), |y| format!(" ({y})"));
    let state = t.str("state").unwrap_or_default();
    let told = |e: &&Event| e.kind == "turn-prompted" && e.str("agent") == Some(lane) && e.str("state") == Some(state);
    let since = |at: u64| move |e: &&Event| told(e) && e.ts >= at;
    let sp = last_spawn(events, lane).map_or(0, |e| e.ts);
    match state {
        "done" => match events.iter().rfind(since(sp)) {
            None => ("drive", TURN_NUDGE.into()),
            Some(p) if t.ts > p.ts || now >= p.ts + NUDGE_SECS => {
                ("drive", format!("{TURN_BLOCKED}done again with nothing pushed"))
            }
            Some(p) => (
                "none",
                format!("reported done, nothing pushed: nudged {} ago", age(now - p.ts)),
            ),
        },
        "blocked" => (
            "drive",
            format!("{TURN_BLOCKED}{}", t.str("why").unwrap_or("no reason given")),
        ),
        "need-input" => ("none", format!("reported need-input{why} with no open ask")),
        _ => match events.iter().rfind(since(t.ts)) {
            Some(p) => (
                "none",
                format!("reported working{why}: prompted to continue {} ago", age(now - p.ts)),
            ),
            None if now >= t.ts + stall => ("drive", TURN_GO_ON.into()),
            None => (
                "none",
                format!("reported working{why}: a continue prompt after {}", age(stall)),
            ),
        },
    }
}

/// #444: the `respawn`s since the issue's last push, verdict or needs-human: (count, the last one's time).
fn respawns(events: &[Event], fleet: &str, n: u64) -> (usize, u64) {
    let reset = ["pushed", "verdict", "needs-human", "round-extended", "repaired"];
    let mine = events.iter().rev().filter(|e| e.fleet == fleet && e.issue == Some(n));
    let at: Vec<u64> = mine
        .take_while(|e| !reset.contains(&e.kind.as_str()))
        .filter(|e| e.kind == "respawn")
        .map(|e| e.ts)
        .collect();
    (at.len(), at.first().copied().unwrap_or(0))
}

/// The 40-hex sha on verdict.md's `Reviewed head` line (#293).
pub fn reviewed_head(t: &str) -> Option<&str> {
    let l = t.lines().find(|l| l.contains("Reviewed head"))?;
    l.split(|c: char| !c.is_ascii_hexdigit()).find(|w| w.len() == 40)
}

/// #447: the log read once for every issue's wait: all of it, and its open asks.
pub struct Log<'a> {
    pub all: &'a [Event],
    pub asks: Vec<&'a Event>,
}

impl<'a> Log<'a> {
    pub fn new(all: &'a [Event]) -> Self {
        Log {
            all,
            asks: open_asks(all),
        }
    }
}

/// Pure: the issue's wait reason. `log` = the whole log, `events` = the issue's part of it (or all of it).
/// `head` = the lane worktree's (sha, commit time); `pr` = the (cached) PR view;
/// `rev` = the rev worktree's verdict.md (absolute path, first line) when written since the rev spawn.
/// #173: each next step is the one `fc`'s pipeline maps the outcome to.
/// #630: `waits`, plus what a pulled root lost (a `root-moved` since the issue's last push).
pub fn waiting_on(
    log: &Log,
    events: &[Event],
    s: &IssueState,
    head: Option<(&str, u64)>,
    pr: &Value,
    fc: &Fleet,
    rev: Option<(&str, &str)>,
) -> Waiting {
    let mut w = waits(log, events, s, head, pr, fc, rev);
    let mut since = events.iter().rev().take_while(|e| !e.is("pushed", &s.fleet, s.issue));
    let moved = since.find(|e| e.is("root-moved", &s.fleet, s.issue));
    if let Some(l) = moved.and_then(|e| e.str("lost")).filter(|l| !l.is_empty()) {
        w.what += &format!(" · root gone, lost {l}");
    }
    w
}

fn waits(
    log: &Log,
    events: &[Event],
    s: &IssueState,
    head: Option<(&str, u64)>,
    pr: &Value,
    fc: &Fleet,
    rev: Option<(&str, &str)>,
) -> Waiting {
    let (f, n, req, p) = (&s.fleet, s.issue, fc.checks(pr), fc.flow());
    // The role an outcome of `stage` goes to (a build for a fix, a check for a review).
    let to = |stage: &str, o: &str| {
        p.next(stage, o)
            .and_then(|t| p.role(t.0))
            .unwrap_or(p.start())
            .to_string()
    };
    let build = p.stage_of(s.build.as_deref().unwrap_or(p.start()));
    let passed = s.verdicts.last().filter(|v| v.1 == "PASS").map(|v| v.2.as_str());
    let then = passed.and_then(|c| p.next(c, "PASS")).map(|t| t.0);
    let next_check = then.filter(|t| p.get(t).is_some_and(|c| c.kind == "check"));
    let gate = then
        .filter(|t| p.get(t).is_some_and(|g| g.kind == "gate"))
        .unwrap_or(p.gate());
    let lane = s.lane.clone().unwrap_or_default();
    let w = |who, what: &str, since, next: Option<String>| Waiting {
        fleet: f.clone(),
        issue: n,
        lane: lane.clone(),
        who,
        what: what.into(),
        since,
        next_cmd: next,
    };
    let cmd = |verb: &str, tail: &str| Some(format!("herdsman {verb} {f} {n}{tail}"));
    let mine =
        |a: &&Event| a.fleet == *f && (a.issue == Some(n) || s.lanes.values().any(|l| a.str("agent") == Some(l)));
    if let Some(a) = log.asks.iter().copied().find(mine) {
        let id = a.str("id").unwrap_or("?");
        // #412: a design's ask names its answers.
        let next = crate::design::is_ask(a).then(|| format!("herdsman answer {f} {id} approve|revise|reject"));
        return w("owner", &format!("waiting on your answer ({id})"), a.ts, next);
    }
    // #350: a reverted landing's issue is in a fix round while its old PR stays merged.
    if s.state == "merged" || pr["state"] == "MERGED" && !s.state.starts_with("fix") {
        return w("none", "nothing to do (merged): clean me", s.last_ts, None);
    }
    if s.state == "closed" || pr["state"] == "CLOSED" {
        return w("none", "nothing to do (closed): clean me", s.last_ts, None);
    }
    let on = crate::events::held_on(events, s);
    if !on.is_empty() {
        return w(
            "drive",
            &format!(
                "held until {} merge{}",
                crate::queue::hashes(&on),
                ["s", ""][usize::from(on.len() > 1)]
            ),
            s.last_ts,
            None,
        );
        // #425: drive wakes it then
    }
    // #294: a lane held for a serial slot waits for drive's update round once the holder merges.
    if let Some(g) = fc
        .serial
        .iter()
        .filter(|g| crate::queue::held(events, f, n, &g.name))
        .min_by_key(|g| &g.name)
    {
        let st = crate::events::fold(log.all);
        let h = crate::queue::holders(log.all, &st, fc, f, &g.name);
        let who: Vec<_> = h.iter().map(|s| format!("#{}", s.issue)).collect();
        let by = if who.is_empty() {
            "for a slot".into()
        } else {
            format!("held by {}", who.join(" "))
        };
        let what = format!("waits: {} {by} ({}/{})", g.name, h.len(), g.max);
        return w("drive", &what, s.last_ts, None);
    }
    // #104: an impl/fix/review lane whose account capped since its spawn respawns on a free one (`kill` pushes `wip/`).
    let active = matches!(s.state.split('(').next(), Some("impl" | "fix" | "review"));
    let spawned = last_spawn(events, &lane).filter(|_| active);
    let since_spawn = |e: &&Event| spawned.is_some_and(|sp| e.ts >= sp.ts && e.str("account") == sp.str("account"));
    let rest = |e: &&Event| e.is("account-capped", f, n) || e.is("agent-unavailable", f, n); // #302
    if let Some(c) = events.iter().filter(since_spawn).find(rest) {
        let role = spawned.and_then(|sp| sp.str("role")).unwrap_or_default();
        let what = "account capped, waiting for drive to respawn on a free one";
        return w("drive", what, c.ts, cmd("spawn", &format!(" {role} --supersede")));
    }
    let pushed = pr["headRefOid"].as_str().unwrap_or_default();
    // #129: an auto-update moves the head without new work, so a review of the head it updated still stands.
    let mut base = pushed;
    let update = |to: &str| {
        events
            .iter()
            .find(|e| e.is("auto-updated", f, n) && e.str("to") == Some(to))
    };
    while let Some(e) = update(base) {
        base = e.str("from").unwrap_or_default();
    }
    let handed = |h: &str| events.iter().any(|e| e.is("pushed", f, n) && e.str("sha") == Some(h));
    // #102: a PASSed PR conflicting with / behind integration gets an update lane; its pushed head is reviewed first.
    let checks = gh::required(&pr["statusCheckRollup"], req);
    let how = match (pr["mergeable"].as_str(), pr["mergeStateStatus"].as_str()) {
        (Some("CONFLICTING"), _) => "conflicts with",
        (_, Some("BEHIND")) => "behind",
        _ => "",
    };
    // #148: a PASSed item blocked by CI, mergeability or host load is retried as ready on every pass.
    let block = events
        .iter()
        .rev()
        .find(|e| e.is("blocked", f, n))
        .and_then(|e| e.str("code"));
    let retry = s.state == "blocked" && s.verdicts.last().is_some_and(|v| v.1 == "PASS");
    // #599: a PASS at the head that goes on to another check waits on that check, whatever the merge refused.
    let here = s
        .verdicts
        .last()
        .is_some_and(|v| v.0.eq_ignore_ascii_case(base) || v.0.eq_ignore_ascii_case(pushed));
    let st = match block {
        _ if retry && here && next_check.is_some() => "ready",
        Some("CI_NOT_GREEN" | "NOT_MERGEABLE" | "HOST_BUSY") if retry => "ready",
        // #165: a head with no verdict of its own (a non-pure merge) gets a review round, not a block.
        Some("STALE_VERDICT") if s.state == "blocked" => "stale",
        // #214: blocked with an open PR is no resting state: a conflict is updated, anything else reviewed again.
        _ if s.state == "blocked" && pr["state"] == "OPEN" => {
            if how.is_empty() {
                "stale"
            } else {
                "ready"
            }
        }
        _ => s.state.split('(').next().unwrap_or_default(),
    };
    let at_head = |h: &str| h.eq_ignore_ascii_case(base) || h.eq_ignore_ascii_case(pushed);
    match st {
        // #173: a PASS that goes on to another check: that check reviews the same head.
        "ready" if next_check.is_some() => {
            let c = next_check.unwrap_or_default();
            let what = format!("{} PASS, waiting for drive to spawn {c}", passed.unwrap_or_default());
            w(
                "drive",
                &what,
                s.last_ts,
                cmd("spawn", &format!(" {}", p.role(c).unwrap_or(c))),
            )
        }
        "ready" if !how.is_empty() => {
            let base = pr["baseRefName"].as_str().unwrap_or("?");
            let what = format!("PASS · {how} {base} · catching up next pass (≤ 5 min)");
            let b = to(gate, "conflict");
            w(
                "drive",
                &what,
                s.last_ts,
                cmd("spawn", &format!(" {b} --supersede --update")),
            )
        }
        // #212: CI read live by the fast step, which merges a green PASS within a minute.
        "ready" if gh::ci(&checks) == "green" => w(
            "drive",
            "PASS · CI green · merging within 1 min",
            s.last_ts,
            cmd("merge", ""),
        ),
        "ready" => {
            let all = checks.as_array().map(Vec::as_slice).unwrap_or_default();
            let n = all.iter().filter(|c| gh::check_state(c) == "green").count();
            let what = match all.iter().find(|c| gh::check_state(c) == "failed") {
                Some(c) => format!("PASS · CI red: {}", gh::check_name(c)),
                None => format!("PASS · CI running {n}/{}", all.len()),
            };
            w("ci", &what, s.last_ts, None)
        }
        "stale" => w(
            "drive",
            "no verdict for the PR head: review again",
            s.last_ts,
            cmd("spawn", &format!(" {} --supersede", to(build, "push"))),
        ),
        // The exact command, or none (no nudge) while the verdict word or the head sha is unknown.
        "review" => match rev {
            // #92: a verdict is for the head the rev lane was spawned at; a moved PR head gets a fresh reviewer.
            Some(_) if head.is_some_and(|h| !pushed.is_empty() && !at_head(h.0)) => {
                let what = format!(
                    "review was for {:.7}, head moved to {pushed:.7} (not a clean merge): review again",
                    head.unwrap_or_default().0
                );
                let c = s.check.as_deref().unwrap_or("rev");
                w("drive", &what, s.last_ts, cmd("spawn", &format!(" {c} --supersede")))
            }
            Some((path, v)) => {
                let at = head.is_some_and(|h| at_head(h.0));
                let next = (matches!(v, "PASS" | "FAIL") && at).then(|| {
                    format!(
                        "herdsman verdict {f} {n} {v} --sha {} --file {path}",
                        head.unwrap_or_default().0
                    )
                });
                // #607: a PASS refused NO_REPRO at this head says why, never "recording".
                let refused = (events.iter().rev())
                    .filter(|e| e.fleet == *f && e.issue == Some(n))
                    .take_while(|e| !(e.kind == "spawn" && crate::events::checks(e)))
                    .find(|e| e.kind == "repro-refused" && head.is_some_and(|h| e.str("sha") == Some(h.0)));
                let what = match (&next, refused) {
                    (Some(_), Some(e)) => format!("verdict refused NO_REPRO: {}", e.str("why").unwrap_or_default()),
                    (Some(_), None) => "review done · recording within 1 min".into(),
                    (None, _) => "verdict written, waiting for drive to record it".into(),
                };
                w("drive", &what, s.last_ts, next)
            }
            None => w("none", "rev idle without a verdict (check the pane)", s.last_ts, None),
        },
        // #217: an extra round whose lane never started (a failed or deferred spawn) is due the same way.
        // #500: so is one a `repaired` false park put back.
        "fix"
            if matches!(s.last_event.as_str(), "verdict" | "repaired")
                || unspawned(events, s)
                || lost(events, s, pushed) =>
        {
            // The FAIL's stage: a check's FAIL, or a gate's CI red (logs before #173 name none: `rev`).
            let v = events.iter().rev().find(|e| e.is("verdict", f, n));
            let by = v.and_then(|e| e.str("stage")).unwrap_or("rev");
            let o = if p.get(by).is_some_and(|g| g.kind == "gate") {
                "ci_red"
            } else {
                "FAIL"
            };
            let what = "review FAIL, waiting for drive to start fix round";
            w(
                "drive",
                what,
                s.last_ts,
                cmd("spawn", &format!(" {} --supersede", to(by, o))),
            )
        }
        // #182: a head the PR handed off (`pushed`, logged by drive) is due too, whatever the worktree holds.
        // #319: so is the FAILed head again after a fix round that changed nothing (`fix-no-change`).
        "impl" | "fix"
            if !pushed.is_empty()
                && (!s.reviewed(pushed) || s.last_event == "fix-no-change" || fix_done(events, f, n).is_some())
                && (handed(pushed) || head.is_none_or(|h| h.0 == pushed)) =>
        {
            let how = match s.rounds {
                _ if s.reviewed(pushed) => "no change at ",
                0 => "pushed ",
                _ => "pushed fix ",
            };
            let mut what = format!(
                "{how}{}, waiting for drive to spawn review",
                &pushed[..7.min(pushed.len())]
            );
            // #212: drive's last try at this review was refused: say why in plain words.
            let tried = events.iter().rfind(|e| e.is("driven", f, n));
            let refused =
                |e: &&Event| e.str("step") == Some("review") && !matches!(e.str("code"), None | Some("EXIT" | "ERROR"));
            if let Some(e) = tried.filter(refused) {
                let why = crate::drive::words(e.str("code"), "review", e.str("result").unwrap_or_default());
                what = if why.contains(crate::drive::OLD) {
                    why
                } else {
                    format!("waiting for a reviewer slot: {why}")
                };
            }
            w(
                "drive",
                &what,
                head.map_or(s.last_ts, |h| h.1.max(s.last_ts)),
                cmd("spawn", &format!(" {}", to(build, "push"))),
            )
        }
        "impl" | "fix" if s.last_event == "lane-killed" => w(
            "drive",
            "lane killed, waiting for drive to respawn impl",
            s.last_ts,
            cmd("spawn", &format!(" {}", p.role(build).unwrap_or(p.start()))),
        ),
        "impl" | "fix" => w("none", "idle without pushing (check the pane)", s.last_ts, None),
        "design" => w(
            "none",
            "design lane idle without a pushed design (check the pane)",
            s.last_ts,
            cmd("spawn", " design --supersede"),
        ),
        st => w("none", &format!("nothing to do ({st})"), s.last_ts, None),
    }
}

/// One row per lane herdr does not show `working`: the latest lane's reason (`s.lane == w.lane`), shared by the
/// issue's older lanes, which wait on the latest one while it works. `live` refreshes the PR read, else the cache only.
pub fn scan(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    events: &[Event],
    agents: &[Value],
    live: bool,
) -> Vec<(IssueState, Waiting)> {
    let (mut out, log) = (vec![], Log::new(events));
    // #447: each issue's events, in one pass: a wait reads its own, never the whole log again.
    let mut of: std::collections::HashMap<_, Vec<Event>> = std::collections::HashMap::new();
    for e in events {
        if let Some(n) = e.issue {
            of.entry((e.fleet.as_str(), n)).or_default().push(e.clone());
        }
    }
    let busy = |l: &str| agents.iter().any(|a| a["name"] == l && a["agent_status"] == "working");
    for s in fold(events).into_values().filter(|s| cfg.fleet.contains_key(&s.fleet)) {
        // #43: after a `lane-killed`, the issue's surviving lane carries its reason (a killed rev: spawn rev again).
        let Some(lane) = s.lane.clone().or_else(|| {
            s.build
                .as_ref()
                .and_then(|b| s.lanes.get(b))
                .or(s.check_lane())
                .cloned()
        }) else {
            continue;
        };
        let s = IssueState {
            lane: Some(lane.clone()),
            ..s
        };
        let mine = of.get(&(s.fleet.as_str(), s.issue)).map_or(&[][..], Vec::as_slice);
        let (f, branch) = (&cfg.fleet[&s.fleet], crate::gate::branch_of(Some(&s), s.issue));
        // #169: no PR yet: the issue branch's latest run backs the `checks` link (the tick reads it, never a request).
        if live && !matches!(s.state.as_str(), "merged" | "closed") && gh::cached_pr(ctx, &s.fleet, s.issue).is_null() {
            gh::run(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now);
        }
        if busy(&lane) {
            let who = if s.check_lane() == Some(&lane) {
                "reviewer"
            } else {
                "none"
            };
            let w = Waiting {
                fleet: s.fleet.clone(),
                issue: s.issue,
                lane: lane.clone(),
                who,
                what: format!("waiting on {lane} (working)"),
                since: s.last_ts,
                next_cmd: None,
            };
            out.extend(s.lanes.values().filter(|l| !busy(l)).map(|l| (s.clone(), w.on(l))));
            continue;
        }
        let mut pr = match (s.state.as_str(), live) {
            ("merged" | "closed", _) => Value::Null,
            (_, true) => gh::pr(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now)
                .unwrap_or_else(|_| gh::cached_pr(ctx, &s.fleet, s.issue)),
            _ => gh::cached_pr(ctx, &s.fleet, s.issue),
        };
        crate::land::mark(ctx, None, f, (&s.fleet, s.issue), events, &mut pr);
        let sp = last_spawn(mine, &lane);
        let wt = sp.and_then(|e| e.str("worktree")).filter(|_| !s.done());
        let on = crate::hosts::On::of(r, sp);
        let spawn = s.check_lane().and_then(|l| last_spawn(mine, l));
        // #337: the worktree head only where it decides something: an impl/fix not seen pushed, a review with no sha.
        let pushed = pr["headRefOid"].as_str().unwrap_or_default();
        let seen = mine
            .iter()
            .any(|e| e.is("pushed", &s.fleet, s.issue) && e.str("sha") == Some(pushed));
        let need = match s.state.split('(').next() {
            Some("impl" | "fix") => pushed.is_empty() || !seen,
            Some("review") => spawn.and_then(|e| e.str("sha")).is_none(),
            _ => false,
        };
        let git = (wt.filter(|_| need)).and_then(|wt| ok(&on, "git", &["-C", wt, "log", "-1", "--format=%H %ct"]).ok());
        let head = git
            .as_deref()
            .and_then(|g| g.trim().split_once(' '))
            .and_then(|(h, t)| Some((h, t.parse().ok()?)));
        let path = spawn
            .and_then(|e| e.str("worktree"))
            .map(|wt| format!("{wt}/verdict.md"));
        // #376: an adopted orphan's verdict counts from its `spawn-started` (none logged: any).
        let since = spawn.map_or(0, |e| e.data.get("since").and_then(Value::as_u64).unwrap_or(e.ts));
        let fresh = |p: &String| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t > std::time::UNIX_EPOCH + std::time::Duration::from_secs(since))
        };
        // #157: on another host the rev spawn removed any older verdict.md, so one there is this round's.
        let there = crate::hosts::On::of(r, spawn);
        // #337: a verdict recorded since the rev spawn stands for its file, which is then never read again.
        let kept = spawn.and_then(|sp| {
            mine.iter()
                .rfind(|e| e.is("verdict", &s.fleet, s.issue) && e.ts >= sp.ts)
        });
        let text = path
            .as_ref()
            .filter(|_| kept.is_none())
            .and_then(|p| match there.machine {
                Some(_) => there.read(std::path::Path::new(p)),
                None => std::fs::read_to_string(p).ok().filter(|_| fresh(p)),
            });
        let word = text
            .as_deref()
            .map(|v| v.lines().next().unwrap_or_default().trim().to_string())
            // #228: a form-only FAIL is recorded as `FAIL`; `verdict` reads the tag from the file.
            .map(|v| if v == "FAIL (form-only)" { "FAIL".into() } else { v })
            .or_else(|| kept.and_then(|e| e.str("verdict")).map(String::from));
        let named = text
            .as_deref()
            .and_then(reviewed_head)
            .or_else(|| kept.and_then(|e| e.str("sha")));
        // In review, `head` is the sha verdict.md names (#293), or the rev spawn head (#92; else its worktree's).
        let at = spawn
            .filter(|_| s.check_lane() == Some(&lane))
            .and_then(|e| Some((named.or(e.str("sha"))?, e.ts)));
        // #207: a PR head that merges the reviewed one with integration and adds nothing carries its verdict, as a
        // logged auto-update does: read as one, the verdict is recorded at the reviewed sha.
        let updated = |e: &Event| e.is("auto-updated", &s.fleet, s.issue) && e.str("to") == Some(pushed);
        let carry = at.filter(|(x, _)| {
            word.is_some()
                && !pushed.is_empty()
                && !x.eq_ignore_ascii_case(pushed)
                && !mine.iter().any(updated)
                && crate::overlap::carries(r, f, &s, x, pushed)
        });
        let ev = carry.map(|(x, _)| {
            let d = serde_json::json!({"from": x, "to": pushed});
            [mine, &[Event::new(ctx.now, &s.fleet, Some(s.issue), "auto-updated", d)]].concat()
        });
        let ev = ev.as_deref().unwrap_or(mine);
        let mut w = waiting_on(&log, ev, &s, at.or(head), &pr, f, path.as_deref().zip(word.as_deref()));
        // #285: behind integration under a non-strict rule is no catch-up: the PR merges as it is.
        if w.what.starts_with("PASS · behind ") && !gh::update_ready(ctx, None, f, &s.fleet) {
            let green = gh::ci(&gh::required(&pr["statusCheckRollup"], f.checks(&pr))) == "green";
            let merge = format!("herdsman merge {} {}", s.fleet, s.issue);
            (w.who, w.next_cmd) = if green { ("drive", Some(merge)) } else { ("ci", None) };
            w.what = format!("PASS · behind {} (not required) · merging when CI green", f.integration);
        }
        // #249: a review on another host names it.
        if let Some(h) = spawn.filter(|_| there.machine.is_some()).and_then(|e| e.str("host")) {
            w.what = w.what.replace("review done", &format!("review done on {h}"));
        }
        // #214: a reviewer whose lane exited (no herdr agent) without a verdict gets a new review round.
        if w.what.starts_with("rev idle without") && !agents.iter().any(|a| a["name"] == lane.as_str()) {
            let c = s.check.as_deref().unwrap_or("rev");
            w.who = "drive";
            w.what = "reviewer exited without a verdict: reviewing again".into();
            w.next_cmd = Some(format!("herdsman spawn {} {} {c} --supersede", s.fleet, s.issue));
        }
        // #444: an impl/fix lane gone from herdr, or whose prompt never landed, with no push: drive respawns it
        // `--supersede` at most every 30 min; after two respawns in a row without a push it needs a human.
        // #500: the build role's own lane, never the check lane that ran last.
        let built = s.build.as_ref().and_then(|b| s.lanes.get(b)).unwrap_or(&lane).clone();
        let sp = last_spawn(mine, &built);
        let unconfirmed = sp.and_then(|e| e.data.get("confirmed")) == Some(&Value::Bool(false));
        let gone = !agents.iter().any(|a| a["name"] == built.as_str());
        // A lane started this pass, or a spawn still starting, is not gone yet.
        let settled = || {
            sp.is_some_and(|e| ctx.now >= e.ts + 120)
                && !crate::drive::pending(ctx)
                    .iter()
                    .any(|p| p.fleet == s.fleet && p.issue == Some(s.issue))
        };
        // #517: a lesson lane idle with no push and no `done` is prompted once (live), then closed as "no repeated
        // class" once that goes unanswered 30 min (or its lane is gone): never #444's respawn or needs-human.
        let idle = w.what.starts_with("idle without pushing");
        let waiting_turn = turn(mine, &s);
        if idle && s.build.as_deref() == Some("lesson") && settled() {
            let asked = mine
                .iter()
                .rfind(|e| e.kind == "lesson-prompted" && e.str("agent") == Some(built.as_str()));
            (w.who, w.lane, w.next_cmd) = ("drive", built, None);
            w.what = match asked {
                None if !gone && !unconfirmed => LESSON_PROMPT.into(),
                Some(e) if !gone && ctx.now < e.ts + RESPAWN_SECS => {
                    format!(
                        "lesson lane prompted {} ago, closing it after 30 min",
                        age(ctx.now - e.ts)
                    )
                }
                _ => LESSON_CLOSE.into(),
            };
        } else if let Some(t) = waiting_turn.filter(|_| idle && !gone && !unconfirmed) {
            // #511: a live build lane's reported turn decides, not a guess.
            (w.who, w.what) = turned(mine, t, &built, ctx.now, cfg.limits.stall_minutes * 60);
            (w.lane, w.since) = (built, t.ts);
        } else if idle && (gone || unconfirmed) && settled() {
            let (n, last) = respawns(mine, &s.fleet, s.issue);
            let role = s.build.as_deref().unwrap_or(f.flow().start());
            let why = if gone { "lane gone" } else { "prompt never landed" };
            (w.who, w.lane) = ("drive", built);
            w.next_cmd = Some(format!("herdsman spawn {} {} {role} --supersede", s.fleet, s.issue));
            w.what = if n >= 2 {
                w.next_cmd = None;
                format!("lane keeps ending{NO_PUSH}needs you")
            } else if n > 0 && ctx.now < last + RESPAWN_SECS {
                format!(
                    "{why}{NO_PUSH}respawned {} ago, again after 30 min",
                    age(ctx.now - last)
                )
            } else {
                format!("{why}{NO_PUSH}respawning {role}")
            };
        }
        // #478: an active lane on a host the last look could not reach is not gone: nothing respawns it.
        if let Some(h) = crate::hosts::offline(ctx, sp).filter(|_| s.active_lane().is_some()) {
            (w.who, w.what, w.next_cmd) = ("none", format!("host offline: {h}"), None);
        }
        // #592: a live remote lane's rules, frozen at spawn, that fleet.toml now resolves otherwise: a respawn takes them.
        let sp = s.active_lane().and_then(|(_, a)| last_spawn(mine, a));
        if sp.is_some_and(|e| crate::hook::rules_stale(cfg, &s.fleet, e)) {
            w.what += " · rules stale";
        }
        let gone = || gh::missing(ctx, live.then_some(r), cfg, (&s.fleet, s.issue), mine, &pr);
        let m = w.what.starts_with("PASS · CI running").then(gone).flatten();
        w.what = m.map_or(w.what, |m| format!("PASS · {m}"));
        // #150: a PASS whose PR lacks the merge label never gets its full CI run.
        if let Some(l) = f
            .merge_label
            .as_deref()
            .filter(|l| w.who == "ci" && !gh::has_label(&pr, l))
        {
            w.what = format!("PASS, PR lacks label `{l}`, so the full CI has not run");
        }
        w.what = w.what.replace("for drive", "for drive (next pass ≤ 5 min)");
        // #513: a due spawn drive's last try refused for want of a slot waits for one (the lane row says so, not parked).
        let tried = mine.iter().rfind(|e| matches!(e.kind.as_str(), "driven" | "spawn"));
        let spawn = w.next_cmd.as_deref().is_some_and(|c| c.starts_with("herdsman spawn "));
        if spawn && tried.is_some_and(|e| e.str("code") == Some("SPAWN_PENDING")) {
            w.what = SLOT.into();
        }
        // #212: a CI/merge line from a PR read older than 2 min (no fast step since) says when it was taken.
        let at = gh::cached_at(ctx, &s.fleet, s.issue).filter(|t| ctx.now > t + 120);
        if let Some(t) = at.filter(|_| w.what.starts_with("PASS · ")) {
            w.what += &format!(" · checked {} ago", age(ctx.now - t));
        }
        // #83: a paused fleet's lane with no pane and a clean, pushed worktree is parked until `resume`.
        let safe = || wt.is_some_and(|wt| crate::spawn::dirty(&on, std::path::Path::new(wt)).is_none());
        if f.paused
            && !agents
                .iter()
                .any(|a| a["name"] == lane.as_str() && a["agent_status"] != "parked")
            && safe()
        {
            let pr = pr["number"]
                .as_u64()
                .map_or(String::new(), |p| format!(" (resume to review PR #{p})"));
            (w.who, w.what, w.next_cmd) = ("none", format!("parked: fleet paused{pr}"), None);
        }
        // #500: the row's lane is the one it waits on (a respawn's: the build lane).
        let s = IssueState {
            lane: Some(w.lane.clone()),
            ..s
        };
        out.extend(
            s.lanes
                .values()
                .filter(|l| **l != w.lane && !busy(l))
                .map(|l| (s.clone(), w.on(l))),
        );
        out.push((s, w));
    }
    out
}

/// #493: the issues `check` guards: impl, fix, review or ready, and each fleet's next startable queued one.
pub fn guarded(events: &[Event], cfg: &Config) -> Vec<IssueState> {
    let st = fold(events);
    let busy = |s: &&IssueState| cfg.fleet.contains_key(&s.fleet) && crate::queue::in_flight(&s.state);
    let mut out: Vec<IssueState> = st.values().filter(busy).cloned().collect();
    for f in cfg.fleet.keys() {
        let Some(n) = crate::queue::startable(events, f).first().and_then(|n| n.parse().ok()) else {
            continue;
        };
        let q = events
            .iter()
            .rfind(|e| e.kind == "queue" && e.fleet == *f)
            .map_or(0, |e| e.ts);
        let s = st.get(&(f.clone(), n)).cloned().unwrap_or_else(|| IssueState {
            fleet: f.clone(),
            issue: n,
            state: "queued".into(),
            ..Default::default()
        });
        out.push(IssueState {
            last_ts: s.last_ts.max(q),
            ..s
        });
    }
    out
}

/// #493: a refusal that stops drive's starts for every fleet (`CAP`: its own); the queue waits on it.
const HOST_WAIT: [&str; 5] = ["HOST_BUSY", "DISK_LOW", "DISK_BUDGET", "MEM_LOW", "ALL_CAPPED"];
pub const ORPHAN_SECS: u64 = 1800;

/// #493: since when nothing has driven guarded issue `s`, once that is 30 min: no working lane, no drive action on it
/// (a due step, or a refused one drive retries) and no named wait (an open ask, a hold, a paused fleet, CI after PASS).
pub fn undriven(log: &Log, s: &IssueState, f: &Fleet, pr: &Value, agents: &[Value], now: u64) -> Option<u64> {
    let working = |l: &String| {
        agents
            .iter()
            .any(|a| a["name"] == l.as_str() && a["agent_status"] == "working")
    };
    let asked = |a: &&Event| {
        a.fleet == s.fleet && (a.issue == Some(s.issue) || s.lanes.values().any(|l| a.str("agent") == Some(l)))
    };
    let label = f.merge_label.as_deref().is_none_or(|l| gh::has_label(pr, l));
    let ci = s.state == "ready" && label && gh::ci(&gh::required(&pr["statusCheckRollup"], f.checks(pr))) == "pending";
    let held = !crate::events::held_on(log.all, s).is_empty();
    if f.paused || ci || held || s.lanes.values().any(working) || log.asks.iter().any(asked) {
        return None;
    }
    let (mut since, mut seen) = (s.last_ts, false);
    for e in log.all.iter().filter(|e| e.kind == "drive-pass") {
        seen |= e.fleet == s.fleet;
        for a in e.data.get("actions").and_then(Value::as_array).into_iter().flatten() {
            let code = a["code"].as_str().unwrap_or_default();
            let wait = HOST_WAIT.contains(&code) || code == "CAP" && e.fleet == s.fleet;
            // #534: a LANE_LIVE loop on a finished lane drives nothing.
            let old = a["words"].as_str().is_some_and(|w| w.contains(crate::drive::OLD));
            if e.fleet == s.fleet && a["issue"] == s.issue && !old || s.state == "queued" && wait {
                since = since.max(e.ts);
            }
        }
    }
    // A fleet drive never passed is driven by hand.
    (seen && now >= since + ORPHAN_SECS).then_some(since)
}
