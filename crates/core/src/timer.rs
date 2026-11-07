//! #98 `herdsman setup|timer|tick` and the Host tab's Services rows: herdsman runs its own 5-minute job, no Hermes.
use crate::config::Config;
use crate::events::{age, emit, read, Event};
use crate::runner::{ok, Runner};
use crate::{drive, herdr, host, Ctx};
use anyhow::Result;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const TICK: &str = "dev.herdsman.tick";
/// #212: the every-minute `drive --fast` job, its own job so the slow tick never delays it.
pub const FAST: &str = "dev.herdsman.fast";
/// #237: the build's own jobs (label, args); `install` derives each one's interval from its args.
/// #345: `herdsman service`, kept alive; it runs the fast step itself (and the board with `--board`).
pub const SERVICE: &str = "dev.herdsman.service";
pub const JOBS: [(&str, &[&str]); 2] = [(TICK, &["tick"]), (FAST, &["drive", "--fast"])];
/// #557: herdsman's own install every 5 min and the daily scheduled clean, no Hermes; `setup` and `timer` write them.
pub const UPDATE: &str = "dev.herdsman.update";
pub const CLEAN: &str = "dev.herdsman.clean";
pub const MORE: [(&str, &[&str]); 2] = [(UPDATE, &["self-update"]), (CLEAN, &["clean", "--scheduled"])];
/// #557: the `every` of a job that runs daily at 03:30 (launchd `StartCalendarInterval`, systemd `OnCalendar`).
pub const DAILY: u64 = 86_400;
const MAC: bool = cfg!(target_os = "macos");
const FIX: &str = "herdsman timer install";
const ABANDON: &str = "<key>AbandonProcessGroup</key><true/>";
const HANDOFF: &str = "restart herdr to finish the update: `herdr update --handoff`";
/// One Services row: (item, tone `g|a|r`, detail, the fix shown on red).
pub type Row = (String, &'static str, String, String);

/// The job's files (path, content) for launchd (`mac`) or systemd: `argv` every `every` seconds or kept alive (0),
/// with an explicit PATH, both streams to `<state>/tick.log` (or `serve.log`).
pub fn files(ctx: &Ctx, label: &str, argv: &[&str], path: &str, every: u64, mac: bool) -> Vec<(PathBuf, String)> {
    let (i, secs) = (usize::from(every > 0), every.to_string());
    let log = ctx.state.with_file_name(["serve.log", "tick.log"][i]);
    let log = log.to_string_lossy();
    let (cmd, when, template, dest) = if mac {
        let strings = argv.iter().map(|a| format!("<string>{a}</string>")).collect();
        let when = ["KeepAlive</key><true/>", "StartInterval</key><integer>{secs}</integer>"][i];
        let at = "StartCalendarInterval</key><dict><key>Hour</key><integer>3</integer><key>Minute</key><integer>30</integer></dict>";
        let when = if every == DAILY { at } else { when };
        let dest = format!("Library/LaunchAgents/{label}.plist");
        (strings, when, include_str!("../assets/job.plist"), dest)
    } else {
        let when = ["Restart=always\n\n[Install]\nWantedBy=default.target", "Type=oneshot"][i];
        let dest = format!(".config/systemd/user/{label}.service");
        (argv.join(" "), when, include_str!("../assets/job.service"), dest)
    };
    let kv = [("{label}", label), ("{args}", &cmd), ("{path}", path), ("{log}", &log)];
    let fill = |s: String, (k, v): &(&str, &str)| s.replace(k, v).replace("{secs}", &secs);
    let text = kv.iter().fold(template.replace("{when}", when), fill);
    let timer = ctx.home.join(format!(".config/systemd/user/{label}.timer"));
    let mut unit = include_str!("../assets/job.timer").replace("{secs}", &secs);
    if every == DAILY {
        unit = unit.replace(
            "OnActiveSec=0\nOnUnitActiveSec=86400",
            "OnCalendar=*-*-* 03:30:00\nPersistent=true",
        );
    }
    let timer = (every > 0 && !mac).then_some((timer, unit));
    std::iter::once((ctx.home.join(dest), text)).chain(timer).collect()
}

/// (probe, load, unload) of the job: `launchctl` in the gui domain, else `systemctl --user`.
fn ctl(ctx: &Ctx, label: &str, every: bool) -> [String; 3] {
    if MAC {
        let uid = std::fs::metadata(&ctx.home).map_or(0, |m| m.uid());
        let (t, plist) = (format!("gui/{uid}/{label}"), files(ctx, label, &[], "", 0, true));
        let load = format!("launchctl bootstrap gui/{uid} {}", plist[0].0.display());
        return [format!("launchctl print {t}"), load, format!("launchctl bootout {t}")];
    }
    let unit = format!("{label}.{}", if every { "timer" } else { "service" });
    ["is-active", "enable --now", "disable --now"].map(|a| format!("systemctl --user {a} {unit}"))
}

/// #557: restart the board job on the binary just installed: `launchctl kickstart -k`, else `systemctl --user restart`.
pub fn restart(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> Result<String> {
    let label = cfg.serve.launchd_label.as_deref().unwrap_or("dev.herdsman.serve");
    let probe = ctl(ctx, label, false)[0].replacen("print", "kickstart -k", 1);
    exec(r, &probe.replacen("is-active", "restart", 1))
}

fn exec(r: &dyn Runner, cmd: &str) -> Result<String> {
    let mut w = cmd.split(' ');
    ok(r, w.next().unwrap_or_default(), &w.collect::<Vec<_>>())
}

/// The job's files as `install` writes them, with `path` for PATH.
fn expect(ctx: &Ctx, cfg: &Config, label: &str, args: &[&str], path: &str) -> Vec<(PathBuf, String)> {
    let every = !["serve", "service"].contains(&args[0]);
    let secs = match args[0] {
        _ if !every => 0,
        "drive" => cfg.limits.fast_secs,
        "self-update" => 300,
        "clean" => DAILY,
        _ => cfg.limits.slow_secs,
    };
    let cmd: Vec<&str> = std::iter::once(ctx.exe.as_str()).chain(args.iter().copied()).collect();
    files(ctx, label, &cmd, path, secs, MAC)
}

/// #237: each own job against this build: `loaded`, `missing` (no file, or not loaded) or `outdated (args or
/// interval differ)`; PATH is the host's, never compared.
pub fn drift(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> Vec<(&'static str, &'static str)> {
    let same = |(p, t): &(PathBuf, String)| {
        let x = std::fs::read_to_string(p).unwrap_or_default();
        t.split_once('\0')
            .map_or(x == *t, |(a, b)| x.starts_with(a) && x.ends_with(b))
    };
    let state = |(label, args): (&'static str, &[&str])| {
        let files = expect(ctx, cfg, label, args, "\0");
        let up = files.iter().all(|f| f.0.exists()) && exec(r, &ctl(ctx, label, true)[0]).is_ok();
        let i = usize::from(up) + usize::from(up && files.iter().all(same));
        // #340: without it launchd kills the job's detached kicks when the job exits.
        let abandons = |(p, _): &(PathBuf, String)| std::fs::read_to_string(p).is_ok_and(|x| x.contains(ABANDON));
        if i == 1 && MAC && !files.iter().all(abandons) {
            return (label, "outdated (no AbandonProcessGroup)");
        }
        (label, ["missing", "outdated (args or interval differ)", "loaded"][i])
    };
    JOBS.map(state).to_vec()
}

/// The jobs' PATH: the dirs of the harness binaries, `gh`, `herdr` and herdsman, then the system dirs
/// (`/usr/sbin:/sbin` for `sysctl` and `vm_stat`, the #30 lesson).
fn path(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> String {
    let roles = cfg.fleet.values().flat_map(|f| f.role.values());
    let found = roles
        .map(|role| role.harness.as_str())
        .chain(["gh", herdr::BIN, "cargo"]) // #557: `self-update` builds with cargo
        .filter_map(|b| ok(r, "which", &[b]).ok());
    let dir = |p: String| Some(Path::new(p.trim()).parent()?.to_string_lossy().into_owned());
    let mut dirs: Vec<String> = found.chain([ctx.exe.clone()]).filter_map(dir).collect();
    dirs.extend(["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from));
    let mut seen = std::collections::BTreeSet::new();
    dirs.retain(|d| !d.is_empty() && seen.insert(d.clone()));
    dirs.join(":")
}

/// Write the job and load it (unless `!load`): `= already` when written (and loaded) as is, else `✓` or `✗ … → fix`.
pub fn install(ctx: &Ctx, cfg: &Config, r: &dyn Runner, label: &str, args: &[&str], load: bool) -> String {
    let (every, files) = (
        !["serve", "service"].contains(&args[0]),
        expect(ctx, cfg, label, args, &path(ctx, cfg, r)),
    );
    let [probe, up, down] = ctl(ctx, label, every);
    let live = load && exec(r, &probe).is_ok();
    let same = |(p, t): &(PathBuf, String)| std::fs::read_to_string(p).is_ok_and(|x| x == *t);
    if (live || !load) && files.iter().all(same) {
        return format!("= already {label}");
    }
    for (p, t) in &files {
        let wrote = std::fs::create_dir_all(p.parent().unwrap_or(p)).and_then(|()| std::fs::write(p, t));
        if let Err(e) = wrote {
            return format!("✗ {label}: {e} → check {}", p.display());
        }
    }
    if !load {
        return format!("✓ wrote {label}; load it: {up}");
    }
    if live || MAC {
        let _ = exec(r, &down); // a loaded job reloads its new content
    }
    if !MAC {
        let _ = exec(r, "systemctl --user daemon-reload");
    }
    exec(r, &up).map_or_else(|e| format!("✗ {label}: {e:#} → {up}"), |_| format!("✓ {up}"))
}

/// `herdsman timer install|remove|status`.
pub fn timer(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, cmd: (&str, &str, bool)) -> Result<i32> {
    let (action, label, load) = cmd;
    let both = [(label, JOBS[0].1), JOBS[1], MORE[0], MORE[1]];
    let line = match action {
        "install" => {
            let lines = both.map(|(l, args)| (l, install(ctx, cfg, r, l, args, load)));
            installed(ctx, cfg, &lines)?;
            lines.map(|l| l.1).join("\n")
        }
        "remove" => both
            .map(|(label, _)| {
                let _ = exec(r, &ctl(ctx, label, true)[2]);
                let files = files(ctx, label, &[], "", 1, MAC);
                let gone = files.iter().filter(|f| std::fs::remove_file(&f.0).is_ok()).count();
                if gone > 0 {
                    format!("✓ removed {label}")
                } else {
                    format!("= no {label} job")
                }
            })
            .join("\n"),
        _ => {
            let on = ["not loaded", "loaded"][usize::from(exec(r, &ctl(ctx, label, true)[0]).is_ok())];
            let (_, detail) = tick_row(ctx, &read(&ctx.state)?);
            format!("{label}: {on} · tick {detail}")
        }
    };
    writeln!(out, "{line}")?;
    Ok(i32::from(line.contains('✗')))
}

/// #461: log `timer-install`: `by` the owner or the lane (`HERDSMAN_LANE`), the jobs written and the labels
/// reloaded; a conductor's thread (`HERDSMAN_THREAD`) shows it as a system line.
fn installed(ctx: &Ctx, cfg: &Config, lines: &[(&str, String)]) -> Result<()> {
    let lane = std::env::var("HERDSMAN_LANE").unwrap_or_default();
    let pick = |f: fn(&str) -> bool| lines.iter().filter(|l| f(&l.1)).map(|l| l.0).collect::<Vec<_>>();
    let written = pick(|s| !s.starts_with("= ") && !s.contains("→ check"));
    let reloaded = pick(|s| s.starts_with("✓ ") && !s.starts_with("✓ wrote"));
    let by = if lane.is_empty() { "owner" } else { &lane };
    let mut d = serde_json::json!({"by": by, "written": written, "reloaded": reloaded});
    if let Some(t) = std::env::var("HERDSMAN_THREAD").ok().filter(|t| !t.is_empty()) {
        d["thread"] = t.into();
    }
    let ev = Event::new(ctx.now, "herdsman", None, "timer-install", d);
    emit(&ctx.state, cfg, ev)
}

/// Log `kind` for `item` and notify, at most once an hour per item.
fn alert(ctx: &Ctx, cfg: &Config, events: &[Event], kind: &str, item: &str, reason: &str) -> Result<()> {
    let recent = |e: &Event| e.kind == kind && e.str("key") == Some(item) && ctx.now < e.ts + 3600;
    if events.iter().any(recent) {
        return Ok(());
    }
    let d = serde_json::json!({"key": item, "reason": reason});
    emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, kind, d))
}

/// `herdsman tick`: `check`, then `drive` (under #92's lock), the hourly clean, remote build trims, then an hourly alert per
/// red Services row. A refusal exits 0; a real error (config, herdr) logs `tick-failed`, alerted at most hourly, and
/// exits 1. #561: one probe per remote host, 15 s per remote call; `tick.step` names the step for `tick-stuck`.
pub fn tick(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write) -> Result<i32> {
    let log = ctx.state.with_file_name("tick.log");
    if log.metadata().is_ok_and(|m| m.len() > 1 << 20) {
        let f = std::fs::OpenOptions::new().write(true).open(&log);
        let _ = f.and_then(|f| f.set_len(0));
    }
    let mark = ctx.state.with_file_name("tick.step");
    let pass = crate::hosts::Pass::new(r, Some((mark.clone(), ctx.now)));
    let loaded = Config::load(&ctx.config);
    let res = loaded.as_ref().map_err(|e| anyhow::anyhow!("{e:#}")).and_then(|cfg| {
        pass.step("herdr status");
        herdr::status(&pass).map_err(|e| anyhow::anyhow!("herdr: {e:#}"))?;
        pass.step("check");
        crate::check::check(ctx, cfg, &pass, out)?;
        pass.step("drive");
        let driven = drive::drive(ctx, cfg, &pass, out, None, false, false);
        // #164: the `clean --yes` sweep once an hour, not only after a merge; its failures don't fail the tick.
        // #561: after drive, so no spawn or merge waits on it.
        let at = ctx.state.with_file_name("clean.at");
        let last = std::fs::read_to_string(&at)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        if last.is_none_or(|t| ctx.now >= t + 3600) {
            pass.step("clean");
            let _ = std::fs::write(&at, ctx.now.to_string());
            if let Err(e) = crate::check::clean(ctx, cfg, &pass, out, None, (None, None, false), true) {
                writeln!(out, "clean: {e:#}")?;
            }
        }
        pass.step("trim");
        crate::check::trim_hosts(cfg, &pass, out)?; // #485: maintenance after drive, never before it
        pass.step("lesson");
        crate::lesson::weekly(ctx, cfg, &pass, out)?; // #419
        driven
    });
    let _ = std::fs::remove_file(&mark);
    let (cfg, events) = (loaded.unwrap_or_default(), read(&ctx.state).unwrap_or_default());
    match res {
        Err(e) if e.downcast_ref::<crate::gate::Refusal>().is_some() => writeln!(out, "tick: {e}")?,
        Err(e) => {
            writeln!(out, "tick failed: {e:#}")?;
            alert(ctx, &cfg, &events, "tick-failed", "tick", &format!("{e:#}"))?;
            return Ok(1);
        }
        Ok(()) => {}
    }
    repair(ctx, &cfg, r, &events)?;
    for (item, _, detail, fix) in services(ctx, &cfg, r, None).into_iter().filter(|s| s.1 == "r") {
        let why = format!("{item}: {detail} → {fix}");
        alert(ctx, &cfg, &events, "service-red", &item, &why)?;
    }
    Ok(0)
}

/// #561: a tick still running 10 min after it started (its `tick.step`): `tick-stuck` once, naming the step and the
/// child it waits on. The fast step looks, since a stuck tick cannot.
pub fn stuck(ctx: &Ctx, cfg: &Config) -> Result<()> {
    let Ok(text) = std::fs::read_to_string(ctx.state.with_file_name("tick.step")) else {
        return Ok(());
    };
    let m: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let Some(since) = m["since"].as_u64().filter(|t| ctx.now >= t + 600) else {
        return Ok(());
    };
    if read(&ctx.state)?
        .iter()
        .any(|e| e.kind == "tick-stuck" && e.data["key"] == since)
    {
        return Ok(());
    }
    let (step, child) = (
        m["step"].as_str().unwrap_or("?"),
        m["child"].as_str().unwrap_or_default(),
    );
    let child = if child.is_empty() { "none" } else { child };
    let why = format!(
        "tick running {} min, in {step}; waiting on: {child}",
        (ctx.now - since) / 60
    );
    let d = serde_json::json!({"key": since, "step": step, "child": child, "reason": why});
    emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, "tick-stuck", d))
}

/// #237: reinstall each own job `drift` finds missing or outdated, read it back, log `timer-repaired`; alert when
/// that fails. Never the tick's own job: reloading it would end this run (its row stays red, alerted).
fn repair(ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event]) -> Result<()> {
    for ((label, state), (_, args)) in drift(ctx, cfg, r).into_iter().zip(JOBS) {
        // #345: a running service is the fast step; its job stays out.
        if label == TICK || state == "loaded" || label == FAST && crate::service::up(ctx).is_some() {
            continue;
        }
        let line = install(ctx, cfg, r, label, args, true);
        if line.starts_with('✓') && exec(r, &ctl(ctx, label, true)[0]).is_ok() {
            let d = serde_json::json!({"label": label, "was": state});
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, "herdsman", None, "timer-repaired", d),
            )?;
        } else {
            alert(ctx, cfg, events, "timer-repair-failed", label, &line)?;
        }
    }
    Ok(())
}

/// Every file under the config dir and the fleets' Hermes configs: did `profiles sync` change any?
fn snap(ctx: &Ctx, cfg: &Config) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(p: &Path, v: &mut Vec<(PathBuf, Vec<u8>)>) {
        match std::fs::read_dir(p) {
            Ok(d) => d.flatten().for_each(|e| walk(&e.path(), v)),
            Err(_) => v.extend(std::fs::read(p).ok().map(|b| (p.into(), b))),
        }
    }
    let mut v = vec![];
    walk(ctx.config.parent().unwrap_or(&ctx.config), &mut v);
    for (role, def) in cfg.fleet.values().flat_map(|f| &f.role) {
        walk(&ctx.hermes.join(def.profile_or(role)).join("config.yaml"), &mut v);
    }
    v.sort();
    v
}

/// `herdsman setup`: tick job, board job (or adopt one under another label), tailscale serve, profiles sync, then
/// doctor, whose exit code it returns. Each step prints `✓ done`, `= already` or `✗ <error> → <fix>`.
pub fn setup(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, load: bool) -> Result<i32> {
    let cfg = Config::load(&ctx.config)?;
    let (label, addr) = (cfg.serve.launchd_label.as_deref(), cfg.serve.addr());
    let label = label.unwrap_or("dev.herdsman.serve");
    let other = crate::doctor::serve_plist(ctx).filter(|p| MAC && !p.ends_with(format!("{label}.plist")));
    let board = match other {
        Some(p) => format!("= adopted {} (re-labelling is manual: docs/setup.md)", p.display()),
        None => install(ctx, &cfg, r, label, &["serve", "--addr", &addr], load),
    };
    let port = cfg.serve.tailscale_https_port;
    let https = port.unwrap_or_default();
    let up = format!("tailscale serve --bg --https={https} http://{addr}");
    let tailscale = if port.is_none() {
        "= tailscale serve skipped: [serve] tailscale_https_port is unset".into()
    } else if ok(r, "which", &["tailscale"]).is_err() {
        "= tailscale serve skipped: tailscale is not on PATH".into()
    } else if exec(r, "tailscale serve status").is_ok_and(|s| s.contains(&addr)) {
        format!("= already tailscale serve → {addr}")
    } else {
        exec(r, &up).map_or_else(|e| format!("✗ tailscale serve: {e:#} → {up}"), |_| format!("✓ {up}"))
    };
    let before = snap(ctx, &cfg);
    let synced = crate::spawn::profiles_sync(ctx, &cfg, &crate::usage_doc(), &mut std::io::sink());
    let sync = match synced {
        Err(e) => format!("✗ profiles sync: {e:#} → herdsman profiles sync"),
        Ok(()) if snap(ctx, &cfg) == before => "= already profiles synced".into(),
        Ok(()) => "✓ profiles synced".into(),
    };
    let tick = install(ctx, &cfg, r, TICK, &["tick"], load);
    let fast = install(ctx, &cfg, r, FAST, &["drive", "--fast"], load);
    let [update, clean] = MORE.map(|(l, args)| install(ctx, &cfg, r, l, args, load));
    writeln!(
        out,
        "{}",
        [tick, fast, board, tailscale, sync, update, clean].join("\n")
    )?;
    crate::doctor::doctor(ctx, r, out, false, false)
}

/// #345 `herdsman service --install`: write its unit (kept alive: launchd/systemd restart it) and print the commands
/// that swap it in for the fast-step job (and the board job, with `--board`); loads nothing itself.
pub fn service_unit(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, api: &str, board: Option<&str>) -> Result<()> {
    let cfg = Config::load(&ctx.config)?;
    let mut args = vec!["service", "--api", api];
    args.extend(board.iter().flat_map(|b| ["--board", *b]));
    writeln!(out, "{}", install(ctx, &cfg, r, SERVICE, &args, false))?;
    let serve = cfg.serve.launchd_label.as_deref().unwrap_or("dev.herdsman.serve");
    writeln!(out, "To swap it in, run:")?;
    for (label, every) in std::iter::once((FAST, true)).chain(board.map(|_| (serve, false))) {
        writeln!(out, "  {}", ctl(ctx, label, every)[2])?;
    }
    writeln!(out, "  {}", ctl(ctx, SERVICE, false)[1])?;
    Ok(())
}

/// The phone alerts state (#87): on/off, devices and last sent, unless a webpush failure is newer.
pub fn phone(ctx: &Ctx, cfg: Option<&Config>, events: &[Event]) -> (&'static str, String) {
    let subs = crate::webpush::load(&ctx.state).len();
    let sent = events.iter().rfind(|e| e.kind == "push-sent");
    let webpush = |e: &&Event| e.kind == "notify-failed" && e.str("cmd") == Some("webpush");
    let failed = events
        .iter()
        .rfind(webpush)
        .filter(|f| sent.is_none_or(|e| f.ts >= e.ts));
    let when = sent.map_or("never sent".into(), |e| {
        format!("last sent {}", ago(ctx.now, Some(e.ts)))
    });
    let s = if subs == 1 { "" } else { "s" };
    match (cfg.and_then(Config::webpush_sub), failed) {
        (None, _) => ("a", "off: no webpush notifier".into()),
        (_, Some(f)) => ("r", format!("on · last failure: {}", f.str("error").unwrap_or("?"))),
        _ => (
            ["a", "g"][usize::from(subs > 0)],
            format!("on · {subs} device{s} · {when}"),
        ),
    }
}

/// herdr's (client, server) versions, and whether the server is older (`herdr status`).
pub fn herdr_versions(r: &dyn Runner) -> Option<(String, String, bool)> {
    versions(&herdr::status(r).ok()?)
}

/// `herdr status` text's (client, server) versions, and whether the server is older.
pub fn versions(status: &str) -> Option<(String, String, bool)> {
    let version = |l: &str| l.trim().strip_prefix("version: ").map(String::from);
    let mut v = status.lines().filter_map(version);
    let (client, server) = (v.next()?, v.next()?);
    let parse = |v: &str| v.split('.').map(|n| n.parse().unwrap_or(0)).collect::<Vec<u32>>();
    let older = parse(&server) < parse(&client);
    Some((client, server, older))
}

/// Tone for a time: amber after 10 min, red after 15 (or never).
fn tone(now: u64, t: Option<u64>) -> &'static str {
    let since = t.map_or(u64::MAX, |t| now.saturating_sub(t));
    ["g", "a", "r"][usize::from(since > 600) + usize::from(since > 900)]
}

fn ago(now: u64, t: Option<u64>) -> String {
    t.map_or("never".into(), |t| format!("{} ago", age(now.saturating_sub(t))))
}

/// The last tick: the `check.at` stamp `check` writes, red when a `tick-failed` is newer.
fn tick_row(ctx: &Ctx, events: &[Event]) -> (&'static str, String) {
    let stamp = std::fs::read_to_string(ctx.state.with_file_name("check.at")).unwrap_or_default();
    let ran = stamp.trim().parse::<u64>().ok();
    let newer = |e: &&Event| e.kind == "tick-failed" && ran.is_none_or(|t| e.ts >= t);
    let failed = events.iter().rfind(newer);
    match failed.and_then(|e| e.str("reason")) {
        Some(why) => ("r", format!("failed: {why}")),
        None => (tone(ctx.now, ran), format!("ran {}", ago(ctx.now, ran))),
    }
}

/// The Host tab's Services rows from the event log, `herdr`, the host and the stamps; `board`: the serving
/// page's own detail, else the board is up when its address listens.
pub fn services(ctx: &Ctx, cfg: &Config, r: &dyn Runner, board: Option<String>) -> Vec<Row> {
    let events = crate::events::shared(&ctx.state).unwrap_or_default();
    let row = |item: &str, t, d: String, fix: &str| (item.to_string(), t, d, fix.to_string());
    let addr = cfg.serve.addr();
    let port = format!("-iTCP:{}", addr.rsplit(':').next().unwrap_or_default());
    let listens = || ok(r, "lsof", &["-nP", &port, "-sTCP:LISTEN"]).is_ok();
    let board = board.or_else(|| listens().then(|| format!("listening on {addr}")));
    let t = if board.is_some() { "g" } else { "r" };
    let d = board.unwrap_or(format!("down on {addr}"));
    let mut rows = vec![row("board", t, d, "herdsman setup")];
    // #237: each own job against this build, folded into its step's row: red unless loaded.
    let drift = drift(ctx, cfg, r);
    let service = crate::service::up(ctx);
    let job = |i: usize, t, d: String| match drift[i].1 {
        "loaded" => (t, d),
        _ if i == 1 && service.is_some() => (t, d + " · by the service"),
        state => ("r", format!("{d} · job {state}")),
    };
    let (t, d) = tick_row(ctx, &events);
    let (t, d) = job(0, t, d);
    rows.push(row("tick", t, d, FIX));
    for (name, f) in &cfg.fleet {
        let item = format!("drive {name}");
        let last = drive::last_pass(ctx, name);
        let did = events.iter().rfind(|e| e.kind == "driven" && e.fleet == *name);
        let did = did.and_then(|e| e.str("result")).unwrap_or("nothing yet");
        let driven = format!("herdsman · last pass {} · {did}", ago(ctx.now, last));
        rows.push(if f.paused {
            row(&item, "a", "paused".into(), "")
        } else {
            row(&item, tone(ctx.now, last), driven, FIX)
        });
    }
    // #246: how long the last fast step took and when it ran.
    let (at, secs) = drive::fast_step(ctx).unzip();
    let took = secs
        .flatten()
        .map_or(String::new(), |s| format!("fast step {s:.1} s · "));
    // #237: red after 3× its interval.
    let late = at.is_none_or(|t| ctx.now.saturating_sub(t) > 3 * cfg.limits.fast_secs);
    let t = if late { "r" } else { tone(ctx.now, at) };
    let (t, d) = job(1, t, format!("{took}ran {}", ago(ctx.now, at)));
    rows.push(row("fast step", t, d, FIX));
    // #304: the last kick (a lane push, stop or ask, a board tap) beside the timers.
    let k = events.iter().rfind(|e| e.kind == "kick");
    let why = k.map(|e| format!(" ({} #{})", e.str("why").unwrap_or("?"), e.issue.unwrap_or(0)));
    let d = format!("last kick {}{}", ago(ctx.now, k.map(|e| e.ts)), why.unwrap_or_default());
    rows.push(row("kick", "m", d, ""));
    // #529: the service's lane event streams; amber while one is down and the passes poll.
    for rt in [herdr::BIN, "shed"].iter().filter(|_| service.is_some()) {
        let last = events
            .iter()
            .rfind(|e| e.kind == "lane-stream" && e.str("runtime") == Some(rt));
        let Some(e) = last else { continue };
        let (t, d) = match e.str("why") {
            None => ("g", format!("live, since {}", ago(ctx.now, Some(e.ts)))),
            Some(why) => ("a", format!("down {}: {why} · polling", ago(ctx.now, Some(e.ts)))),
        };
        rows.push(row(&format!("{rt} lane events"), t, d, ""));
    }
    let (t, d) = phone(ctx, Some(cfg), &events);
    rows.push(row("phone alerts", t, d, "herdsman push test"));
    let v = herdr_versions(r);
    let shown = |(c, s, _): &(String, String, bool)| format!("client {c}, server {s}");
    let d = v.as_ref().map_or("not on PATH, or no server".into(), shown);
    let t = v.map_or("r", |v| ["g", "a"][usize::from(v.2)]);
    rows.push(row(herdr::BIN, t, d, [herdr::BIN, HANDOFF][usize::from(t == "a")]));
    if let Some(gb) = host::free_gb(r) {
        let (min, n) = (cfg.limits.min_free_gb, usize::from);
        let t = ["g", "a", "r"][n(gb <= min * 1.25) + n(gb < min)];
        rows.push(row("disk", t, format!("{gb:.0} GB free"), "herdsman clean --yes"));
    }
    // #164: the worktree total from the last walk (#160: never a `du` here), against the budget; amber from 80 %,
    // red over (DISK_BUDGET).
    let budget = cfg.limits.worktree_budget_gb;
    let du = crate::check::worktree_du(ctx);
    let (t, d) = du.map_or(("a", "not measured yet".into()), |(gb, at)| {
        let full = |i: &u8| f64::from(*i) < (gb / budget * 10.0).round();
        let bar: String = (0..10).map(|i| if full(&i) { '█' } else { '░' }).collect();
        let t = ["g", "a", "r"][usize::from(gb > budget * 0.8) + usize::from(gb > budget)];
        (t, format!("▕{bar}▏ {gb:.1} / {budget} GB · {}", ago(ctx.now, at)))
    });
    rows.push(row("fleet disk", t, d, "herdsman clean --yes"));
    rows
}
