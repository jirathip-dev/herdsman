//! #537 `herdsman runners`: the owner's `[ci_runners]` VMs and their GitHub runners (docs/hosts.md). Each verb is
//! logged `runners-<verb>`. The registration token goes to a VM on stdin (`cloud::run`), never into argv, the log or
//! the output; the provider key stays on this host.
use crate::config::{CiRunners, Config};
use crate::events::{age, emit, read, Event};
use crate::gate::refused;
use crate::runner::{argv, ok, Runner};
use crate::Ctx;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::io::Write;

/// How often `check` pushes a fresh registration token to every VM (GitHub's last an hour).
const TOKEN_EVERY: u64 = 30 * 60;

pub enum Verb {
    Status,
    Revive(String),
    Add(u64),
    Remove(String),
    Tokens,
    RetireLocal,
}

/// A GitHub runner: id, name, online, busy.
type Gh = (u64, String, bool, bool);

fn conf(cfg: &Config) -> Result<&CiRunners> {
    cfg.ci_runners.as_ref().context("no [ci_runners] in fleet.toml")
}

fn log(ctx: &Ctx, cfg: &Config, kind: &str, d: Value) -> Result<()> {
    emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, kind, d))
}

/// The VMs now: `vms`, then each `runners-add` and `runners-remove` in the log.
pub fn vms(c: &CiRunners, events: &[Event]) -> Vec<String> {
    let mut v = c.vms.clone();
    for e in events {
        match (e.kind.as_str(), e.str("vm")) {
            ("runners-add", Some(x)) => v.push(x.into()),
            ("runners-remove", Some(x)) => v.retain(|y| y != x),
            _ => {}
        }
    }
    v
}

/// The repo's GitHub runners labelled `label`.
fn github(r: &dyn Runner, c: &CiRunners, label: &str) -> Result<Vec<Gh>> {
    let v: Value = serde_json::from_str(&ok(
        r,
        "gh",
        &["api", &format!("repos/{}/actions/runners?per_page=100", c.repo)],
    )?)?;
    let has = |x: &Value| x["labels"].as_array().into_iter().flatten().any(|l| l["name"] == label);
    let row = |x: &Value| {
        let name = x["name"].as_str().unwrap_or_default().to_string();
        (
            x["id"].as_u64().unwrap_or(0),
            name,
            x["status"] == "online",
            x["busy"] == true,
        )
    };
    Ok(v["runners"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| has(x))
        .map(row)
        .collect())
}

fn deregister(r: &dyn Runner, c: &CiRunners, g: &Gh) -> Result<()> {
    ok(
        r,
        "gh",
        &[
            "api",
            "-X",
            "DELETE",
            &format!("repos/{}/actions/runners/{}", c.repo, g.0),
        ],
    )
    .map(drop)
}

/// A fresh registration token (held in memory only).
fn token(r: &dyn Runner, c: &CiRunners) -> Result<String> {
    let url = format!("repos/{}/actions/runners/registration-token", c.repo);
    let t = ok(r, "gh", &["api", "-X", "POST", &url, "--jq", ".token"])?
        .trim()
        .to_string();
    ensure!(!t.is_empty(), "GitHub gave no registration token");
    Ok(t)
}

/// Recipe command `key` for VM `vm`: `runner_create|state|delete` run on the provider, the others (`runner_revive`,
/// `runner_token`, `runner_drain`, `runner_names`) on the VM through `runner_exec` (as `$C`, `{rt}` the token).
fn rc(ctx: &Ctx, r: &dyn Runner, c: &CiRunners, key: &str, vm: &str, token: Option<&str>) -> Result<String> {
    let rc = crate::cloud::load(ctx, &c.provider)?;
    let s = |k: &str| {
        let v = rc.get(k).and_then(|v| v.as_str());
        v.map(str::to_string)
            .with_context(|| format!("recipe {} has no {k}", c.provider))
    };
    let fill = |l: String| {
        let ttl = (c.ttl_hours * 3600).to_string();
        let l = l.replace("{vm}", vm).replace("{size}", &c.size).replace("{ttl}", &ttl);
        let l = l.replace("{snapshots}", &c.snapshots.to_string());
        l.replace("{service}", &c.service)
    };
    let line = match key {
        "runner_create" | "runner_state" | "runner_delete" => fill(s(key)?),
        _ => format!(
            "export C='{}'; {}",
            fill(s(key)?).replace('\'', "'\\''"),
            fill(s("runner_exec")?)
        ),
    };
    crate::cloud::run(r, &rc, &format!("{vm} {key}"), &line, (token, None))
}

/// One VM's cost a day: the recipe's `rate_usd_hour` for `size`, × 24.
fn per_day(ctx: &Ctx, c: &CiRunners) -> Result<f64> {
    let rc = crate::cloud::load(ctx, &c.provider)?;
    let rate = rc.get("rate_usd_hour").and_then(|t| t.get(&c.size)?.as_float());
    Ok(rate.with_context(|| format!("no rate_usd_hour for size {}", c.size))? * 24.0)
}

/// The owner said yes to ask `id` about `runners <verb>`: an owner ask whose first option the owner (not a conductor, not
/// the default) chose, used by no verb yet (one yes lets one run on).
fn approved(events: &[Event], id: Option<&str>, verb: &str) -> bool {
    let Some(id) = id else { return false };
    let ask = events
        .iter()
        .find(|e| e.kind == "needs-decision" && e.str("id") == Some(id));
    let ask = ask.filter(|a| a.str("kind") == Some("owner"));
    let ask = ask.filter(|a| {
        a.str("question")
            .is_some_and(|q| q.contains(&format!("runners {verb}")))
    });
    let yes = ask.and_then(|a| a.data["options"].get(0)?.as_str());
    let by = |e: &&Event| {
        e.str("by")
            .is_some_and(|b| b != "default" && !b.starts_with("conductor-"))
    };
    let decision = events
        .iter()
        .rev()
        .find(|e| e.kind == "decision" && e.str("id") == Some(id));
    let used = events
        .iter()
        .any(|e| e.kind.starts_with("runners-") && e.str("approved") == Some(id));
    !used && yes.is_some() && decision.filter(by).and_then(|d| d.str("choice")) == yes
}

fn ask_owner<T>(code: &'static str, why: &str, verb: &str) -> Result<T> {
    refused(
        code,
        format!(
            "{why}: ask the owner (`herdsman ask <fleet> \"herdsman runners {verb}: ok?\" --option yes --option no \
             --kind owner`), then run it again with --approved <id>"
        ),
    )
}

/// The VM runners online and how many there should be (`slots × VMs`).
pub fn health(r: &dyn Runner, c: &CiRunners, events: &[Event]) -> Result<(u64, u64)> {
    let on = github(r, c, &c.label)?.iter().filter(|g| g.2).count();
    Ok((on as u64, c.slots * vms(c, events).len() as u64))
}

/// Push a fresh token to every VM; the VMs it reached.
fn push_tokens(ctx: &Ctx, cfg: &Config, r: &dyn Runner, c: &CiRunners) -> Result<usize> {
    let (t, all) = (token(r, c)?, vms(c, &read(&ctx.state)?));
    let failed: Vec<_> = all
        .iter()
        .filter(|vm| rc(ctx, r, c, "runner_token", vm, Some(&t)).is_err())
        .collect();
    log(
        ctx,
        cfg,
        "runners-tokens",
        json!({"vms": all.len() - failed.len(), "failed": failed}),
    )?;
    Ok(all.len() - failed.len())
}

/// `check`: a fresh token every `TOKEN_EVERY`, then the health (`runners-low` / `runners-ok` are check's).
pub fn tick(ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event]) -> Option<Result<(u64, u64)>> {
    let c = cfg.ci_runners.as_ref()?;
    let last = events
        .iter()
        .rev()
        .find(|e| e.kind == "runners-tokens")
        .map_or(0, |e| e.ts);
    if last + TOKEN_EVERY <= ctx.now {
        if let Err(e) = push_tokens(ctx, cfg, r, c) {
            eprintln!("herdsman check: runner tokens: {e:#}");
        }
    }
    Some(health(r, c, events))
}

/// `herdsman runners <verb>`; `ok_id` an owner ask that approved going past a gate.
pub fn run(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    verb: Verb,
    ok_id: Option<&str>,
) -> Result<()> {
    let c = conf(cfg)?;
    let events = read(&ctx.state)?;
    let all = vms(c, &events);
    match verb {
        Verb::Status => {
            let gh = github(r, c, &c.label)?;
            let line = format!(
                "{}/{} online",
                gh.iter().filter(|g| g.2).count(),
                c.slots * all.len() as u64
            );
            writeln!(
                out,
                "runners: {line} ({} VMs × {} slots, label {})",
                all.len(),
                c.slots,
                c.label
            )?;
            for vm in &all {
                let st = rc(ctx, r, c, "runner_state", vm, None);
                writeln!(
                    out,
                    "vm {vm}: {}",
                    st.map_or_else(|e| format!("unknown ({e:#})"), |s| s.trim().into())
                )?;
            }
            let local = c.local_label.as_deref().map(|l| github(r, c, l)).transpose()?;
            for g in gh.iter().chain(local.iter().flatten()) {
                let busy = if g.3 { " busy" } else { "" };
                writeln!(out, "runner {}: {}{busy}", g.1, ["offline", "online"][usize::from(g.2)])?;
            }
            log(ctx, cfg, "runners-status", json!({"health": line}))
        }
        Verb::Revive(vm) => {
            ensure!(
                all.contains(&vm),
                "{vm} is no [ci_runners] VM (`runners status` lists them)"
            );
            rc(ctx, r, c, "runner_revive", &vm, Some(&token(r, c)?))?;
            log(ctx, cfg, "runners-revive", json!({"vm": vm}))?;
            Ok(writeln!(
                out,
                "{vm}: fresh registration token, runner service restarted"
            )?)
        }
        Verb::Add(n) => {
            let (total, per) = (all.len() as u64 + n, per_day(ctx, c)?);
            let usd = f64::from(u32::try_from(total).unwrap_or(u32::MAX)) * per;
            if (total > c.max_vms || usd > c.usd_day) && !approved(&events, ok_id, "add") {
                let why = format!(
                    "{total} VMs at ${usd:.2}/day is past max_vms {} / usd_day {}",
                    c.max_vms, c.usd_day
                );
                return ask_owner("RUNNERS_CAP", &why, &format!("add {n}"));
            }
            for _ in 0..n {
                let id = rc(ctx, r, c, "runner_create", "", None)?;
                let id = id.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default();
                ensure!(
                    !id.is_empty() && !id.contains(char::is_whitespace),
                    "runner_create printed no VM id"
                );
                log(
                    ctx,
                    cfg,
                    "runners-add",
                    json!({"vm": id, "usd_day": per, "approved": ok_id}),
                )?;
                rc(ctx, r, c, "runner_revive", id, Some(&token(r, c)?))?;
                writeln!(out, "{id}: created ({}), {} runners starting", c.size, c.slots)?;
            }
            Ok(())
        }
        Verb::Remove(target) if all.contains(&target) => {
            let prefix = rc(ctx, r, c, "runner_names", &target, None)?.trim().to_string();
            ensure!(!prefix.is_empty(), "runner_names printed no name prefix for {target}");
            let mine = |g: &&Gh| g.1.starts_with(&prefix);
            let others = github(r, c, &c.label)?.iter().filter(|g| g.2 && !mine(g)).count();
            if others == 0 && !approved(&events, ok_id, "remove") {
                let why = format!("{target} is the last VM with online runners");
                return ask_owner("RUNNERS_LAST", &why, &format!("remove {target}"));
            }
            rc(ctx, r, c, "runner_drain", &target, None)?;
            let mut idle = None;
            for _ in 0..60 {
                let gh = github(r, c, &c.label)?;
                if !gh.iter().filter(mine).any(|g| g.3) {
                    idle = Some(gh);
                    break;
                }
                r.sleep(30);
            }
            let Some(gh) = idle else {
                bail!("{target}: a job still runs after 30 min; drained, not deleted")
            };
            let gone: Vec<_> = gh.iter().filter(mine).collect();
            gone.iter().try_for_each(|g| deregister(r, c, g))?;
            rc(ctx, r, c, "runner_delete", &target, None)?;
            log(
                ctx,
                cfg,
                "runners-remove",
                json!({"vm": target, "runners": gone.len(), "approved": ok_id}),
            )?;
            Ok(writeln!(
                out,
                "{target}: drained, {} runner(s) deregistered, deleted",
                gone.len()
            )?)
        }
        Verb::Remove(name) => {
            let local = c.local_label.as_deref().map(|l| github(r, c, l)).transpose()?;
            let gh = github(r, c, &c.label)?.into_iter().chain(local.into_iter().flatten());
            let g = gh.into_iter().find(|g| g.1 == name);
            let g = g.with_context(|| format!("{name} is no [ci_runners] VM nor runner"))?;
            ensure!(!g.3, "{name} is running a job; remove it once idle");
            deregister(r, c, &g)?;
            log(ctx, cfg, "runners-remove", json!({"runner": name}))?;
            Ok(writeln!(out, "{name}: deregistered")?)
        }
        Verb::Tokens => {
            let n = push_tokens(ctx, cfg, r, c)?;
            let last = events.iter().rev().find(|e| e.kind == "runners-tokens");
            let ago = last.map_or("never".into(), |e| format!("{} ago", age(ctx.now - e.ts)));
            Ok(writeln!(
                out,
                "tokens: a fresh one on {n}/{} VMs; the refresher is `herdsman check` (the 5-minute timer), every {} \
                 min (before this: {ago})",
                all.len(),
                TOKEN_EVERY / 60
            )?)
        }
        Verb::RetireLocal => {
            let label = c.local_label.as_deref().context("[ci_runners] has no local_label")?;
            let (on, want) = health(r, c, &events)?;
            if on < want && !approved(&events, ok_id, "retire-local") {
                return ask_owner(
                    "RUNNERS_LOW",
                    &format!("VM runners are {on}/{want} online"),
                    "retire-local",
                );
            }
            if let Some(s) = &c.local_stop {
                let o = r.run("sh", &argv(&["-c", s]))?;
                ensure!(o.code == 0, "local_stop failed ({}): {}", o.code, o.stderr.trim());
            }
            let (busy, idle): (Vec<_>, Vec<_>) = github(r, c, label)?.into_iter().partition(|g| g.3);
            idle.iter().try_for_each(|g| deregister(r, c, g))?;
            let d = json!({"deregistered": idle.len(), "busy": busy.len(), "approved": ok_id});
            log(ctx, cfg, "runners-retire-local", d)?;
            Ok(writeln!(
                out,
                "local runners stopped: {} deregistered, {} still busy",
                idle.len(),
                busy.len()
            )?)
        }
    }
}
