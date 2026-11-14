use crate::config::{Config, Fleet};
use crate::events::{age, fold, last_spawn, open_asks, Event, IssueState};
use crate::promise::Say;
pub use crate::promise::SLOT;
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
    /// #854: the promise its line makes, when it makes one.
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub promise: Option<crate::promise::Kind>,
}

type Who = &'static str;
fn who<'de, D: Deserializer<'de>>(d: D) -> Result<Who, D::Error> {
    let w = String::deserialize(d)?;
    Ok((["drive", "owner", "ci", "reviewer"].into_iter())
        .find(|x| *x == w)
        .unwrap_or("none"))
}

impl Waiting {
    /// The same row for another lane of the issue.
    pub fn say(&mut self, s: &Say) {
        (self.what, self.promise) = (s.line(), Some(s.kind()));
    }
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

/// #217: an extra round is recorded for a lane that has not started yet (`extend --no-spawn` asks for none); #778: its pending action, until a hard refusal ends it (logs before #778: the round's own event, last).
pub fn unspawned(events: &[Event], s: &IssueState) -> bool {
    let last = events
        .iter()
        .rev()
        .find(|e| e.fleet == s.fleet && e.issue == Some(s.issue));
    crate::pending::extending(events, &s.fleet, s.issue)
        || last.is_some_and(|e| e.kind == "round-extended" && e.data.get("spawn") == Some(&Value::Bool(true)))
}

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
const DRAFT: &str = "PR is a draft";
/// #511: a build lane's typed turn end, as drive acts on it (`turn`): nudged once, prompted to go on, or blocked.
pub const TURN_NUDGE: &str = "reported done, nothing new pushed: nudging it once";
pub const TURN_GO_ON: &str = "reported working, idle: prompting it to continue";
pub const TURN_BLOCKED: &str = "reported blocked: ";
const NUDGE_SECS: u64 = 600;

fn failed_start(events: &[Event], s: &IssueState) -> bool {
    let Some(sp) = s.lane.as_deref().and_then(|l| last_spawn(events, l)) else {
        return false;
    };
    let hit = |e: &Event| e.kind == "lane-failed-start" && e.ts >= sp.ts && e.str("agent") == sp.str("agent");
    events.iter().any(hit)
}

/// #511: the build lane's latest `drover turn` since its spawn and any extra round the owner gave.
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

pub(crate) fn respawns(events: &[Event], fleet: &str, n: u64) -> (usize, u64) {
    let reset = [
        "pushed",
        "verdict",
        "needs-human",
        "round-extended",
        "repaired",
        "runtime-cleared",
    ];
    let mine = events.iter().rev().filter(|e| e.fleet == fleet && e.issue == Some(n));
    let at: Vec<&Event> = mine
        .take_while(|e| !reset.contains(&e.kind.as_str()))
        .filter(|e| e.kind == "respawn")
        .collect();
    let resumed = |e: &&&Event| e.str("why").is_some_and(resumed);
    let n = at.iter().filter(|e| !resumed(e)).count();
    (n, at.first().map_or(0, |e| e.ts))
}

/// #662: a gone lane's wait (or `respawn` why) that names a resumable exit cause.
pub fn resumed(why: &str) -> bool {
    crate::shed::RESUMABLE.iter().any(|c| why.contains(&format!("({c})")))
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
    /// #639: each queued issue's blockers not merged yet (#667: another repo's too), as `#11, cooee#87`.
    pub blocked: Vec<(&'a str, u64, String)>,
}

impl<'a> Log<'a> {
    pub fn new(all: &'a [Event]) -> Self {
        let st = fold(all);
        let fleets: std::collections::BTreeSet<&str> = (all.iter().filter(|e| e.kind == "queue"))
            .map(|e| e.fleet.as_str())
            .collect();
        let blocked = (fleets.into_iter())
            .flat_map(|f| crate::queue::view(all, &st, f).into_iter().map(move |i| (f, i)))
            .map(|(f, i)| (f, i.issue, crate::queue::labels(&i)))
            .filter(|i| !i.2.is_empty())
            .collect();
        Log {
            all,
            asks: open_asks(all),
            blocked,
        }
    }

    /// #639: `fleet` #`n`'s open queue blockers: no lane of any kind starts while one is left.
    pub fn blockers(&self, fleet: &str, n: u64) -> &str {
        let i = self.blocked.iter().find(|i| i.0 == fleet && i.1 == n);
        i.map_or("", |i| &i.2)
    }
}

/// Pure: the issue's wait reason. `log` = the whole log, `events` = the issue's part of it (or all of it). `head` = the lane worktree's (sha, commit time); `pr` = the (cached) PR view; `rev` = the rev worktree's verdict.md (absolute path, first line) when written since the rev spawn. #173: each next step is the one `fc`'s pipeline maps the outcome to. #630: `waits`, plus what a pulled root lost (a `root-moved` since the issue's last push); #790: the lane moved on.
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
        w.what += &format!(" · moved off a pulled root, lost {l}");
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
        promise: None,
    };
    let said = |s: Say, since, next: Option<String>| {
        let mut x = w("drive", "", since, next);
        x.say(&s);
        x
    };
    let drive = |reason: &str, step: &str| Say::Drive {
        reason: reason.into(),
        step: step.into(),
    };
    let cmd = |verb: &str, tail: &str| Some(format!("drover {verb} {f} {n}{tail}"));
    let mine =
        |a: &&Event| a.fleet == *f && (a.issue == Some(n) || s.lanes.values().any(|l| a.str("agent") == Some(l)));
    if let Some(a) = log.asks.iter().copied().find(mine) {
        let id = a.str("id").unwrap_or("?");
        let next = crate::design::is_ask(a).then(|| format!("drover answer {f} {id} approve|revise|reject"));
        return w("owner", &format!("waiting on your answer ({id})"), a.ts, next);
    }
    if s.state == "merged" || pr["state"] == "MERGED" && !s.state.starts_with("fix") {
        return w("none", "nothing to do (merged): clean me", s.last_ts, None);
    }
    if s.state == "closed" || pr["state"] == "CLOSED" {
        return w("none", "nothing to do (closed): clean me", s.last_ts, None);
    }
    let mut on = crate::events::held_on(events, s);
    on.retain(|b| !log.all.iter().any(|e| e.is("merged", f, *b)));
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
    }
    let on = log.blockers(f, n);
    if !on.is_empty() {
        let what = format!("blocked by {on}");
        return w("drive", &what, s.last_ts, None);
    }
    if let Some(g) = crate::queue::serials(log.all, fc, f)
        .into_iter()
        .filter(|g| crate::queue::held(events, f, n, &g.name))
        .min_by(|a, b| a.name.cmp(&b.name))
    {
        let st = crate::events::fold(log.all);
        let h = crate::queue::holders(log.all, &st, fc, f, &g.name);
        let who: Vec<_> = h.iter().map(|s| format!("#{}", s.issue)).collect();
        let by = if who.is_empty() {
            "for a slot".into()
        } else {
            format!("held by {}", who.join(" "))
        };
        let what = format!("serial: {} {by} ({}/{})", g.name, h.len(), g.max);
        return w("drive", &what, s.last_ts, None);
    }
    let active = matches!(s.state.split('(').next(), Some("impl" | "fix" | "review"));
    let spawned = last_spawn(events, &lane).filter(|_| active);
    let since_spawn = |e: &&Event| spawned.is_some_and(|sp| e.ts >= sp.ts && e.str("account") == sp.str("account"));
    let rest = |e: &&Event| crate::access::rests(&e.kind) && e.is(&e.kind, f, n);
    if let Some(c) = events.iter().filter(since_spawn).find(rest) {
        let role = spawned.and_then(|sp| sp.str("role")).unwrap_or_default();
        let what = drive("account capped", "respawn on a free one");
        return said(what, c.ts, cmd("spawn", &format!(" {role} --supersede")));
    }
    let pushed = pr["headRefOid"].as_str().unwrap_or_default();
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
    let checks = gh::required(&pr["statusCheckRollup"], req);
    let how = match (pr["mergeable"].as_str(), pr["mergeStateStatus"].as_str()) {
        (Some("CONFLICTING"), _) => "conflicts with",
        (_, Some("BEHIND")) => "behind",
        _ => "",
    };
    let blocked = events.iter().rev().find(|e| e.is("blocked", f, n));
    let block = blocked.and_then(|e| e.str("code"));
    let retry = s.state == "blocked" && s.verdicts.last().is_some_and(|v| v.1 == "PASS");
    let here = s
        .verdicts
        .last()
        .is_some_and(|v| v.0.eq_ignore_ascii_case(base) || v.0.eq_ignore_ascii_case(pushed));
    let done = passed.is_some() && crate::gate::verified(fc, Some(s), pushed, false).is_ok();
    let mut outcomes = s
        .verdicts
        .iter()
        .filter(|v| v.0.eq_ignore_ascii_case(pushed))
        .map(|v| &v.2);
    let more = outcomes.next().is_some_and(|c| outcomes.any(|o| o != c));
    let next_check = next_check.filter(|_| !done);
    let st = match block {
        _ if retry && here && next_check.is_some() => "ready",
        Some("CI_NOT_GREEN" | "NOT_MERGEABLE" | "HOST_BUSY") if retry => "ready",
        Some("STALE_VERDICT") if retry && done => "ready",
        Some(_) if retry && done && more && how.is_empty() => "refused",
        Some("STALE_VERDICT") if s.state == "blocked" => "stale",
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
        "ready" if next_check.is_some() => {
            let c = next_check.unwrap_or_default();
            let what = drive(&format!("{} PASS", passed.unwrap_or_default()), &format!("spawn {c}"));
            said(what, s.last_ts, cmd("spawn", &format!(" {}", p.role(c).unwrap_or(c))))
        }
        "ready" if !how.is_empty() => {
            let base = pr["baseRefName"].as_str().unwrap_or("?");
            let what = Say::CatchUp(format!("PASS · {how} {base}"));
            let b = to(gate, "conflict");
            said(what, s.last_ts, cmd("spawn", &format!(" {b} --supersede --update")))
        }
        "refused" => {
            let why = blocked.and_then(|e| e.str("reason")).unwrap_or_default();
            let what = format!("PASS · merge refused {}: {why}", block.unwrap_or_default());
            w("owner", &what, s.last_ts, None)
        }
        "ready" if gh::ci(&checks) == "green" => {
            let skip = |k: &str| events.iter().any(|e| e.is(k, f, n) && e.str("sha") == Some(pushed));
            let fast = pr["landing"] == "fast-first";
            let unlabelled = (fc.merge_label.as_deref()).filter(|l| !fast && !gh::has_label(pr, l));
            let what = if pr["isDraft"] == true {
                return w(
                    "drive",
                    &format!("PASS · CI green · {DRAFT}"),
                    s.last_ts,
                    cmd("merge", ""),
                );
            } else if let Some(l) = unlabelled {
                return w(
                    "drive",
                    &format!("PASS · CI green · PR lacks label `{l}`"),
                    s.last_ts,
                    cmd("merge", ""),
                );
            } else if done && skip("verify-skipped") {
                Say::Merge("PASS · verify skipped".into())
            } else if done && skip("verify-no-recipe") {
                Say::Merge("PASS · verify no recipe".into())
            } else {
                Say::MergeFast("PASS · CI green".into())
            };
            said(what, s.last_ts, cmd("merge", ""))
        }
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
        "review" => match rev {
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
                        "drover verdict {f} {n} {v} --sha {} --file {path}",
                        head.unwrap_or_default().0
                    )
                });
                let refused = (events.iter().rev())
                    .filter(|e| e.fleet == *f && e.issue == Some(n))
                    .take_while(|e| !(e.kind == "spawn" && crate::events::checks(e)))
                    .find(|e| e.kind == "repro-refused" && head.is_some_and(|h| e.str("sha") == Some(h.0)));
                let what = match (&next, refused) {
                    (Some(_), Some(e)) => {
                        let why = format!("verdict refused NO_REPRO: {}", e.str("why").unwrap_or_default());
                        return w("drive", &why, s.last_ts, next);
                    }
                    (Some(_), None) => Say::Record("review done".into()),
                    (None, _) => drive("verdict written", "record it"),
                };
                said(what, s.last_ts, next)
            }
            None => w("none", "rev idle without a verdict (check the pane)", s.last_ts, None),
        },
        "fix"
            if !pushed.is_empty()
                && (matches!(s.last_event.as_str(), "verdict" | "repaired")
                    || unspawned(events, s)
                    || lost(events, s, pushed)) =>
        {
            let v = events.iter().rev().find(|e| e.is("verdict", f, n));
            let by = v.and_then(|e| e.str("stage")).unwrap_or("rev");
            let o = if p.get(by).is_some_and(|g| g.kind == "gate") {
                "ci_red"
            } else {
                "FAIL"
            };
            let what = drive("review FAIL", "start fix round");
            said(what, s.last_ts, cmd("spawn", &format!(" {} --supersede", to(by, o))))
        }
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
            let mut what = drive(&format!("{how}{}", &pushed[..7.min(pushed.len())]), "spawn review");
            let tried = events.iter().rfind(|e| e.is("driven", f, n));
            let refused =
                |e: &&Event| e.str("step") == Some("review") && !matches!(e.str("code"), None | Some("EXIT" | "ERROR"));
            if let Some(e) = tried.filter(refused) {
                let why = crate::drive::words(e.str("code"), "review", e.str("result").unwrap_or_default());
                if why.contains(crate::drive::OLD) {
                    return w(
                        "drive",
                        &why,
                        s.last_ts,
                        cmd("spawn", &format!(" {}", to(build, "push"))),
                    );
                }
                what = Say::ReviewerSlot(why);
            }
            said(
                what,
                head.map_or(s.last_ts, |h| h.1.max(s.last_ts)),
                cmd("spawn", &format!(" {}", to(build, "push"))),
            )
        }
        "impl" | "fix" if s.last_event == "lane-killed" => said(
            drive("lane killed", "respawn impl"),
            s.last_ts,
            cmd("spawn", &format!(" {}", p.role(build).unwrap_or(p.start()))),
        ),
        "impl" | "fix" if failed_start(events, s) => w("owner", "failed start (no event back)", s.last_ts, None),
        "impl" | "fix" => w("none", "idle without pushing (check the pane)", s.last_ts, None),
        "design" => {
            let role = if s.lane.is_some() && s.lanes.get("architect") == s.lane.as_ref() {
                "architect"
            } else {
                "design"
            };
            w(
                "none",
                &format!("{role} lane idle without a pushed {role} (check the pane)"),
                s.last_ts,
                cmd("spawn", &format!(" {role} --supersede")),
            )
        }
        "needs-human" => {
            let what = format!("needs-human: your card's [{}]", s.card());
            w("owner", &what, s.last_ts, cmd("extend", ""))
        }
        st => w("none", &format!("nothing to do ({st})"), s.last_ts, None),
    }
}

pub fn scan(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    events: &[Event],
    agents: &[Value],
    live: bool,
) -> Vec<(IssueState, Waiting)> {
    let (mut out, log) = (vec![], Log::new(events));
    let busy = |l: &str| agents.iter().any(|a| a["name"] == l && a["agent_status"] == "working");
    let hung = crate::roots::hung_now(events);
    for s in fold(events).into_values().filter(|s| cfg.fleet.contains_key(&s.fleet)) {
        let Some(lane) = s.lane.clone().or_else(|| {
            s.build
                .as_ref()
                .and_then(|b| s.lanes.get(b))
                .or(s.check_lane())
                .cloned()
                .or((s.state == "needs-human").then(String::new))
        }) else {
            continue;
        };
        let s = IssueState {
            lane: Some(lane.clone()),
            ..s
        };
        let (f, branch) = (&cfg.fleet[&s.fleet], crate::gate::branch_of(Some(&s), s.issue));
        if live && !matches!(s.state.as_str(), "merged" | "closed") && gh::cached_pr(ctx, &s.fleet, s.issue).is_null() {
            gh::run(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now);
        }
        let mine: Vec<Event> = crate::events::of_issue(events, &s.fleet, s.issue)
            .into_iter()
            .cloned()
            .collect();
        let mine = mine.as_slice();
        let sp = last_spawn(mine, &lane);
        if let Some(root) = crate::roots::hung_under(&hung, sp).filter(|_| !s.done()) {
            let w = Waiting {
                fleet: s.fleet.clone(),
                issue: s.issue,
                lane: lane.clone(),
                who: "none",
                what: format!("root hung: {root}"),
                since: s.last_ts,
                next_cmd: None,
                promise: None,
            };
            out.push((s, w));
            continue;
        }
        let rested = |e: &Event| {
            crate::access::rests(&e.kind) && sp.is_some_and(|p| e.ts >= p.ts && e.str("account") == p.str("account"))
        };
        if busy(&lane) && !(sp.is_some_and(|p| p.str("account").is_some()) && mine.iter().any(rested)) {
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
                promise: None,
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
        let since = spawn.map_or(0, |e| e.data.get("since").and_then(Value::as_u64).unwrap_or(e.ts));
        let fresh = |p: &String| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t > std::time::UNIX_EPOCH + std::time::Duration::from_secs(since))
        };
        let there = crate::hosts::On::of(r, spawn);
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
            .map(|v| if v == "FAIL (form-only)" { "FAIL".into() } else { v })
            .or_else(|| kept.and_then(|e| e.str("verdict")).map(String::from));
        let named = text
            .as_deref()
            .and_then(reviewed_head)
            .or_else(|| kept.and_then(|e| e.str("sha")));
        let at = spawn
            .filter(|_| s.check_lane() == Some(&lane))
            .and_then(|e| Some((named.or(e.str("sha"))?, e.ts)));
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
        if w.what.starts_with("PASS · behind ") && !gh::update_ready(ctx, None, f, &s.fleet) {
            let green = gh::ci(&gh::required(&pr["statusCheckRollup"], f.checks(&pr))) == "green";
            let merge = format!("drover merge {} {}", s.fleet, s.issue);
            (w.who, w.next_cmd) = if green { ("drive", Some(merge)) } else { ("ci", None) };
            w.say(&Say::MergeOnGreen(format!(
                "PASS · behind {} (not required)",
                f.integration
            )));
        }
        if let Some(h) = spawn.filter(|_| there.machine.is_some()).and_then(|e| e.str("host")) {
            w.what = w.what.replace("review done", &format!("review done on {h}"));
        }
        if w.what.starts_with("rev idle without") && !agents.iter().any(|a| a["name"] == lane.as_str()) {
            let c = s.check.as_deref().unwrap_or("rev");
            w.who = "drive";
            w.what = "reviewer exited without a verdict: reviewing again".into();
            w.next_cmd = Some(format!("drover spawn {} {} {c} --supersede", s.fleet, s.issue));
        }
        let built = s.build.as_ref().and_then(|b| s.lanes.get(b)).unwrap_or(&lane).clone();
        let sp = last_spawn(mine, &built);
        let unconfirmed = sp.and_then(|e| e.data.get("confirmed")) == Some(&Value::Bool(false));
        let gone = !agents.iter().any(|a| a["name"] == built.as_str());
        let settled = || {
            sp.is_some_and(|e| ctx.now >= e.ts + 120)
                && !crate::drive::pending(ctx, cfg, r)
                    .iter()
                    .any(|p| p.fleet == s.fleet && p.issue == Some(s.issue))
        };
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
        } else if let Some(t) =
            waiting_turn.filter(|t| idle && (!gone && !unconfirmed || t.str("state") == Some("blocked")))
        {
            (w.who, w.what) = turned(mine, t, &built, ctx.now, cfg.limits.stall_minutes * 60);
            (w.lane, w.since) = (built, t.ts);
        } else if idle && (gone || unconfirmed) && settled() {
            let (n, last) = respawns(mine, &s.fleet, s.issue);
            let role = s.build.as_deref().unwrap_or(f.flow().start());
            let cause = crate::shed::exit_cause(mine, &built).filter(|_| gone);
            let why = match cause {
                Some(c) => format!("lane gone ({c})"),
                None => ["prompt never landed", "lane gone"][usize::from(gone)].into(),
            };
            let resume = waiting_turn.is_none() && resumed(&why);
            (w.who, w.lane, w.promise) = ("drive", built, None);
            w.next_cmd = Some(format!("drover spawn {} {} {role} --supersede", s.fleet, s.issue));
            w.what = if n >= 2 && !resume {
                w.next_cmd = None;
                format!("lane keeps ending{NO_PUSH}needs you")
            } else if last > 0 && ctx.now < last + RESPAWN_SECS {
                format!(
                    "{why}{NO_PUSH}respawned {} ago, again after 30 min",
                    age(ctx.now - last)
                )
            } else {
                format!("{why}{NO_PUSH}respawning {role}")
            };
        }
        if let Some(h) = crate::hosts::offline(ctx, sp).filter(|_| s.active_lane().is_some()) {
            (w.who, w.what, w.next_cmd, w.promise) = ("none", format!("host offline: {h}"), None, None);
        }
        let stuck = ["idle without pushing", "rev idle without", "reviewer exited"]
            .iter()
            .any(|p| w.what.starts_with(p));
        let spawning = w.next_cmd.as_deref().is_some_and(|c| c.starts_with("drover spawn "));
        let hold = stuck || spawning || w.what.contains(NO_PUSH);
        if let Some(t) = sp
            .and_then(|e| crate::breaker::says(log.all, crate::breaker::host(e)))
            .filter(|_| hold)
        {
            (w.who, w.what, w.next_cmd, w.promise) = ("none", t, None, None);
        }
        let sp = s.active_lane().and_then(|(_, a)| last_spawn(mine, a));
        if sp.is_some_and(|e| crate::hook::rules_stale(cfg, &s.fleet, e)) {
            w.what += " · rules stale";
        }
        let gone = || gh::missing(ctx, live.then_some(r), cfg, (&s.fleet, s.issue), mine, &pr);
        let m = w.what.starts_with("PASS · CI running").then(gone).flatten();
        w.what = m.map_or(w.what, |m| format!("PASS · {m}"));
        if let Some(l) = f
            .merge_label
            .as_deref()
            .filter(|l| w.who == "ci" && !gh::has_label(&pr, l))
        {
            w.what = format!("PASS, PR lacks label `{l}`, so the full CI has not run");
        }
        if pr["isDraft"] == true && w.what.starts_with("PASS") && !w.what.ends_with(DRAFT) {
            (w.what, w.promise) = (format!("PASS · {DRAFT}"), None);
        }
        let tried = mine.iter().rfind(|e| matches!(e.kind.as_str(), "driven" | "spawn"));
        let spawn = w.next_cmd.as_deref().is_some_and(|c| c.starts_with("drover spawn "));
        if spawn && tried.is_some_and(|e| e.str("code") == Some("SPAWN_PENDING")) {
            let by = crate::drive::held_by(ctx, cfg, r);
            w.say(&Say::SpawnSlot((!by.is_empty()).then_some(by)));
        }
        if let Some(why) = spawn
            .then(|| crate::drive::retrying(ctx, mine, &s.fleet, s.issue))
            .flatten()
        {
            w.what = why;
        }
        if let Some(h) = spawn
            .then(|| held(mine, (&s.fleet, s.issue), w.next_cmd.as_deref()))
            .flatten()
        {
            w.what = match crate::promise::strip_catch_up(&w.what) {
                Some(p) => format!("{p}{h}"),
                None => format!("{} · {h}", w.what),
            };
        }
        let at = gh::cached_at(ctx, &s.fleet, s.issue).filter(|t| ctx.now > t + 120);
        if let Some(t) = at.filter(|_| w.what.starts_with("PASS · ")) {
            w.what += &format!(" · checked {} ago", age(ctx.now - t));
        }
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
            (w.who, w.what, w.next_cmd, w.promise) = ("none", format!("parked: fleet paused{pr}"), None, None);
        }
        let head = pr["headRefOid"].as_str().unwrap_or_default();
        if let Some(m) = crate::promise::missed(mine, &s, head, w.promise, ctx.now) {
            w.what = m;
        }
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
    let review = |w: &Waiting| {
        let role = w.next_cmd.as_deref().and_then(|c| c.strip_prefix("drover spawn "));
        let role = role.and_then(|c| c.split(' ').nth(2));
        role.is_some_and(|r| cfg.fleet[&w.fleet].flow().is_check(r))
    };
    let held = |w: &Waiting| {
        let mine = crate::events::of_issue(events, &w.fleet, w.issue);
        let tried = mine
            .into_iter()
            .rfind(|e| matches!(e.kind.as_str(), "driven" | "spawn"));
        tried
            .and_then(|e| e.str("code"))
            .is_some_and(|c| ["SPAWN_PENDING", "HOST_FULL"].contains(&c) || HOST_WAIT.contains(&c))
    };
    let ahead = out
        .iter_mut()
        .filter(|(s, w)| s.lane.as_ref() == Some(&w.lane) && review(w));
    for (ahead, (_, w)) in ahead.enumerate().filter(|(n, (_, w))| *n > 0 && held(w)) {
        w.say(&Say::ReviewsAhead(ahead));
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

pub fn held(events: &[Event], (f, n): (&str, u64), cmd: Option<&str>) -> Option<String> {
    let mine = |e: &&Event| e.fleet == f && e.issue == Some(n) && matches!(e.kind.as_str(), "driven" | "spawn");
    let e = events.iter().rfind(mine)?;
    let wait = ["EXIT", "ERROR", "SPAWN_PENDING", "SPAWN_BACKOFF"];
    let code = e.str("code").filter(|c| !wait.contains(c))?;
    let c = e.str("cmd").filter(|c| c.starts_with("drover spawn "))?;
    cmd.is_none_or(|x| c.starts_with(x)).then_some(())?;
    let why = e.str("result").unwrap_or_default();
    let why = why.split_once(": ").map_or(why, |p| p.1);
    Some(format!(
        "held: {code} {}",
        why.split("; retry later").next().unwrap_or(why)
    ))
}

/// #792: a working lane's silent-lane recovery since its spawn: ` · nudged 3m ago: silent 10m mid-turn`.
pub fn recovered(events: &[Event], lane: &str, now: u64) -> String {
    let sp = last_spawn(events, lane);
    let mine = |e: &&Event| {
        matches!(e.kind.as_str(), "nudged" | "respawned-silent")
            && e.str("agent") == Some(lane)
            && sp.is_some_and(|sp| e.ts >= sp.ts)
    };
    events.iter().rfind(mine).map_or(String::new(), |e| {
        let how = ["nudged", "respawned"][usize::from(e.kind == "respawned-silent")];
        format!(
            " · {how} {} ago: {}",
            age(now.saturating_sub(e.ts)),
            e.str("why").unwrap_or_default()
        )
    })
}

const HOST_WAIT: [&str; 5] = ["HOST_BUSY", "DISK_LOW", "DISK_BUDGET", "MEM_LOW", "ALL_CAPPED"];
pub const ORPHAN_SECS: u64 = 1800;

/// #493: since when nothing has driven guarded issue `s`, once that is 30 min: no working lane, no drive action on it (a due step, or a refused one drive retries) and no named wait (an open ask, a hold, a paused fleet, CI after PASS).
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
    let held = !crate::events::held_on(log.all, s).is_empty() || !log.blockers(&s.fleet, s.issue).is_empty();
    let held = held || self::held(log.all, (&s.fleet, s.issue), None).is_some();
    if f.paused || ci || held || s.lanes.values().any(working) || log.asks.iter().any(asked) {
        return None;
    }
    let (mut since, mut seen) = (s.last_ts, false);
    for e in log.all.iter().filter(|e| e.kind == "drive-pass") {
        seen |= e.fleet == s.fleet;
        for a in e.data.get("actions").and_then(Value::as_array).into_iter().flatten() {
            let code = a["code"].as_str().unwrap_or_default();
            let wait = HOST_WAIT.contains(&code) || code == "HOST_FULL" || code == "CAP" && e.fleet == s.fleet;
            let old = a["words"].as_str().is_some_and(|w| w.contains(crate::drive::OLD));
            if e.fleet == s.fleet && a["issue"] == s.issue && !old || s.state == "queued" && wait {
                since = since.max(e.ts);
            }
        }
    }
    (seen && now >= since + ORPHAN_SECS).then_some(since)
}
