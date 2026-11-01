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
const MAC: bool = cfg!(target_os = "macos");
const FIX: &str = "herdsman timer install";
/// One Services row: (item, tone `g|a|r`, detail, the fix shown on red).
pub type Row = (String, &'static str, String, String);

/// The job's files (path, content) for launchd (`mac`) or systemd: `argv` every 5 min (`every`) or kept alive,
/// with an explicit PATH, both streams to `<state>/tick.log` (or `serve.log`).
pub fn files(ctx: &Ctx, label: &str, argv: &[&str], path: &str, every: bool, mac: bool) -> Vec<(PathBuf, String)> {
    let log = ctx.state.with_file_name(if every { "tick.log" } else { "serve.log" });
    let (log, i) = (log.to_string_lossy(), usize::from(every));
    let (cmd, when, template, dest) = if mac {
        let strings = argv.iter().map(|a| format!("<string>{a}</string>")).collect();
        let when = ["KeepAlive</key><true/>", "StartInterval</key><integer>300</integer>"][i];
        let dest = format!("Library/LaunchAgents/{label}.plist");
        (strings, when, include_str!("../assets/job.plist"), dest)
    } else {
        let when = ["Restart=always\n\n[Install]\nWantedBy=default.target", "Type=oneshot"][i];
        let dest = format!(".config/systemd/user/{label}.service");
        (argv.join(" "), when, include_str!("../assets/job.service"), dest)
    };
    let kv = [("{label}", label), ("{args}", &cmd), ("{path}", path), ("{log}", &log)];
    let fill = |s: String, (k, v): &(&str, &str)| s.replace(k, v);
    let text = kv.iter().fold(template.replace("{when}", when), fill);
    let timer = ctx.home.join(format!(".config/systemd/user/{label}.timer"));
    let timer = (every && !mac).then(|| (timer, include_str!("../assets/job.timer").into()));
    std::iter::once((ctx.home.join(dest), text)).chain(timer).collect()
}

/// (probe, load, unload) of the job: `launchctl` in the gui domain, else `systemctl --user`.
fn ctl(ctx: &Ctx, label: &str, every: bool) -> [String; 3] {
    if MAC {
        let uid = std::fs::metadata(&ctx.home).map_or(0, |m| m.uid());
        let (t, plist) = (format!("gui/{uid}/{label}"), files(ctx, label, &[], "", every, true));
        let load = format!("launchctl bootstrap gui/{uid} {}", plist[0].0.display());
        return [format!("launchctl print {t}"), load, format!("launchctl bootout {t}")];
    }
    let unit = format!("{label}.{}", if every { "timer" } else { "service" });
    ["is-active", "enable --now", "disable --now"].map(|a| format!("systemctl --user {a} {unit}"))
}

fn exec(r: &dyn Runner, cmd: &str) -> Result<String> {
    let mut w = cmd.split(' ');
    ok(r, w.next().unwrap_or_default(), &w.collect::<Vec<_>>())
}

/// Is the tick job loaded (`herdsman doctor`)?
pub fn loaded(ctx: &Ctx, r: &dyn Runner) -> bool {
    exec(r, &ctl(ctx, TICK, true)[0]).is_ok()
}

/// The jobs' PATH: the dirs of the harness binaries, `gh`, `herdr` and herdsman, then the system dirs
/// (`/usr/sbin:/sbin` for `sysctl` and `vm_stat`, the #30 lesson).
fn path(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> String {
    let roles = cfg.fleet.values().flat_map(|f| f.role.values());
    let found = roles
        .map(|role| role.harness.as_str())
        .chain(["gh", herdr::BIN])
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
    let (every, exe) = (args[0] == "tick", ctx.exe.as_str());
    let cmd: Vec<&str> = std::iter::once(exe).chain(args.iter().copied()).collect();
    let files = files(ctx, label, &cmd, &path(ctx, cfg, r), every, MAC);
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
    let line = match action {
        "install" => install(ctx, cfg, r, label, &["tick"], load),
        "remove" => {
            let _ = exec(r, &ctl(ctx, label, true)[2]);
            let files = files(ctx, label, &[], "", true, MAC);
            let gone = files.iter().filter(|f| std::fs::remove_file(&f.0).is_ok()).count();
            if gone > 0 {
                format!("✓ removed {label}")
            } else {
                format!("= no {label} job")
            }
        }
        _ => {
            let on = ["not loaded", "loaded"][usize::from(exec(r, &ctl(ctx, label, true)[0]).is_ok())];
            let (_, detail) = tick_row(ctx, &read(&ctx.state)?);
            format!("{label}: {on} · tick {detail}")
        }
    };
    writeln!(out, "{line}")?;
    Ok(i32::from(line.starts_with('✗')))
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

/// `herdsman tick`: `check`, then `drive` (under #92's lock), then an hourly alert per red Services row.
/// A refusal exits 0; a real error (config, herdr) logs `tick-failed`, alerted at most hourly, and exits 1.
pub fn tick(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write) -> Result<i32> {
    let log = ctx.state.with_file_name("tick.log");
    if log.metadata().is_ok_and(|m| m.len() > 1 << 20) {
        let f = std::fs::OpenOptions::new().write(true).open(&log);
        let _ = f.and_then(|f| f.set_len(0));
    }
    let loaded = Config::load(&ctx.config);
    let res = loaded.as_ref().map_err(|e| anyhow::anyhow!("{e:#}")).and_then(|cfg| {
        herdr::status(r).map_err(|e| anyhow::anyhow!("herdr: {e:#}"))?;
        crate::check::check(ctx, cfg, r, out)?;
        drive::drive(ctx, cfg, r, out, None)
    });
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
    for (item, _, detail, fix) in services(ctx, &cfg, r, None).into_iter().filter(|s| s.1 == "r") {
        let why = format!("{item}: {detail} → {fix}");
        alert(ctx, &cfg, &events, "service-red", &item, &why)?;
    }
    Ok(0)
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
    writeln!(out, "{}", [tick, board, tailscale, sync].join("\n"))?;
    crate::doctor::doctor(ctx, r, out, false)
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
    let status = herdr::status(r).ok()?;
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
    let events = read(&ctx.state).unwrap_or_default();
    let row = |item: &str, t, d: String, fix: &str| (item.to_string(), t, d, fix.to_string());
    let addr = cfg.serve.addr();
    let port = format!("-iTCP:{}", addr.rsplit(':').next().unwrap_or_default());
    let listens = || ok(r, "lsof", &["-nP", &port, "-sTCP:LISTEN"]).is_ok();
    let board = board.or_else(|| listens().then(|| format!("listening on {addr}")));
    let t = if board.is_some() { "g" } else { "r" };
    let d = board.unwrap_or(format!("down on {addr}"));
    let mut rows = vec![row("board", t, d, "herdsman setup")];
    let (t, d) = tick_row(ctx, &events);
    rows.push(row("tick", t, d, FIX));
    let agents = host::agents(r).unwrap_or_default();
    for (name, f) in &cfg.fleet {
        let item = format!("drive {name}");
        let orch = agents.iter().find(|a| a["name"] == format!("{name}-orch").as_str());
        let orch = orch.and_then(|a| a["agent_status"].as_str());
        let last = drive::last_pass(ctx, name);
        let did = events.iter().rfind(|e| e.kind == "driven" && e.fleet == *name);
        let did = did.and_then(|e| e.str("result")).unwrap_or("nothing yet");
        let driven = format!("herdsman · last pass {} · {did}", ago(ctx.now, last));
        let orched = format!("orch · {}", orch.unwrap_or("not running"));
        rows.push(match () {
            () if f.paused => row(&item, "a", "paused".into(), ""),
            () if f.driven() => row(&item, tone(ctx.now, last), driven, FIX),
            () => row(&item, ["a", "g"][usize::from(orch.is_some())], orched, ""),
        });
    }
    let (t, d) = phone(ctx, Some(cfg), &events);
    rows.push(row("phone alerts", t, d, "herdsman push test"));
    let v = herdr_versions(r);
    let shown = |(c, s, _): &(String, String, bool)| format!("client {c}, server {s}");
    let d = v.as_ref().map_or("not on PATH, or no server".into(), shown);
    let t = v.map_or("r", |v| ["g", "a"][usize::from(v.2)]);
    rows.push(row(herdr::BIN, t, d, herdr::BIN));
    if let Some(gb) = host::free_gb(r) {
        // The worktree total by `du -sk` (not #103's `check::du` walk): the page reads it through its 60 s cache.
        let du = ok(r, "du", &["-sk", &ctx.worktrees.to_string_lossy()]).unwrap_or_default();
        let kb = du.split_whitespace().next().and_then(|k| k.parse::<f64>().ok());
        let wt = kb.unwrap_or(0.0) / 1_048_576.0;
        let t = ["g", "a", "r"][usize::from(gb < host::AMBER_FREE_GB) + usize::from(gb < cfg.limits.min_free_gb)];
        let d = format!("{gb:.0} GB free · worktrees {wt:.1} GB");
        rows.push(row("disk", t, d, "herdsman clean --yes"));
    }
    rows
}
