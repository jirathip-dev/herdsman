//! #254 cloud hosts (docs/hosts.md): recipe commands start, stop and price a `[hosts.<n>] cloud` host; logs `cloud-*`.
use crate::config::{write_atomic, Config};
use crate::events::{age, emit, read, Event};
use crate::runner::{argv, ok, Runner};
use crate::Ctx;
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;

/// Host `name`'s recipe (`<config>/recipes/<cloud>.toml`, else the built-in `boat`) and size: #635's old `medium` is
/// `default` where the recipe prices no `medium`.
pub fn recipe(ctx: &Ctx, cfg: &Config, name: &str) -> Result<(toml::Table, String)> {
    let (c, size) = (&cfg.hosts[name].cloud, cfg.hosts[name].size.clone().unwrap_or_default());
    let rc = load(ctx, c.as_deref().unwrap_or(""))?;
    let rates = rc.get("rate_usd_hour").and_then(|t| t.as_table());
    let old = size == "medium" && rates.is_some_and(|t| !t.contains_key("medium") && t.contains_key("default"));
    Ok((rc, if old { "default".into() } else { size }))
}

/// #635: the one fleet that fits host `name` (its tags), whose store scope its cloud key is read from first.
pub fn fleet<'a>(ctx: &Ctx, cfg: &'a Config, name: &str) -> Option<&'a str> {
    let tags = crate::hosts::tags(&cfg.hosts[name], crate::hosts::cached(ctx).get(name));
    let mut fits = cfg.fleet.iter().filter(|f| f.1.needs.iter().all(|t| tags.contains(t)));
    fits.next().filter(|_| fits.next().is_none()).map(|f| f.0.as_str())
}

/// The provider key `var` and where it was found: the environment, else the store's `herdsman/<fleet>/<var>`, else
/// `herdsman/all/<var>`.
pub fn key(r: &dyn Runner, var: &str, fleet: Option<&str>) -> Option<(String, String)> {
    let env = std::env::var(var).ok().map(|k| (k, "env".into()));
    let stored = |f: &str| {
        let svc = crate::secret::service(f, var);
        r.secret(&svc).ok().flatten().map(|k| (k, svc))
    };
    let found = env.or_else(|| fleet.and_then(stored)).or_else(|| stored("all"));
    found.filter(|k| !k.0.is_empty())
}

/// Recipe `c`: `<config>/recipes/<c>.toml`, else the built-in `boat`.
pub fn load(ctx: &Ctx, c: &str) -> Result<toml::Table> {
    let p = ctx.config.with_file_name(format!("recipes/{c}.toml"));
    let boat = || (c == "boat").then(|| include_str!("../assets/recipes/boat.toml").into());
    let text = std::fs::read_to_string(&p).ok().or_else(boat);
    let text = text.with_context(|| format!("cloud recipe {}", p.display()))?;
    Ok(toml::from_str(&text)?)
}

/// Run host `name`'s recipe command `verb`, the provider key exported from stdin (never in argv or a log); its stdout.
fn cmd(ctx: &Ctx, cfg: &Config, r: &dyn Runner, name: &str, verb: &str) -> Result<String> {
    let (rc, size) = recipe(ctx, cfg, name)?;
    let line = rc.get(verb).and_then(|v| v.as_str()).unwrap_or_default();
    let line = line.replace("{name}", name).replace("{size}", &size);
    run(r, &rc, &format!("{name} {verb}"), &line, (None, fleet(ctx, cfg, name)))
}

/// Run recipe `rc`'s filled-in `line`: its provider key (#635: `fleet`'s first), then #537's `token` (as `RT`),
/// exported from stdin.
type Scope<'a> = (Option<&'a str>, Option<&'a str>);
pub fn run(r: &dyn Runner, rc: &toml::Table, what: &str, line: &str, (token, fleet): Scope) -> Result<String> {
    let var = rc.get("key_env").and_then(|v| v.as_str()).unwrap_or_default();
    let f = fleet.map_or(String::new(), |f| format!("`{f}` or "));
    let (key, _) =
        key(r, var, fleet).with_context(|| format!("{var} not set (env or the store's {f}`all` secret {var})"))?;
    let rt = token.map_or("", |_| "IFS= read -r RT; export RT; ");
    let script = format!("IFS= read -r K; export {var}=\"$K\"; unset K; {rt}{line}");
    let o = r.run_in(
        "sh",
        &argv(&["-c", &script]),
        &format!("{key}\n{}", token.map_or(String::new(), |t| format!("{t}\n"))),
    )?;
    let mut err = o.stderr.trim().replace(&key, "•••");
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        err = err.replace(t, "•••");
    }
    ensure!(o.code == 0, "{what} failed ({}): {err}", o.code);
    Ok(o.stdout)
}

fn log(ctx: &Ctx, cfg: &Config, kind: &str, name: &str, mut d: Value) -> Result<()> {
    d["host"] = json!(name);
    emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, kind, d))
}

fn last<'a>(events: &'a [Event], name: &str, kinds: &[&str]) -> Option<&'a Event> {
    let hit = |e: &&Event| kinds.contains(&e.kind.as_str()) && e.str("host") == Some(name);
    events.iter().rev().find(hit)
}

/// running | starting | stopped, from the last start, up or stop.
pub fn state(events: &[Event], name: &str) -> &'static str {
    match last(events, name, &["cloud-start", "cloud-up", "cloud-stop"]).map(|e| e.kind.as_str()) {
        Some("cloud-start") => "starting",
        Some("cloud-up") => "running",
        _ => "stopped",
    }
}

/// Host `name`'s USD this UTC month and hours today: its `cloud-start` → `cloud-stop` spans × `rate_usd_hour`.
pub fn spend(ctx: &Ctx, cfg: &Config, events: &[Event], name: &str) -> Result<(f64, f64)> {
    let (rc, size) = recipe(ctx, cfg, name)?;
    let rate = rc.get("rate_usd_hour").and_then(|t| t.get(&size)?.as_float());
    let rate = rate.with_context(|| format!("no rate_usd_hour for size {size}"))?;
    // The 0-based day of the month, from the civil calendar of the days since 1970 (H. Hinnant).
    let doe = (ctx.now / 86_400 + 719_468) % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let (today, dom) = (ctx.now - ctx.now % 86_400, doy - (153 * ((5 * doy + 2) / 153) + 2) / 5);
    let hours = |from: u64| {
        let (mut on, mut t) = (None, 0);
        for e in events.iter().filter(|e| e.str("host") == Some(name)) {
            match e.kind.as_str() {
                "cloud-start" => on = on.or(Some(e.ts)),
                "cloud-stop" => t += on.take().map_or(0, |s: u64| e.ts.saturating_sub(s.max(from))),
                _ => {}
            }
        }
        let t = t + on.map_or(0, |s| ctx.now.saturating_sub(s.max(from)));
        f64::from(u32::try_from(t).unwrap_or(u32::MAX)) / 3600.0
    };
    Ok((hours(today - dom * 86_400) * rate, hours(today)))
}

/// Why host `name` may not run now: its month's budget (none set: 0) or today's `max_hours_day` used up.
pub fn over(ctx: &Ctx, cfg: &Config, events: &[Event], name: &str) -> Option<String> {
    let h = &cfg.hosts[name];
    let (b, m) = (h.budget_usd_month.unwrap_or(0.0), h.max_hours_day);
    let month = |usd: f64| (usd >= b).then(|| format!("spent ${usd:.2} of ${b} this month"));
    let day = |hrs: f64| m.filter(|m| hrs >= *m).map(|m| format!("{hrs:.1} h today of {m}"));
    spend(ctx, cfg, events, name).map_or_else(|e| Some(format!("{e:#}")), |(u, h)| month(u).or_else(|| day(h)))
}

/// What keeps placement off cloud host `name` as it is: `CLOUD_BUDGET` at a limit, else `CLOUD_STOPPED` (startable).
pub fn held(ctx: &Ctx, cfg: &Config, name: &str) -> Option<(&'static str, String)> {
    cfg.hosts.get(name)?.cloud.as_ref()?;
    let events = read(&ctx.state).unwrap_or_default();
    let stopped = || (state(&events, name) == "stopped").then(|| ("CLOUD_STOPPED", "stopped".into()));
    let budget = over(ctx, cfg, &events, name).map(|w| ("CLOUD_BUDGET", w));
    budget.or_else(stopped)
}

/// Start host `name`, wait (5 min at most) for `status` to print `running`, then refresh its address; a failure stops it.
pub fn start(ctx: &Ctx, cfg: &Config, r: &dyn Runner, name: &str) -> Result<()> {
    log(ctx, cfg, "cloud-start", name, json!({}))?;
    let made = |v: &str| {
        // #635: a new sandbox has a new host key: its old pin goes.
        let _ = std::fs::remove_file(pin(ctx, name));
        cmd(ctx, cfg, r, name, v)
    };
    let snap = last(&read(&ctx.state)?, name, &["cloud-snapshot"]).is_some();
    let up = cmd(ctx, cfg, r, name, "status").and_then(|s| match s.trim() {
        "" if snap && recipe(ctx, cfg, name)?.0.contains_key("create_from") => made("create_from"),
        "" => made("create"),
        _ => cmd(ctx, cfg, r, name, "start"),
    });
    let up = up.and_then(|_| {
        for _ in 0..30 {
            if cmd(ctx, cfg, r, name, "status")?.trim() == "running" {
                return refresh(ctx, cfg, r, name).map(|()| drop(crate::hosts::survey(ctx, cfg, r, true)));
            }
            r.sleep(10);
        }
        bail!("{name} not running after 5 min")
    });
    if let Err(e) = &up {
        // Logged stopped only once `stop` worked: else it stays `starting`, its VM maybe still billing.
        cmd(ctx, cfg, r, name, "stop").with_context(|| format!("start failed ({e:#}), then stop failed"))?;
        let d = json!({"why": format!("start failed: {e:#}")});
        log(ctx, cfg, "cloud-stop", name, d)?;
    }
    up
}

/// #635: host `name`'s pinned host key, outside `config.d` (an `Include config.d/*` would read it as config).
fn pin(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    ctx.home.join(format!(".ssh/herdsman/{name}.known"))
}

/// Rewrite host `name`'s block in `~/.ssh/config.d/herdsman` and its `known_hosts` from `address` (a key other than
/// the pinned one fails closed); logs `cloud-up`.
fn refresh(ctx: &Ctx, cfg: &Config, r: &dyn Runner, name: &str) -> Result<()> {
    let out = cmd(ctx, cfg, r, name, "address")?;
    let bad = || format!("{name} address: not `user@host port` + key");
    let mut l = out.lines().map(str::trim);
    let (dest, port) = l.next().and_then(|a| a.split_once(' ')).with_context(bad)?;
    let ((user, host), key) = (dest.split_once('@').with_context(bad)?, l.next().with_context(bad)?);
    // The reply goes into ssh config verbatim: a numeric port and no whitespace, so it can't add a directive.
    ensure!(
        port.parse::<u16>().is_ok() && !dest.contains(char::is_whitespace),
        bad()
    );
    let (dir, known) = (ctx.home.join(".ssh/config.d"), pin(ctx, name));
    let at = [format!("[{host}]:{port}"), host.into()];
    let was = std::fs::read_to_string(&known).unwrap_or_default();
    let was = was.split_once(' ').map_or("", |w| w.1.trim());
    let k = known.display();
    ensure!(
        was.is_empty() || was == key,
        "{name}: host key changed from the one pinned in {k} (rm it to trust the new one)"
    );
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(known.parent().context("pin dir")?)?;
    write_atomic(&known, &format!("{} {key}\n", at[usize::from(port == "22")]))?;
    let _ = std::fs::remove_file(dir.join(format!("herdsman-{name}.known"))); // the pre-#635 place
    let (a, b) = (format!("# herdsman {name}\n"), format!("# end {name}\n"));
    let m = cfg.hosts[name].machine.as_deref().unwrap_or(name);
    let s = format!("{a}Host {m}\n  HostName {host}\n  User {user}\n  Port {port}\n  UserKnownHostsFile {k}\n{b}");
    let p = dir.join("herdsman");
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    let (keep, tail) = old.split_once(&a).unwrap_or((&old, ""));
    // A block whose end marker is lost ends at the next block, never swallowing it.
    let next = || tail.find("# herdsman ").map_or("", |i| &tail[i..]);
    let rest = keep.to_string() + tail.split_once(&b).map_or_else(next, |x| x.1);
    write_atomic(&p, &(rest + &s))?;
    log(ctx, cfg, "cloud-up", name, json!({"address": format!("{dest} {port}")}))
}

/// Save host `name`'s lane work to `wip/<host>-<lane>` (a failed push or unreachable host: `cloud-abort`), then `verb`.
pub fn stop(ctx: &Ctx, cfg: &Config, r: &dyn Runner, name: &str, verb: &str, why: &str) -> Result<Vec<String>> {
    let (events, mut wip) = (read(&ctx.state)?, vec![]);
    let on = crate::hosts::On::new(r, cfg.hosts.get(name).context("no such host")?.machine.as_deref());
    let mut lanes = BTreeMap::new();
    let mine = |e: &&Event| e.kind == "spawn" && e.str("host") == Some(name);
    for e in events.iter().filter(mine) {
        lanes.extend(e.str("agent").zip(e.str("worktree")));
    }
    for (lane, wt) in lanes {
        let w = format!("wip/{name}-{lane}");
        let saved = match on.run("test", &argv(&["-e", wt])).map(|o| o.code) {
            Ok(c) if c == 1 || c == 0 && crate::spawn::dirty(&on, wt.as_ref()).is_none() => continue,
            Ok(0) => crate::check::save_wip(&on, wt, &w, &format!("wip: cloud host {verb}")),
            o => o.and_then(|c| Err(anyhow!("can't reach {name} to check {wt} (exit {c})"))),
        };
        if let Err(e) = saved {
            let d = json!({"lane": lane, "why": format!("{e:#}")});
            log(ctx, cfg, "cloud-abort", name, d)?;
            return Err(e.context(format!("{name}: {verb} aborted, {lane}'s work not saved")));
        }
        wip.push(w);
    }
    cmd(ctx, cfg, r, name, verb)?;
    let d = json!({"verb": verb, "why": why, "wip": wip});
    log(ctx, cfg, "cloud-stop", name, d)?;
    Ok(wip)
}

/// `check`: stop each running cloud host at its spend limit, or once it has had no live agent for `idle_stop_minutes`.
pub type Live = BTreeMap<String, Option<Vec<Value>>>;
pub fn tick(ctx: &Ctx, cfg: &Config, r: &dyn Runner, live: &Live, out: &mut dyn std::io::Write) -> Result<()> {
    let (events, seen) = (read(&ctx.state).unwrap_or_default(), crate::hosts::cached(ctx));
    for (name, h) in cfg.hosts.iter().filter(|h| h.1.cloud.is_some()) {
        let Some(Some(agents)) = live.get(name).filter(|_| state(&events, name) == "running") else {
            continue;
        };
        let idle = h.idle_stop_minutes.unwrap_or(20) * 60;
        let up = last(&events, name, &["cloud-up"]);
        let busy = seen.get(name).map_or(0, |s| s.busy).max(up.map_or(0, |e| e.ts));
        let quiet = (agents.is_empty() && busy + idle <= ctx.now).then(|| format!("idle {} min", idle / 60));
        if let Some(why) = over(ctx, cfg, &events, name).or(quiet) {
            let res = stop(ctx, cfg, r, name, "stop", &why);
            let said = res.map_or_else(|e| format!("{e:#}"), |w| format!("{name}: stopped ({why}); wip {w:?}"));
            writeln!(out, "cloud {said}")?;
        }
    }
    Ok(())
}

/// Host card: head words (none when running); the `cloud` row's tone (amber from 80 %, red at a limit) and text.
pub fn card(ctx: &Ctx, cfg: &Config, ev: &[Event], (name, n): (&str, usize)) -> Option<[String; 3]> {
    let h = cfg.hosts.get(name).filter(|h| h.cloud.is_some())?;
    let st = state(ev, name);
    let paused = format!("stopped · {n} lane(s) paused: cloud host stopped");
    let head = [st.replace("running", ""), paused][usize::from(st == "stopped")].clone();
    let (b, m) = (h.budget_usd_month.unwrap_or(0.0), h.max_hours_day.unwrap_or(24.0));
    let (usd, hrs) = spend(ctx, cfg, ev, name).unwrap_or((f64::INFINITY, 0.0));
    let at = usize::from((usd / b).max(hrs / m) >= 0.8) + usize::from(over(ctx, cfg, ev, name).is_some());
    let ago = |k| last(ev, name, &[k]).map_or("never".into(), |e| format!("{} ago", age(ctx.now - e.ts)));
    let (rc, size) = (h.cloud.as_deref().unwrap_or_default(), h.size.as_deref().unwrap_or("?"));
    let (a, s, t) = (ago("cloud-up"), ago("cloud-start"), ago("cloud-stop"));
    let text =
        format!("{rc} {size} · spent ${usd:.0} of ${b} · {hrs:.1} h today of {m} · address {a} · last start {s}");
    let tone = ["g", "a", "r"][at.min(2)].to_string();
    let gh = crate::hosts::cached(ctx).remove(name).map(|s| s.github);
    let gh = gh.filter(|g| !g.is_empty()).map(|g| format!(" · {g}"));
    Some([head, tone, format!("{text} · last stop {t}{}", gh.unwrap_or_default())])
}

/// #255: host `on`'s gh token as its card says it (`github: token · 2 repos · expires in 80d`) and a health row from 14
/// days before it expires, from one `gh api -i` there (the expiry header and the repos it reaches, never the token).
pub fn github(on: &dyn Runner, now: u64) -> (String, Option<String>) {
    let o = ok(on, "gh", &["api", "-i", "user/repos?per_page=100"]);
    token(&o.unwrap_or_default(), now)
}

/// `gh api -i` text's card words, and its row: `!` within 14 days of the expiry, `✗` when gh can't use it (expired).
pub fn token(text: &str, now: u64) -> (String, Option<String>) {
    let repos = format!("github: token · {} repos", text.matches("\"full_name\"").count());
    let exp = |l: &&str| l.to_lowercase().starts_with("github-authentication-token-expiration:");
    let day = text.lines().find(exp).and_then(|l| l.get(39..)?.trim().get(..10));
    let n: Vec<i64> = day.unwrap_or_default().split('-').flat_map(str::parse).collect();
    let new = "→ a new fine-grained token, then hosts bootstrap";
    let [y, m, d] = n[..] else {
        let bad = format!("✗ github — gh there has no working token (expired?) {new}");
        return [
            (format!("{repos} · no expiry"), None),
            ("github: unreadable".into(), Some(bad)),
        ][usize::from(text.is_empty())]
        .clone();
    };
    // The days since 1970 of that civil date (H. Hinnant's days_from_civil).
    let (y, mp) = (y - i64::from(m <= 2), (m + 9) % 12);
    let (era, yoe) = (y.div_euclid(400), y.rem_euclid(400));
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + d - 1;
    let left = era * 146_097 + doe - 719_468 - i64::try_from(now / 86_400).unwrap_or(0);
    let row = format!("! github — token expires in {left}d {new}");
    (format!("{repos} · expires in {left}d"), (left <= 14).then_some(row))
}

/// Before each bootstrap step there: the user's tool folders on PATH, and `apt_ <pkg>` installs a package.
pub const SH: &str = "export PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:$PATH\"; apt_() { sudo apt-get update -qq && \
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \"$@\"; }; ";
/// Bootstrap's steps after herdr and each harness's install, `name⇥read-back check⇥install` each (Rust with a C linker
/// for `cargo install`).
const TOOLS: &str = "git\tcommand -v git\tapt_ git\ngh\tcommand -v gh\tapt_ gh";
const RUST: &str =
    "rust\tcommand -v cargo && command -v cc\tapt_ build-essential && curl -sSf https://sh.rustup.rs | sh -s -- -y";

/// #255 `hosts bootstrap <name>`: over SSH, each missing tool installed and read back (a present one skipped, so it runs
/// again safely); the logins opened in herdr panes there for the owner; then git over HTTPS with that token and the
/// clones of the fleets it fits. #592: no fleet.toml there. Nothing is copied from this host's logins.
pub fn bootstrap(ctx: &Ctx, cfg: &Config, r: &dyn Runner, name: &str, out: &mut dyn Write) -> Result<()> {
    let h = cfg.hosts.get(name).context("no such host")?;
    if h.cloud.is_some() && state(&read(&ctx.state)?, name) != "running" {
        start(ctx, cfg, r, name)?;
    }
    let m = h.machine.as_deref().context("the main host needs no bootstrap")?;
    let on = crate::hosts::On::new(r, Some(m));
    let (tags, home) = crate::hosts::probe(&on).with_context(|| format!("can't reach {name}"))?;
    let mine = crate::config::home().to_string_lossy().into_owned();
    let [exe, conf] = [&ctx.exe, &ctx.config.display().to_string()].map(|p| p.replace(&mine, &home));
    let (root, sha) = (exe.rsplitn(3, '/').last().unwrap_or(&home), env!("HERDSMAN_SHA"));
    let v = crate::timer::herdr_versions(r).context("no herdr server here")?.0;
    let sh = |s: &str| on.run("sh", &argv(&["-c", &format!("{SH}{s}")]));
    let pass = |s: &str| sh(s).is_ok_and(|o| o.code == 0);
    let mut step = |line: &str| -> Result<()> {
        let [what, check, install] =
            <[&str; 3]>::try_from(line.splitn(3, '\t').collect::<Vec<_>>()).unwrap_or_default();
        if pass(check) {
            return Ok(writeln!(out, "✓ {what} — there")?);
        }
        // herdr's install is `herdr machine add` from here: this host's version there.
        let add = || r.run(crate::herdr::BIN, &argv(&["machine", "add", m, "--label", m]));
        let o = if install.is_empty() { add()? } else { sh(install)? };
        let err = o.stderr.lines().last().unwrap_or_default().to_string();
        ensure!(pass(check), "{name}: {what} installed, `{check}` fails: {err}");
        Ok(writeln!(out, "✓ {what} — installed")?)
    };
    let (repo, version) = (env!("CARGO_PKG_REPOSITORY"), crate::VERSION);
    let build = format!("cargo install --git {repo} --rev {sha} --locked --root '{root}' --force");
    let mut steps = format!("herdr\therdr --version | grep -qx 'herdr {v}'\t\n{TOOLS}\n");
    for (n, i) in crate::harness::all().filter_map(|(n, h)| Some((n, h.install()?))) {
        steps += &format!("{n}\tcommand -v {n}\t{i}\n");
    }
    steps += &format!("{RUST}\n");
    steps += &format!("herdsman\t[ \"$('{exe}' --version)\" = 'herdsman {version}' ]\t{build}");
    steps.lines().try_for_each(&mut step)?;
    let harness = crate::harness::all().filter_map(|(n, h)| Some((h.health_check()?.1, n)));
    let logins = harness.chain([("gh auth status", "gh-token")]).filter(|t| !pass(t.0));
    let panes = logins
        .map(|t| crate::setup::start(ctx, cfg, r, name, t.1, "bootstrap"))
        .collect::<Result<Vec<_>>>()?;
    let p = panes.join(" ");
    ensure!(
        p.is_empty(),
        "{name}: log in in pane {p} there (herdr or the board), then bootstrap again"
    );
    step("git over https\tgit config --global --get credential.https://github.com.helper\tgh auth setup-git")?;
    let tags = [h.tags.clone(), tags].concat();
    for f in cfg.fleet.values().filter(|f| f.needs.iter().all(|t| tags.contains(t))) {
        let l = crate::hosts::under(&f.local, Some(&home));
        step(&format!(
            "clone {0}\ttest -d '{1}/.git'\tgit clone https://github.com/{0}.git '{1}'",
            f.repo,
            l.display()
        ))?;
    }
    // #592: no fleet.toml there (spawn syncs each lane's profile); an old one still gets its sync.
    step(&format!(
        "profiles sync\ttest ! -e '{conf}' || '{exe}' profiles sync >/dev/null\ttrue"
    ))?;
    log(ctx, cfg, "host-bootstrap", name, json!({"sha": sha}))?;
    crate::hosts::survey(ctx, cfg, r, true);
    Ok(writeln!(out, "{name}: bootstrapped (next: doctor, snapshot)")?)
}

/// #255 `hosts snapshot <name>`: after a fresh health look, up with no `✗`, the recipe's `snapshot`; logs `cloud-snapshot`.
pub fn snapshot(ctx: &Ctx, cfg: &Config, r: &dyn Runner, name: &str) -> Result<()> {
    crate::hosts::survey(ctx, cfg, r, true);
    let s = crate::hosts::cached(ctx).remove(name).unwrap_or_default();
    let bad: Vec<_> = s.health.iter().filter(|w| w.starts_with('✗')).cloned().collect();
    ensure!(s.up && bad.is_empty(), "{name} is not healthy: {}", bad.join("; "));
    cmd(ctx, cfg, r, name, "snapshot")?;
    log(ctx, cfg, "cloud-snapshot", name, json!({}))
}

/// #635 doctor rows for cloud host `name`: an old size, where its provider key is, and each recipe subcommand of its
/// CLI (the recipe's name, e.g. `boat`) in that CLI's `--help`.
pub fn doctor(ctx: &Ctx, cfg: &Config, r: &dyn Runner, name: &str, rows: &mut Vec<String>) -> Result<()> {
    let (rc, size) = recipe(ctx, cfg, name)?;
    let c = cfg.hosts[name].cloud.clone().unwrap_or_default();
    if cfg.hosts[name].size.as_deref() != Some(&size) {
        let old = cfg.hosts[name].size.clone().unwrap_or_default();
        rows.push(format!(
            "! cloud {name} size — \"{old}\" is {c}'s \"{size}\" → set size = \"{size}\""
        ));
    }
    let var = rc.get("key_env").and_then(|v| v.as_str()).unwrap_or_default();
    let f = fleet(ctx, cfg, name);
    rows.push(key(r, var, f).map_or_else(
        || format!("✗ cloud {name} key — {var} not in env, herdsman/{}/{var} or herdsman/all/{var} → herdsman secret request <thread> {var}", f.unwrap_or("all")),
        |k| format!("✓ cloud {name} key — {}", k.1),
    ));
    let mut uses = vec![];
    for (verb, line) in rc
        .iter()
        .filter(|v| !v.0.starts_with("runner_"))
        .filter_map(|(k, v)| Some((k, v.as_str()?)))
    {
        let w: Vec<_> = line
            .split(|ch: char| !(ch.is_alphanumeric() || "-_".contains(ch)))
            .filter(|w| !w.is_empty())
            .collect();
        uses.extend(
            w.windows(2)
                .filter(|p| p[0] == c && !p[1].starts_with('-'))
                .map(|p| (p[1], verb, line)),
        );
    }
    if uses.is_empty() {
        return Ok(());
    }
    let help = r.run_in("sh", &argv(&["-c", &format!("{c} --help")]), "")?;
    if help.code != 0 {
        rows.push(format!("✗ cloud {name} cli — `{c} --help` fails → install the {c} CLI"));
        return Ok(());
    }
    let has = |s: &str| {
        help.stdout
            .lines()
            .any(|l| l.starts_with("  ") && l.split_whitespace().next() == Some(s))
    };
    let miss: Vec<_> = uses.iter().filter(|u| !has(u.0)).collect();
    rows.extend(
        miss.iter()
            .map(|(s, v, l)| format!("✗ cloud {name} cli — {c} has no `{s}` (recipe {v} = \"{l}\")")),
    );
    let mut subs: Vec<_> = uses.iter().map(|u| u.0).collect();
    subs.sort_unstable();
    subs.dedup();
    if miss.is_empty() {
        rows.push(format!("✓ cloud {name} cli — {c} has {}", subs.join(", ")));
    }
    Ok(())
}

/// #635: an `Include` in `~/.ssh/config` that reads a host-key file under `config.d` (ssh parses it as config).
pub fn includes(ctx: &Ctx, rows: &mut Vec<String>) {
    let ssh = ctx.home.join(".ssh");
    let conf = std::fs::read_to_string(ssh.join("config")).unwrap_or_default();
    let files = std::fs::read_dir(ssh.join("config.d")).into_iter().flatten().flatten();
    let known: Vec<_> = files
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .filter(|f| std::path::Path::new(f).extension().is_some_and(|e| e == "known"))
        .collect();
    // `*` only: the text before the first and after the last.
    let glob = |p: &str, s: &str| {
        let ends = p.split_once('*').zip(p.rsplit_once('*'));
        ends.map_or(p == s, |((a, _), (_, b))| s.starts_with(a) && s.ends_with(b))
    };
    for l in conf
        .lines()
        .map(str::trim)
        .filter(|l| l.to_lowercase().starts_with("include "))
    {
        for p in l[8..].split_whitespace() {
            let p = p.trim_start_matches("~/.ssh/");
            if let Some(f) = known.iter().find(|f| glob(p, &format!("config.d/{f}"))) {
                rows.push(format!("✗ ssh include — `{l}` reads config.d/{f} as config → Include config.d/herdsman, and rm ~/.ssh/config.d/*.known (now under ~/.ssh/herdsman)"));
            }
        }
    }
}
