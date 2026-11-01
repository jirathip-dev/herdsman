//! #46: what an idle lane waits on (orch | owner | ci | none), worked out from the log, git and the cached PR.
use crate::config::Config;
use crate::events::{age, fold, last_spawn, open_asks, Event, IssueState};
use crate::runner::{ok, Runner};
use crate::{gh, Ctx};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Waiting {
    pub fleet: String,
    /// 0 for the orch lane.
    pub issue: u64,
    /// The lane this row is for; every lane of an issue gets one.
    pub lane: String,
    /// orch | owner | ci | none
    pub who: &'static str,
    pub what: String,
    pub since: u64,
    pub next_cmd: Option<String>,
}

impl Waiting {
    /// `idle · <what> (<age>)`
    pub fn label(&self, now: u64) -> String {
        format!("idle · {} ({})", self.what, age(now.saturating_sub(self.since)))
    }
    /// Page tone: owner red, orch amber past 5 min, an impl idle without pushing amber at once, the rest dim.
    pub fn tone(&self, now: u64) -> &'static str {
        match self.who {
            "owner" => "r",
            "orch" if now.saturating_sub(self.since) > 300 => "a",
            "none" if self.what.contains("idle without") => "a",
            _ => "m",
        }
    }
}

/// Pure: the issue's wait reason. `head` = the lane worktree's (sha, commit time); `pr` = the (cached) PR view;
/// `rev` = the rev worktree's verdict.md (absolute path, first line) when written since the rev spawn.
pub fn waiting_on(
    events: &[Event],
    s: &IssueState,
    head: Option<(&str, u64)>,
    pr: &Value,
    req: &[String],
    rev: Option<(&str, &str)>,
) -> Waiting {
    let (f, n) = (&s.fleet, s.issue);
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
    if let Some(a) = open_asks(events).into_iter().find(mine) {
        return w(
            "owner",
            &format!("waiting on your answer ({})", a.str("id").unwrap_or("?")),
            a.ts,
            None,
        );
    }
    if s.state == "merged" || pr["state"] == "MERGED" {
        return w("none", "nothing to do (merged): clean me", s.last_ts, None);
    }
    if pr["state"] == "CLOSED" {
        return w("none", "nothing to do (closed): clean me", s.last_ts, None);
    }
    let pushed = pr["headRefOid"].as_str().unwrap_or_default();
    // #102: a PASSed (or re-pushed fix) PR that conflicts with, or is behind, the integration branch gets an update lane.
    let how = match (pr["mergeable"].as_str(), pr["mergeStateStatus"].as_str()) {
        (Some("CONFLICTING"), _) => "conflicts with",
        (_, Some("BEHIND")) => "is behind",
        _ => "",
    };
    match s.state.split('(').next().unwrap_or_default() {
        "ready" | "fix"
            if !how.is_empty() && (s.state == "ready" || s.last_event != "verdict" && !s.reviewed(pushed)) =>
        {
            let what = format!(
                "conflict: PR #{} {how} {}",
                pr["number"],
                pr["baseRefName"].as_str().unwrap_or("?")
            );
            w("orch", &what, s.last_ts, cmd("spawn", " impl --supersede --update"))
        }
        "ready" if gh::ci(&gh::required(&pr["statusCheckRollup"], req)) == "green" => {
            w("orch", "PASS + green, waiting for merge", s.last_ts, cmd("merge", ""))
        }
        "ready" => w("ci", "waiting for CI", s.last_ts, None),
        // The exact command, or none (no nudge) while the verdict word or the head sha is unknown.
        "review" => match rev {
            // #92: a verdict is for the head the rev lane was spawned at; a moved PR head gets a fresh reviewer.
            Some(_) if head.is_some_and(|h| !pushed.is_empty() && h.0 != pushed) => {
                let what = format!(
                    "verdict is for {:.7}, the PR head moved to {pushed:.7}: review again",
                    head.unwrap_or_default().0
                );
                w("orch", &what, s.last_ts, cmd("spawn", " rev --supersede"))
            }
            Some((path, v)) => {
                let at = head.is_some_and(|h| h.0 == pushed);
                let next = (matches!(v, "PASS" | "FAIL") && at)
                    .then(|| format!("herdsman verdict {f} {n} {v} --sha {pushed} --file {path}"));
                w(
                    "orch",
                    "verdict written, waiting for orch to record it",
                    s.last_ts,
                    next,
                )
            }
            None => w("none", "rev idle without a verdict (check the pane)", s.last_ts, None),
        },
        "fix" if s.last_event == "verdict" => w(
            "orch",
            "review FAIL, waiting for orch to start fix round",
            s.last_ts,
            cmd("spawn", " impl --supersede"),
        ),
        "impl" | "fix" if !pushed.is_empty() && !s.reviewed(pushed) && head.is_none_or(|h| h.0 == pushed) => {
            let what = format!(
                "pushed {}{}, waiting for orch to spawn review",
                if s.rounds > 0 { "fix " } else { "" },
                &pushed[..7.min(pushed.len())]
            );
            w(
                "orch",
                &what,
                head.map_or(s.last_ts, |h| h.1.max(s.last_ts)),
                cmd("spawn", " rev"),
            )
        }
        "impl" | "fix" if s.last_event == "lane-killed" => w(
            "orch",
            "lane killed, waiting for orch to respawn impl",
            s.last_ts,
            cmd("spawn", " impl"),
        ),
        "impl" | "fix" => w("none", "idle without pushing (check the pane)", s.last_ts, None),
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
    let mut out = vec![];
    let busy = |l: &str| agents.iter().any(|a| a["name"] == l && a["agent_status"] == "working");
    for s in fold(events).into_values().filter(|s| cfg.fleet.contains_key(&s.fleet)) {
        // #43: after a `lane-killed`, the issue's surviving lane carries its reason (a killed rev: spawn rev again).
        let Some(lane) = s
            .lane
            .clone()
            .or_else(|| s.lanes.get("impl").or(s.lanes.get("rev")).cloned())
        else {
            continue;
        };
        let s = IssueState {
            lane: Some(lane.clone()),
            ..s
        };
        if busy(&lane) {
            let who = if s.lanes.get("rev") == Some(&lane) {
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
            out.extend(s.lanes.values().filter(|l| !busy(l)).map(|l| {
                (
                    s.clone(),
                    Waiting {
                        lane: l.clone(),
                        ..w.clone()
                    },
                )
            }));
            continue;
        }
        let (f, branch) = (&cfg.fleet[&s.fleet], crate::gate::branch_of(Some(&s), s.issue));
        let pr = match (s.state.as_str(), live) {
            ("merged", _) => Value::Null,
            (_, true) => gh::pr(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now).unwrap_or_default(),
            _ => gh::cached_pr(ctx, &s.fleet, s.issue),
        };
        let wt = last_spawn(events, &lane)
            .and_then(|e| e.str("worktree"))
            .filter(|_| s.state != "merged");
        let git = wt.and_then(|wt| ok(r, "git", &["-C", wt, "log", "-1", "--format=%H %ct"]).ok());
        let head = git
            .as_deref()
            .and_then(|g| g.trim().split_once(' '))
            .and_then(|(h, t)| Some((h, t.parse().ok()?)));
        let spawn = s.lanes.get("rev").and_then(|l| last_spawn(events, l));
        let path = spawn
            .and_then(|e| e.str("worktree"))
            .map(|wt| format!("{wt}/verdict.md"));
        let fresh = |p: &String| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t > std::time::UNIX_EPOCH + std::time::Duration::from_secs(spawn.map_or(0, |e| e.ts)))
        };
        let word = path
            .as_ref()
            .filter(|p| fresh(p))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|v| v.lines().next().unwrap_or_default().trim().to_string());
        // In review, `head` is the one the rev lane was spawned at (logged since #92; else its worktree's).
        let at = spawn
            .filter(|_| s.lanes.get("rev") == Some(&lane))
            .and_then(|e| Some((e.str("sha")?, e.ts)));
        let mut w = waiting_on(
            events,
            &s,
            at.or(head),
            &pr,
            &f.required_checks,
            path.as_deref().zip(word.as_deref()),
        );
        // #83: a paused fleet's lane with no pane and a clean, pushed worktree is parked until `resume`.
        let safe = || wt.is_some_and(|wt| crate::spawn::dirty(r, std::path::Path::new(wt)).is_none());
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
        out.extend(s.lanes.values().filter(|l| **l != lane && !busy(l)).map(|l| {
            (
                s.clone(),
                Waiting {
                    lane: l.clone(),
                    ..w.clone()
                },
            )
        }));
        out.push((s, w));
    }
    out
}

/// The orch lane's reason: the items waiting on it, if any.
pub fn orch(waits: &[Waiting], fleet: &str) -> Option<Waiting> {
    let mine: Vec<_> = waits.iter().filter(|w| w.who == "orch" && w.fleet == fleet).collect();
    let since = mine.iter().map(|w| w.since).min()?;
    let what = format!(
        "orch idle with {} items waiting",
        mine.iter()
            .map(|w| w.issue)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    Some(Waiting {
        fleet: fleet.into(),
        issue: 0,
        lane: format!("{fleet}-orch"),
        who: "orch",
        what,
        since,
        next_cmd: None,
    })
}
