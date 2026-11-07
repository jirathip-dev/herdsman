//! Cached GitHub reads: the issue spec (re-fetched when `updatedAt` moved) and PR/CI views fresh for 60 s, all of a
//! fleet's open PRs read in one GraphQL call (#246).
use crate::config::{Config, Fleet};
use crate::events::Event;
use crate::runner::{json, ok, Runner};
use crate::usage::load;
use crate::Ctx;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PR_FIELDS: &str =
    "number,title,state,baseRefName,headRefOid,mergeable,mergeStateStatus,statusCheckRollup,labels,mergedAt,mergeCommit";
pub const PR_TTL: u64 = 60;
const SPEC_FIELDS: &str = "title,body,updatedAt,comments";

pub fn cache_path(ctx: &Ctx, kind: &str, fleet: &str, issue: u64) -> PathBuf {
    ctx.state
        .parent()
        .unwrap_or(&ctx.state)
        .join("cache")
        .join(format!("{kind}-{fleet}-{issue}.json"))
}

// #212: the fast step reads every PR afresh, so a minute's launchd jitter never leaves a card 2 min old.
thread_local!(pub static FORCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
// #626: the fast step's catch-up interval (secs, 0: off): a fleet with webhooks on is re-read only this often.
thread_local!(pub static CATCHUP: std::cell::Cell<u64> = const { std::cell::Cell::new(0) });

/// A `{"ts": …}` cache entry younger than `PR_TTL` at `now` (#246: while `FORCE` is set, only one of this step).
fn fresh(c: &Value, now: u64) -> bool {
    let ts = c["ts"].as_u64().unwrap_or(0);
    now >= ts && now - ts < PR_TTL && (!FORCE.get() || ts == now)
}

pub(crate) fn store(p: &Path, v: &Value) -> Result<()> {
    std::fs::create_dir_all(p.parent().unwrap_or(p))?;
    crate::config::write_atomic(p, &v.to_string())
}

/// #212: when the cached PR read was taken (the fast step refreshes it each minute).
pub fn cached_at(ctx: &Ctx, fleet: &str, issue: u64) -> Option<u64> {
    load::<Value>(&cache_path(ctx, "pr", fleet, issue))?["ts"].as_u64()
}

/// The cached spec, if any; never calls GitHub (dry runs).
pub fn cached_spec(ctx: &Ctx, fleet: &str, issue: u64) -> Option<Value> {
    load(&cache_path(ctx, "issue", fleet, issue))
}

/// The cached issue title, if any (#391: else the last backlog read's, for a row queued from it); never calls GitHub.
pub fn cached_title(ctx: &Ctx, fleet: &str, issue: u64) -> Option<String> {
    let b = || load::<Value>(&cache_path(ctx, "backlog", fleet, 0)).map(|b| b["issues"][issue.to_string()].clone());
    Some(cached_spec(ctx, fleet, issue).or_else(b)?["title"].as_str()?.into())
}

/// `(url, text)` of a `**Amendment` comment by an OWNER, MEMBER or COLLABORATOR; strangers can't steer a lane.
fn amendment(c: &Value) -> Option<(String, String)> {
    let s = |k: &str| c.pointer(k).and_then(Value::as_str).unwrap_or_default().trim();
    let body = s("/body");
    if !matches!(s("/authorAssociation"), "OWNER" | "MEMBER" | "COLLABORATOR") || !body.starts_with("**Amendment") {
        return None;
    }
    let url = s("/url");
    let text = format!("\n\n{body} — {}, {} ({url})", s("/author/login"), s("/createdAt"));
    Some((url.to_string(), text))
}

/// `{title, body + amendments, updatedAt, amended: last url}` (#101); `refresh` re-checks `updatedAt` first.
pub fn spec(ctx: &Ctx, r: &dyn Runner, f: &Fleet, fleet: &str, issue: u64, refresh: bool) -> Result<Value> {
    let p = cache_path(ctx, "issue", fleet, issue);
    let n = issue.to_string();
    if let Some(c) = load(&p) {
        if !refresh {
            return Ok(c);
        }
        let u = json(r, "gh", &["issue", "view", &n, "-R", &f.repo, "--json", "updatedAt"])?;
        if !u["updatedAt"].is_null() && u["updatedAt"] == c["updatedAt"] {
            return Ok(c);
        }
    }
    let mut v = json(r, "gh", &["issue", "view", &n, "-R", &f.repo, "--json", SPEC_FIELDS])?;
    let comments = v["comments"].as_array().cloned().unwrap_or_default();
    let amends: Vec<(String, String)> = comments.iter().filter_map(amendment).collect();
    if let Some((url, _)) = amends.last() {
        let list: String = amends.iter().map(|a| a.1.as_str()).collect();
        let body = format!("{}\n\n## Amendments{list}", v["body"].as_str().unwrap_or_default());
        v["body"] = json!(body);
        v["amended"] = json!(url);
    }
    store(&p, &v)?;
    Ok(v)
}

/// #246: every open PR of the repo, flattened to `gh pr view`'s shape, in one paginated GraphQL call.
const PRS_QUERY: &str = "query($owner:String!,$name:String!,$endCursor:String){repository(owner:$owner,name:$name){\
pullRequests(states:OPEN,first:100,after:$endCursor){pageInfo{hasNextPage endCursor}nodes{number title body state \
baseRefName headRefName headRefOid mergeable mergeStateStatus labels(first:50){nodes{name}} commits(last:1){nodes{\
commit{statusCheckRollup{contexts(first:100){nodes{__typename ...on CheckRun{name status conclusion startedAt \
completedAt detailsUrl} ...on StatusContext{context state targetUrl}}}}}}}}}}}";
const PRS_JQ: &str = ".data.repository.pullRequests.nodes[] | .labels = .labels.nodes | .statusCheckRollup = \
[.commits.nodes[0].commit.statusCheckRollup.contexts.nodes[]?] | del(.commits)";

/// #530: conditional REST reads that change whenever the snapshot can: the open PRs latest-updated first (heads,
/// labels, titles), the Actions runs (checks) and integration's head (mergeability).
fn probes(f: &Fleet) -> [String; 3] {
    let r = &f.repo;
    [
        format!("repos/{r}/pulls?state=open&sort=updated&direction=desc&per_page=100"),
        format!("repos/{r}/actions/runs?per_page=50"),
        format!("repos/{r}/branches/{}", f.integration),
    ]
}

/// #530: each probe sent with its last `ETag` as `If-None-Match`, `etags` updated; true when none moved (a 304),
/// which costs no rate limit.
fn unchanged(r: &dyn Runner, f: &Fleet, etags: &mut Vec<Value>) -> bool {
    let tag = |l: &str| {
        Some(json!(l
            .split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("etag"))?
            .1
            .trim()))
    };
    let mut new = vec![];
    for (i, path) in probes(f).iter().enumerate() {
        let old = etags.get(i).and_then(Value::as_str).unwrap_or_default();
        let h = format!("If-None-Match: {old}");
        let a: &[&str] = if old.is_empty() {
            &["api", "-i", path]
        } else {
            &["api", "-i", "-H", &h, path]
        };
        let out = r
            .run("gh", &crate::runner::argv(a))
            .map(|o| o.stdout)
            .unwrap_or_default();
        let mut lines = out.lines();
        let hit = !old.is_empty() && lines.next().is_some_and(|l| l.split(' ').nth(1) == Some("304"));
        new.push(if hit {
            json!(old)
        } else {
            lines.take_while(|l| !l.trim().is_empty()).find_map(tag).into()
        });
    }
    let same = new
        .iter()
        .zip(etags.iter())
        .filter(|(a, b)| a == b && !a.is_null())
        .count()
        == 3;
    *etags = new;
    same
}

/// #530: past this, or while a PR's mergeability is still UNKNOWN, the fast step queries even when every probe is 304.
const PROBE_MAX: u64 = 300;

/// #246: the fleet's open PRs by head branch: one `gh api graphql` per fleet per step (fresh as `fresh` says). #530:
/// the fast step skips it while `unchanged` says nothing moved since the last query.
pub fn snapshot(ctx: &Ctx, r: &dyn Runner, f: &Fleet, fleet: &str, now: u64) -> Result<Value> {
    let p = cache_path(ctx, "prs", fleet, 0);
    let old = load::<Value>(&p).unwrap_or_default();
    let c = if fresh(&old, now) {
        old
    } else {
        let mut etags = old["etags"].as_array().cloned().unwrap_or_default();
        let at = old["at"].as_u64().unwrap_or(0);
        let settled = old["prs"]
            .as_object()
            .is_some_and(|m| m.values().all(|v| v["mergeable"] != "UNKNOWN"));
        let age = now.saturating_sub(at);
        let on = || crate::secret::lookup(r, fleet, crate::webhook::SECRET).is_ok_and(|s| s.is_some());
        let hooked = || settled && age < CATCHUP.get() && on();
        let same = FORCE.get() && (hooked() || unchanged(r, f, &mut etags) && settled && age < PROBE_MAX);
        let c = if same {
            json!({"ts": now, "at": at, "prs": old["prs"], "etags": etags})
        } else {
            let (owner, name) = f.repo.split_once('/').unwrap_or_default();
            let v = [
                format!("owner={owner}"),
                format!("name={name}"),
                format!("query={PRS_QUERY}"),
            ];
            let a = [
                &["api", "graphql", "--paginate", "--jq", PRS_JQ][..],
                &["-f", &v[0], "-f", &v[1], "-f", &v[2]],
            ];
            match ok(r, "gh", &a.concat()) {
                Ok(out) => {
                    let prs = out.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok());
                    let by = prs.map(|v| (v["headRefName"].as_str().unwrap_or_default().to_string(), v));
                    let prs = by.collect::<serde_json::Map<_, _>>();
                    json!({"ts": now, "at": now, "prs": prs, "etags": etags})
                }
                Err(e) => json!({"ts": now, "error": format!("{e:#}")}),
            }
        };
        let _ = store(&p, &c);
        c
    };
    match c["error"].as_str() {
        Some(e) => anyhow::bail!("open PRs of {}: {e}", f.repo),
        None => Ok(c["prs"].clone()),
    }
}

const ISSUES_QUERY: &str = "query($owner:String!,$name:String!,$since:DateTime,$endCursor:String){repository(owner:$owner,\
name:$name){issues(states:STATES,filterBy:{since:$since},first:100,after:$endCursor){pageInfo{hasNextPage endCursor}nodes{\
number title state body createdAt updatedAt labels(first:20){nodes{name}} timelineItems(itemTypes:[CROSS_REFERENCED_EVENT],\
first:20){nodes{...on CrossReferencedEvent{source{...on PullRequest{state}}}}}}}}}";
const ISSUES_JQ: &str =
    ".data.repository.issues.nodes[] | {number, title, state, created: .createdAt, updated: .updatedAt, \
labels: [.labels.nodes[].name], ac: (.body | test(\"acceptance criteria|(^|\\\\n)#+ *ac *(\\\\n|$)\"; \"i\")), \
pr: any(.timelineItems.nodes[].source.state?; . == \"MERGED\" or . == \"CLOSED\")}";

/// #365: the repo's open issues (the backlog) in `backlog-<fleet>-0.json`, one GraphQL read: all open ones, then those
/// updated since (a closed one drops out); `ac`: has acceptance criteria, `pr`: a merged/closed PR mentions it.
pub fn backlog(ctx: &Ctx, r: &dyn Runner, f: &Fleet, fleet: &str) -> Result<()> {
    let p = cache_path(ctx, "backlog", fleet, 0);
    let mut c = load::<Value>(&p).unwrap_or_else(|| json!({"issues": {}}));
    let since = c["since"].as_str().map(|s| format!("since={s}"));
    let q = ISSUES_QUERY.replace("STATES", if since.is_some() { "[OPEN,CLOSED]" } else { "OPEN" });
    let (owner, name) = f.repo.split_once('/').unwrap_or_default();
    let v = [format!("owner={owner}"), format!("name={name}"), format!("query={q}")];
    let mut a = ["api", "graphql", "--paginate", "--jq", ISSUES_JQ]
        .map(String::from)
        .to_vec();
    a.extend(v.into_iter().chain(since).flat_map(|x| ["-f".into(), x]));
    let out = ok(r, "gh", &a)?;
    for i in out.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        c["since"] = json!(c["since"].as_str().max(i["updated"].as_str()));
        let (n, m) = (i["number"].to_string(), c["issues"].as_object_mut());
        let m = m.ok_or_else(|| anyhow::anyhow!("bad backlog cache"))?;
        m.remove(&n);
        m.extend((i["state"] == "OPEN").then_some((n, i)));
    }
    c["ts"] = json!(ctx.now);
    store(&p, &c)
}

/// The PR for `branch` as of `now`, from the fleet's snapshot; Null when there is none. #246: a PR that left the open
/// set, or one fleet just changed (`forget_pr`), is read alone (`gh pr view`).
pub fn pr(ctx: &Ctx, r: &dyn Runner, f: &Fleet, (fleet, issue): (&str, u64), branch: &str, now: u64) -> Result<Value> {
    let p = cache_path(ctx, "pr", fleet, issue);
    let c = load::<Value>(&p).filter(|c| c["branch"] == branch);
    if let Some(c) = c.as_ref().filter(|c| fresh(c, now)) {
        return Ok(c["pr"].clone());
    }
    let (old, forgot) = c.map_or((Value::Null, false), |c| (c["pr"].clone(), c["forgot"] == true));
    let open = (!forgot).then(|| snapshot(ctx, r, f, fleet, now)).transpose()?;
    let pr = match open.as_ref().and_then(|o| o.get(branch)) {
        Some(v) => v.clone(),
        // Outside a step (`pr`, `wait`) a branch never seen open is read too: it may be merged already.
        None if forgot || old["state"] == "OPEN" || old.is_null() && !FORCE.get() => {
            json(r, "gh", &["pr", "view", branch, "-R", &f.repo, "--json", PR_FIELDS]).unwrap_or_default()
        }
        None => old,
    };
    store(&p, &json!({"ts": now, "branch": branch, "pr": pr}))?;
    Ok(pr)
}

/// The last cached PR read, however old; never calls `gh` (Null when none).
pub fn cached_pr(ctx: &Ctx, fleet: &str, issue: u64) -> Value {
    load::<Value>(&cache_path(ctx, "pr", fleet, issue)).map_or(Value::Null, |c| c["pr"].clone())
}

/// #169: the latest Actions run url for an issue branch with no PR: one `gh run list` per 60 s, by the tick only.
pub fn run(ctx: &Ctx, r: &dyn Runner, f: &Fleet, (fleet, issue): (&str, u64), branch: &str, now: u64) {
    let p = cache_path(ctx, "run", fleet, issue);
    if FORCE.get() || load::<Value>(&p).is_some_and(|c| fresh(&c, now)) {
        return;
    }
    let a = [
        "run", "list", "-R", &f.repo, "--branch", branch, "--limit", "1", "--json", "url",
    ];
    let url = json(r, "gh", &a).ok().map(|v| v[0]["url"].clone());
    let _ = store(&p, &json!({"ts": now, "url": url}));
}

/// The cached latest run url for the issue, if any; never calls `gh`.
pub fn cached_run(ctx: &Ctx, fleet: &str, issue: u64) -> Option<String> {
    load::<Value>(&cache_path(ctx, "run", fleet, issue)).and_then(|c| c["url"].as_str().map(String::from))
}

/// Numbers of the fleet's closed issues: one `gh issue list` per fleet, fresh for 60 s.
pub fn closed(ctx: &Ctx, r: &dyn Runner, f: &Fleet, fleet: &str, now: u64) -> Result<Vec<u64>> {
    let p = cache_path(ctx, "closed", fleet, 0);
    if let Some(c) = load(&p).filter(|c| fresh(c, now)) {
        return Ok(c["closed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .collect());
    }
    let v = json(
        r,
        "gh",
        &[
            "issue", "list", "-R", &f.repo, "--state", "closed", "--limit", "1000", "--json", "number",
        ],
    )?;
    let closed: Vec<u64> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["number"].as_u64())
        .collect();
    store(&p, &json!({"ts": now, "closed": closed}))?;
    Ok(closed)
}

/// Files PR `num` changes at `head`, a rename's old name too, cached per head sha. #490: the paginated files API (the
/// diff stops at 300 files); at its 3,000 cap, the fetched checkout's `git diff <base>...<head>`.
pub fn diff_names(
    ctx: &Ctx,
    r: &dyn Runner,
    f: &Fleet,
    (fleet, issue): (&str, u64),
    num: &str,
    head: &str,
) -> Result<Vec<String>> {
    let p = cache_path(ctx, "diff", fleet, issue);
    let files = if let Some(c) = load::<Value>(&p).filter(|c| c["head"] == head) {
        c["files"].clone()
    } else {
        let jq = r#".[] | [.filename, .previous_filename // empty] | join("\t")"#;
        let path = format!("repos/{}/pulls/{num}/files", f.repo);
        let mut out = ok(r, "gh", &["api", &path, "--paginate", "--jq", jq])?;
        if out.lines().filter(|l| !l.trim().is_empty()).count() >= 3000 {
            let (dir, range) = (f.local_path(), format!("origin/{}...{head}", f.integration));
            crate::overlap::git(r, &dir, &["fetch", "-q", "origin"])?;
            out = crate::overlap::git(r, &dir, &["diff", "--name-only", "--no-renames", &range])?;
        }
        let names = out.lines().flat_map(|l| l.split('\t')).map(str::trim);
        let v = json!(names.filter(|l| !l.is_empty()).collect::<Vec<_>>());
        store(&p, &json!({"head": head, "files": v}))?;
        v
    };
    Ok(files
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect())
}

/// #626: the fleet's open-PR snapshot is read afresh by the next step (a webhook said a PR moved).
pub fn stale(ctx: &Ctx, fleet: &str) {
    let p = cache_path(ctx, "prs", fleet, 0);
    if let Some(mut c) = load::<Value>(&p) {
        (c["ts"], c["at"]) = (json!(0), json!(0));
        let _ = store(&p, &c);
    }
}

/// Drop the cached PR read (after fleet changed the PR itself): the next read is of that PR alone (#246).
pub fn forget_pr(ctx: &Ctx, fleet: &str, issue: u64) {
    let p = cache_path(ctx, "pr", fleet, issue);
    if let Some(c) = load::<Value>(&p) {
        let _ = store(&p, &json!({"branch": c["branch"], "forgot": true}));
    }
}

/// #285: a ready PR is caught up with integration only when the fleet says `update_ready = "always"` or integration's
/// rule is `strict`: its ruleset's, else its classic protection's (read once an hour). `r` None: the cached read only.
pub fn update_ready(ctx: &Ctx, r: Option<&dyn Runner>, f: &Fleet, fleet: &str) -> bool {
    let p = cache_path(ctx, "strict", fleet, 0);
    let c = load::<Value>(&p).filter(|c| r.is_none() || ctx.now < c["ts"].as_u64().unwrap_or(0) + 3600);
    let strict = || {
        let read = |path: &str, jq: &str| ok(r?, "gh", &["api", &format!("repos/{}/{path}", f.repo), "--jq", jq]).ok();
        let rule =
            ".[] | select(.type == \"required_status_checks\") | .parameters.strict_required_status_checks_policy";
        let b = &f.integration;
        let s = (read(&format!("rules/branches/{b}"), rule).filter(|o| !o.trim().is_empty()))
            .or_else(|| read(&format!("branches/{b}/protection"), ".required_status_checks.strict"))
            .is_some_and(|o| o.trim() == "true");
        r.and_then(|_| store(&p, &json!({"ts": ctx.now, "strict": s})).ok());
        s
    };
    f.update_ready.as_deref() == Some("always") || c.map_or_else(strict, |c| c["strict"] == true)
}

/// The PR view carries label `name`.
pub fn has_label(pr: &Value, name: &str) -> bool {
    pr["labels"].as_array().into_iter().flatten().any(|l| l["name"] == name)
}

/// One check of a statusCheckRollup: green | pending | failed | missing.
pub fn check_state(c: &Value) -> &'static str {
    match c["__typename"].as_str() {
        _ if c["status"] == "MISSING" => "missing",
        Some("StatusContext") => match c["state"].as_str() {
            Some("SUCCESS") => "green",
            Some("PENDING" | "EXPECTED") => "pending",
            _ => "failed",
        },
        _ if c["status"] != "COMPLETED" => "pending",
        _ if matches!(c["conclusion"].as_str(), Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => "green",
        _ => "failed",
    }
}

pub fn check_name(c: &Value) -> &str {
    c["name"].as_str().or(c["context"].as_str()).unwrap_or("?")
}

/// The rollup narrowed to the latest run (by `startedAt`) of each `required` name or a `MISSING` stub; empty = all.
pub fn required(rollup: &Value, required: &[String]) -> Value {
    if required.is_empty() {
        return rollup.clone();
    }
    let all = rollup.as_array().map(Vec::as_slice).unwrap_or_default();
    let latest = |n: &String| {
        let runs = all.iter().filter(|c| check_name(c) == n);
        runs.max_by_key(|c| c["startedAt"].as_str().unwrap_or_default())
            .cloned()
    };
    required
        .iter()
        .map(|n| latest(n).unwrap_or_else(|| json!({"name": n, "status": "MISSING"})))
        .collect()
}

/// The first check that is not green, as "check `name` is state".
pub fn not_green(rollup: &Value) -> Option<String> {
    let c = rollup.as_array()?.iter().find(|c| check_state(c) != "green")?;
    Some(format!("check `{}` is {}", check_name(c), check_state(c)))
}

/// The rollup: failed if any check failed, pending while any runs (or none reported yet).
pub fn ci(rollup: &Value) -> &'static str {
    let st: Vec<_> = rollup.as_array().into_iter().flatten().map(check_state).collect();
    if st.contains(&"failed") {
        "failed"
    } else if st.is_empty() || st.iter().any(|s| *s != "green") {
        "pending"
    } else {
        "green"
    }
}

pub fn summary(pr: &Value, branch: &str, req: &[String]) -> String {
    if pr.is_null() {
        return format!("no PR for branch {branch}");
    }
    let head: String = pr["headRefOid"].as_str().unwrap_or("?").chars().take(7).collect();
    let n = pr["statusCheckRollup"].as_array().map_or(0, Vec::len);
    format!(
        "PR #{} {} → {} head {head} mergeable {} ci {} ({n} checks)",
        pr["number"],
        pr["state"].as_str().unwrap_or("?"),
        pr["baseRefName"].as_str().unwrap_or("?"),
        pr["mergeable"].as_str().unwrap_or("?"),
        ci(&required(&pr["statusCheckRollup"], req))
    )
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Until {
    CiDone,
    CiGreen,
    Merged,
}

/// Exit code once `until` is decided: 0 reached, 1 can no longer be reached.
fn decided(pr: &Value, until: Until, req: &[String]) -> Option<i32> {
    if pr.is_null() {
        return None;
    }
    let checks = required(&pr["statusCheckRollup"], req);
    let ci = ci(&checks);
    match until {
        Until::Merged => match pr["state"].as_str() {
            Some("MERGED") => Some(0),
            Some("CLOSED") => Some(1),
            _ => None,
        },
        Until::CiGreen | Until::CiDone if ci == "green" => Some(0),
        Until::CiGreen if ci == "failed" => Some(1),
        Until::CiDone if ci == "failed" => {
            let pending = checks
                .as_array()
                .into_iter()
                .flatten()
                .any(|c| check_state(c) == "pending");
            (!pending).then_some(1)
        }
        _ => None,
    }
}

/// `herdsman wait`: poll the cached read, 30 s doubling to 5 min; 0 reached, 1 failed/closed, 124 timed out.
#[allow(clippy::too_many_arguments)] // the CLI's wait arguments, one each
pub fn wait(
    ctx: &Ctx,
    r: &dyn Runner,
    out: &mut dyn Write,
    f: &Fleet,
    lane: (&str, u64),
    branch: &str,
    until: Until,
    timeout: u64,
) -> Result<i32> {
    let (mut t, mut delay) = (0, 30);
    loop {
        let pr = pr(ctx, r, f, lane, branch, ctx.now + t)?;
        if let Some(code) = decided(&pr, until, &f.required_checks) {
            writeln!(out, "{}", summary(&pr, branch, &f.required_checks))?;
            return Ok(code);
        }
        if t >= timeout {
            writeln!(out, "timeout after {t}s: {}", summary(&pr, branch, &f.required_checks))?;
            return Ok(124);
        }
        let s = delay.min(timeout - t);
        r.sleep(s);
        t += s;
        delay = (delay * 2).min(300);
    }
}

/// #388: an open PR head with no checks `ci_start_minutes` after its first event: CI broken (a jobless run) or missing.
pub fn missing(x: &Ctx, r: Option<&dyn Runner>, c: &Config, k: (&str, u64), e: &[Event], pr: &Value) -> Option<String> {
    let ((fleet, issue), g, f) = (k, c.limits.ci_start_minutes * 60, c.fleet(k.0).ok()?);
    let head = pr["headRefOid"].as_str().filter(|_| pr["state"] == "OPEN")?;
    let named = |e: &&Event| e.fleet == fleet && e.issue == Some(issue) && e.str("sha").or(e.str("to")) == Some(head);
    let since = e.iter().filter(named).map(|e| e.ts).min()?;
    let labelled = f.merge_label.as_deref().is_none_or(|l| has_label(pr, l));
    (labelled && x.now >= since + g && pr["statusCheckRollup"].as_array().is_none_or(Vec::is_empty)).then_some(())?;
    let p = cache_path(x, "runs", fleet, issue);
    let api = format!("repos/{}/actions/runs", f.repo);
    let c = load::<Value>(&p).filter(|c| c["head"] == head);
    let broken = match r.filter(|_| c.as_ref().is_none_or(|c| c["broken"].is_null() && !fresh(c, x.now))) {
        Some(r) => {
            let jq = ".workflow_runs[] | select(.conclusion==\"failure\") | \"\\(.id) \\(.path|split(\"/\")|last)\"";
            let runs = ok(r, "gh", &["api", &format!("{api}?head_sha={head}"), "--jq", jq]).unwrap_or_default();
            let zero = |id: &str| ok(r, "gh", &["api", &format!("{api}/{id}/jobs"), "--jq", ".total_count"]);
            let mut b = runs.lines().filter_map(|l| l.split_once(' '));
            let b = b.find(|(id, _)| zero(id).is_ok_and(|n| n.trim() == "0"));
            let _ = store(&p, &json!({"ts": x.now, "head": head, "broken": b.map(|b| b.1)}));
            b.map(|b| b.1.to_string())
        }
        None => c.and_then(|c| c["broken"].as_str().map(String::from)),
    };
    let missing = || format!("CI missing (no checks {} after push)", crate::events::age(g));
    Some(broken.map_or_else(missing, |w| format!("CI broken: {w} did not start")))
}
