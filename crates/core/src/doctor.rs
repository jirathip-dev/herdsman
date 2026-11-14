use crate::config::Config;
use crate::runner::{argv, ok, Runner};
use crate::Ctx;
use anyhow::Result;
use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

fn row(rows: &mut Vec<String>, pass: bool, bad: char, text: &str) {
    let cut = text.find(" → ").filter(|_| pass).unwrap_or(text.len());
    rows.push(format!("{} {}", if pass { '✓' } else { bad }, &text[..cut]));
}

fn mode(path: &Path) -> String {
    let bits = std::fs::metadata(path).map(|meta| meta.permissions().mode() & 0o777);
    bits.map_or("missing".into(), |bits| format!("mode {bits:o}"))
}

pub fn doctor(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, json: bool, config_only: bool) -> Result<i32> {
    let mut rows = vec![];
    if !config_only {
        let sample = crate::host::sample(r);
        let host = sample.pressure.is_some() && sample.swapout.is_some() && sample.load_per_cpu.is_some();
        row(&mut rows, host, '✗', "host — sysctl + vm_stat → put /usr/sbin on PATH");
        let gh = ok(r, "gh", &["auth", "status"]).is_ok();
        row(&mut rows, gh, '✗', "gh — auth status → gh auth login");
    }
    match Config::load(&ctx.config) {
        Ok(cfg) if config_only => config_rows(ctx, &cfg, &mut rows),
        Ok(cfg) => checks(ctx, &cfg, r, &mut rows),
        Err(e) => rows.push(format!("✗ config — {e:#} → edit {}", ctx.config.display())),
    }
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
    } else {
        writeln!(out, "{}", rows.join("\n"))?;
    }
    Ok(i32::from(rows.iter().any(|line| line.starts_with('✗'))))
}

fn config_rows(ctx: &Ctx, cfg: &Config, rows: &mut Vec<String>) {
    rows.push(format!("✓ config — {}", ctx.config.display()));
    rows.extend(cfg.warnings().iter().map(|(k, w)| format!("! config — {k} is {w}")));
}

fn account_rows(cfg: &Config, rows: &mut Vec<String>) {
    if cfg.accounts.names.is_empty() {
        return;
    }
    let src = cfg.accounts.account_env.as_ref().map_or_else(
        || "each account's own folder (`/login` in it, docs/accounts.md)".into(),
        |c| format!("the account_env helper `{c}`"),
    );
    rows.push(format!("✓ accounts — sign in from {src}"));
    rows.push("✓ meter — `accounts` shows each reading's age; one past its reset reads 0%".into());
    let hn = crate::providers::accounts().map_or("", |p| p.name);
    for name in cfg.accounts.names.keys() {
        let Some((_, f)) = crate::access::token_file(cfg, hn, name) else {
            continue;
        };
        let at = std::fs::metadata(&f).and_then(|m| m.modified());
        let days = at
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map_or(0, |a| a.as_secs() / 86_400);
        let (ok, line) = match crate::secret::file(&f) {
            Err(e) => (false, format!("{e:#}")),
            Ok(None) => (false, format!("{} missing → place it, mode 600", f.display())),
            Ok(Some(_)) => (
                days < TOKEN_DAYS,
                format!(
                    "{} is {days} days old → renew it before it expires (a year)",
                    f.display()
                ),
            ),
        };
        row(rows, ok, '!', &format!("account {name} token file — {line}"));
    }
}

const TOKEN_DAYS: u64 = 300;

fn checks(ctx: &Ctx, cfg: &Config, r: &dyn Runner, rows: &mut Vec<String>) {
    config_rows(ctx, cfg, rows);
    if cfg
        .runtime
        .as_ref()
        .is_some_and(|r| r.kind == crate::config::RuntimeKind::Berth)
    {
        let fix = format!("set kind = \"shed\" in {}", ctx.config.display());
        rows.push(format!(
            "! runtime — kind = \"berth\" is deprecated, berth is now shed → {fix}"
        ));
    }
    let line = crate::drive::spawns_line(ctx, cfg, r, &crate::events::working(&ctx.state).unwrap_or_default());
    row(rows, !line.contains(" · stale"), '!', &line);
    let (n401, n503) = crate::devices::denials(ctx);
    let log = ctx.state.with_file_name("serve.log");
    let line = format!("board auth — 24 h: {n401} × 401, {n503} × 503 → see {}", log.display());
    row(rows, n503 == 0, '!', &line);
    let line = format!("worktrees — {0} → mkdir -p {0}", ctx.worktrees.display());
    row(
        rows,
        crate::runner::answers(&ctx.worktrees) && ctx.worktrees.is_dir(),
        '✗',
        &line,
    );
    for (name, fleet) in &cfg.fleet {
        let repo = &fleet.repo;
        let view = ok(r, "gh", &["repo", "view", repo, "--json", "name,isArchived"]);
        let read = format!("gh {name} — read {repo} → gh auth refresh");
        row(rows, view.is_ok(), '✗', &read);
        let view = view
            .ok()
            .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok());
        if view.is_some_and(|v| v["isArchived"] == true) {
            rows.push(format!(
                "! gh {name} — {repo} is archived → remove [fleet.{name}] from fleet.toml"
            ));
        }
        if let Some(l) = fleet.merge_label.as_deref() {
            let has = crate::gh::label_exists(r, fleet, l).unwrap_or(false);
            let fix = crate::gh::label_fix(fleet, l);
            row(rows, has, '✗', &format!("label {name} — {l} in {repo} → {fix}"));
        }
        let dir = fleet.local_path().display().to_string();
        let branch = &fleet.integration;
        let origin = format!("origin/{branch}");
        let git = ok(r, "git", &["-C", &dir, "rev-parse", "--verify", &origin]).is_ok();
        let fix = format!("git -C {dir} fetch origin {branch} || git clone https://github.com/{repo}.git {dir}");
        row(rows, git, '✗', &format!("local {name} — {dir} has {origin} → {fix}"));
        if let Some(m) = fleet
            .land
            .as_ref()
            .map(|l| l.mode.as_str())
            .filter(|m| !["", "full-first", "fast-first"].contains(m))
        {
            rows.push(format!(
                "! land {name} — mode `{m}` is unknown, landing full-first → \"fast-first\" or \"full-first\""
            ));
        }
        if let Some(l) = fleet.fast_first() {
            if l.fast.is_empty() {
                rows.push(format!(
                    "! land {name} — fast = [] gates every check → list [fleet.{name}.land] fast"
                ));
            }
            let jq = ".[].statusCheckRollup[] | .name // .context";
            let a = [
                "pr",
                "list",
                "-R",
                repo,
                "--state",
                "merged",
                "-L",
                "5",
                "--json",
                "statusCheckRollup",
                "--jq",
                jq,
            ];
            let seen = ok(r, "gh", &a).unwrap_or_default();
            for x in l.fast.iter().filter(|x| !seen.lines().any(|n| n == x.as_str())) {
                rows.push(format!(
                    "! land {name} — fast check `{x}` not seen on recent PRs → fix [fleet.{name}.land] fast"
                ));
            }
        }
        if let Some(g) = fleet.serial.first().filter(|_| !fleet.serial_paths.is_empty()) {
            rows.push(format!(
                "! serial_paths is the old form; [[fleet.{name}.serial]] name=\"{}\"…",
                g.name
            ));
        }
        let bad = match fleet.bad.len() {
            0 if fleet.flow().builtin => "valid (built-in)".to_string(),
            0 => "valid".into(),
            n => format!("BAD_PIPELINE, {n} error(s): its fleet stops → fix [fleet.{name}.pipeline]"),
        };
        let bad = format!("pipeline {name} — {bad}");
        row(rows, fleet.bad.is_empty(), '✗', &bad);
        rows.extend(
            fleet
                .bad
                .iter()
                .map(|p| format!("  {} — {} (fleet.toml:{})", p.rule, p.msg, p.line)),
        );
    }
    account_rows(cfg, rows);
    let n = crate::webhook::rejected(ctx);
    let bad = match n {
        0 => String::new(),
        n => format!(" · {n} refused (bad signature)"),
    };
    let fix = "put the secret in a file, chmod 600, and name it in [serve] webhook_secret_file (docs/serve.md)";
    let (on, why) = match crate::webhook::secrets(cfg) {
        Ok(k) => (
            !k.is_empty(),
            if k.is_empty() { "no secret" } else { "secret set" }.to_string(),
        ),
        Err(e) => (false, format!("{e:#}")),
    };
    row(rows, on, '!', &format!("webhook — {why}{bad} → {fix}"));
    if cfg!(target_os = "macos") {
        let req = ["-v", "-R", "=certificate leaf[subject.CN] = \"drover-local\"", &ctx.exe];
        let line = format!(
            "signature — {} signed by \"drover-local\" → sh scripts/signing-identity.sh in the drover checkout (once), then sh scripts/install-gate.sh {} {}",
            ctx.exe, ctx.exe, ctx.exe
        );
        row(rows, ok(r, "codesign", &req).is_ok(), '✗', &line);
    }
    let jam = crate::host::keychain_jam(r);
    let line = format!("keychain readers — {}", crate::host::jam_line(&jam));
    row(rows, jam.0.len() <= cfg.limits.keychain_readers, '!', &line);
    let roles: Vec<_> = cfg.fleet.values().flat_map(|fleet| &fleet.role).collect();
    let harnesses: BTreeSet<_> = roles.iter().map(|role| role.1.harness.as_str()).collect();
    for binary in harnesses {
        let line = format!("harness — {binary} on PATH → install {binary}");
        row(rows, ok(r, "which", &[binary]).is_ok(), '✗', &line);
    }
    for (pass, bad, line) in crate::providers::lanes().flat_map(|p| p.doctor(ctx, cfg, r)) {
        row(rows, pass, bad, &line);
    }
    let stray = crate::spawn::stray_hooks(ctx, cfg);
    let what = if stray.is_empty() {
        "run this binary".into()
    } else {
        stray.join("; ")
    };
    row(
        rows,
        stray.is_empty(),
        '✗',
        &format!("profile hooks — {what} → drover profiles sync"),
    );
    let hosts = cfg.serve.allowed_hosts.join(", ");
    let line = format!("serve hosts — [{hosts}] → set [serve] allowed_hosts = [\"<name>.ts.net\"]");
    row(rows, !hosts.is_empty(), '✗', &line);
    let addr = "127.0.0.1:7777";
    let line = format!("serve — listening on {addr} → drover serve --addr {addr}");
    let up = ok(r, "lsof", &["-nP", "-iTCP:7777", "-sTCP:LISTEN"]).is_ok();
    row(rows, up, '✗', &line);
    let tailscale = r.run("tailscale", &argv(&["serve", "status"]));
    let line = format!("tailscale — serve maps to {addr} → tailscale serve --bg --https=8443 http://{addr}");
    let bad = if tailscale.is_err() { '!' } else { '✗' };
    let maps = tailscale.is_ok_and(|o| o.code == 0 && o.stdout.contains(addr));
    row(rows, maps, bad, &line);
    let label = cfg.serve.launchd_label.as_deref().unwrap_or("dev.drover.serve");
    let plist = ctx.home.join(format!("Library/LaunchAgents/{label}.plist"));
    match plist.is_file() {
        _ if !cfg!(target_os = "macos") => {}
        false => {
            let found = serve_plists(ctx);
            let names: Vec<_> = found.iter().filter_map(|p| p.file_name()?.to_str()).collect();
            let found = names
                .first()
                .map_or(String::new(), |_| format!(" (found {})", names.join(", ")));
            let what = format!("{} missing{found}", plist.display());
            rows.push(format!(
                "✗ launchd — no plist runs drover serve: {what} → see docs/setup.md"
            ));
        }
        true => {
            let uid = std::fs::metadata(&ctx.home).map_or(0, |meta| meta.uid());
            let target = format!("gui/{uid}/{label}");
            let fix = format!("launchctl bootstrap gui/$(id -u) {}", plist.display());
            let line = format!("launchd — {target} loaded → {fix}");
            row(rows, ok(r, "launchctl", &["print", &target]).is_ok(), '✗', &line);
        }
    }
    rows.extend(crate::update::paused_line(
        r,
        &crate::events::working(&ctx.state).unwrap_or_default(),
    ));
    rows.extend(login_token(r, None));
    let paired = crate::devices::load(ctx).len();
    let line = format!("phone — {paired} paired → open the page, then drover serve approve <code>");
    row(rows, paired > 0, '!', &line);
    let key = ctx.state.with_file_name("webpush/vapid.key");
    let perm = mode(&key);
    let line = format!("vapid.key — {perm} → drover push test && chmod 600 {}", key.display());
    row(rows, perm == "mode 600", '✗', &line);
    let subs = crate::webpush::load(&ctx.state).len();
    let notifier = cfg.webpush_sub().is_some();
    let line = format!("push — {subs} subscriptions, notifier {notifier} → turn alerts on; add a webpush [[notify]]");
    row(rows, subs > 0 && notifier, '!', &line);
    let running = crate::timer::running(ctx, cfg);
    for what in ["check", "drive"] {
        let stamp = std::fs::read_to_string(ctx.state.with_file_name(format!("{what}.at")));
        let at = stamp.ok().and_then(|text| text.trim().parse::<u64>().ok());
        let age = at.map_or("never".into(), |at| crate::events::age(ctx.now - at));
        let fresh = at.is_some_and(|at| ctx.now <= at + crate::timer::stuck_secs(cfg));
        let ran = match running.filter(|_| !fresh) {
            Some(since) => format!("running for {}", crate::events::age(ctx.now.saturating_sub(since))),
            None => format!("ran {age}"),
        };
        let line = format!("watchdog {what} — {ran} → drover timer install");
        row(rows, fresh || running.is_some(), '✗', &line);
    }
    let stuck = crate::timer::stuck_step(ctx, cfg);
    let line = stuck.as_ref().map_or("tick step — advancing".into(), |(step, secs)| {
        let age = crate::events::age(*secs);
        format!("tick step — stuck in {step} for {age} → stop the tick; what it waits on is not answering")
    });
    row(rows, stuck.is_none(), '✗', &line);
    for (label, state) in crate::timer::drift(ctx, cfg, r) {
        let line = format!("timer — {label} {state} → drover timer install (the owner or an orchestrator)");
        row(rows, state == "loaded", '✗', &line);
    }
    for name in cfg.hosts.iter().filter(|h| h.1.cloud.is_some()).map(|h| h.0) {
        if let Err(e) = crate::cloud::doctor(ctx, cfg, r, name, rows) {
            rows.push(format!("✗ cloud {name} — {e:#}"));
        }
    }
    crate::cloud::includes(ctx, rows);
    if cfg.shed().is_some() {
        shed_rows(ctx, cfg, r, rows);
    }
    for (name, h) in cfg.hosts.iter().filter(|h| h.1.enabled) {
        let Some(m) = h.machine.as_deref() else { continue };
        let on = crate::hosts::On::new(r, Some(m));
        let Some((tags, home)) = crate::hosts::probe(&on) else {
            let ssh = |e: anyhow::Error| format!("unreachable (ssh: {e:#})");
            let why = ok(&on, "true", &[] as &[&str]).map_or_else(ssh, |_| "probe: unreadable answer".into());
            rows.push(format!("✗ host {name} — {why} → ssh {m} true"));
            continue;
        };
        let tags = [tags, h.tags.clone()].concat();
        let bad = crate::hosts::health(ctx, cfg, &on, &tags, &home);
        let h = crate::providers::lanes().filter_map(|h| h.health_check());
        let logins: String = h.map(|c| format!("{}, ", c.0)).collect();
        let v = crate::VERSION;
        let ok = format!("✓ host {name} — git, gh, {logins}drover {v}, shed");
        rows.extend(bad.iter().map(|w| w.replacen(' ', &format!(" host {name} "), 1)));
        rows.extend(bad.is_empty().then_some(ok));
        rows.extend(login_token(&on, Some(name)));
        let conf = crate::hosts::rehome(vec![ctx.config.display().to_string()], Some(&home)).remove(0);
        if on.exists(Path::new(&conf)) {
            rows.push(format!("! stale fleet.toml on {name}: no longer read → rm {conf}"));
        }
    }
}

const STARTUP: [&str; 4] = [".zshenv", ".zprofile", ".zshrc", ".zlogin"];

fn login_token(r: &dyn Runner, host: Option<&str>) -> Option<String> {
    let crate::providers::Credential::Account(a) = crate::providers::accounts()?.credential() else {
        return None;
    };
    let var = a.token_env?;
    let probe = format!(
        "t=$(env -i HOME=\"$HOME\" zsh -ilc 'echo \"token:${{{var}:+set}}\"' </dev/null 2>/dev/null); \
         case \"$t\" in *token:set*) echo token:set; cd \"$HOME\" && \
         grep -l {var} {} 2>/dev/null | sed 's/^/file:/';; esac",
        STARTUP.join(" ")
    );
    let out = ok(r, "sh", &["-c", &probe]).ok()?;
    out.lines().any(|l| l == "token:set").then_some(())?;
    let files: Vec<String> = out
        .lines()
        .filter_map(|l| l.strip_prefix("file:"))
        .filter(|f| STARTUP.contains(f))
        .map(|f| format!("~/{f}"))
        .collect();
    let files = if files.is_empty() {
        "your shell startup files".into()
    } else {
        files.join(", ")
    };
    let on = host.map_or(String::new(), |h| format!(" on host {h}"));
    Some(format!(
        "! a login shell{on} exports {var} (it overrides each lane's account) → remove it from {files}"
    ))
}

/// The launchd job: a plist under ~/Library/LaunchAgents that runs `drover serve`.
pub fn serve_plist(ctx: &Ctx) -> Option<std::path::PathBuf> {
    serve_plists(ctx).into_iter().next()
}

fn serve_plists(ctx: &Ctx) -> Vec<std::path::PathBuf> {
    let agents = std::fs::read_dir(ctx.home.join("Library/LaunchAgents"));
    let serves = |text: String| {
        (text.contains("drover") || text.contains("herdsman")) && text.contains("<string>serve</string>")
    };
    let plists = agents.into_iter().flatten().flatten().map(|entry| entry.path());
    let mut found: Vec<_> = plists
        .filter(|plist| std::fs::read_to_string(plist).is_ok_and(serves))
        .collect();
    found.sort();
    found
}

fn shed_rows(ctx: &Ctx, cfg: &Config, r: &dyn Runner, rows: &mut Vec<String>) {
    let path = crate::timer::job_path(ctx, cfg, r);
    let find = |b: &str| path.split(':').map(|d| Path::new(d).join(b)).find(|p| p.is_file());
    rows.push(find("shed").map_or_else(
        || "✗ shed — not on the timer's PATH → drover timer install from a shell where `which shed` finds it".into(),
        |p| format!("✓ shed — {} on the timer's PATH", p.display()),
    ));
    let ids: BTreeSet<String> = cfg.fleet.keys().filter_map(|n| cfg.shed_for(n)?.acp).collect();
    let agents = ids
        .first()
        .and_then(|_| crate::shed::cli(r, &["agents", "--json"]).ok());
    let agents: serde_json::Value = agents
        .and_then(|o| serde_json::from_str(&o.stdout).ok())
        .unwrap_or_default();
    for id in ids {
        let a = agents.as_array().into_iter().flatten().find(|a| a["id"] == id.as_str());
        let Some(cmd) = a.and_then(|a| a["cmd"][0].as_str()) else {
            rows.push(format!("✗ shed agent {id} — not installed → shed agents add {id}"));
            continue;
        };
        let found = if cmd.contains('/') {
            Path::new(cmd).is_file()
        } else {
            find(cmd).is_some()
        };
        let line =
            format!("shed agent {id} — {cmd} not on the timer's PATH → install {cmd} in a folder on it ({path})");
        row(rows, found, '✗', &line);
    }
}
