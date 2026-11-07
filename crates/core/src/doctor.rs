//! `herdsman doctor`: read-only host check, a `✓|✗|! <check> — <detail> [→ <fix>]` row each. Never prints a secret.
use crate::config::Config;
use crate::runner::{argv, ok, Runner};
use crate::Ctx;
use anyhow::Result;
use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// Add `text`: ✓ without its ` → <fix>` when `pass`, else marked `bad` (✗, or ! for a warning).
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
    // #539 `--config-only`, the pre-install gate: only whether this binary loads the live config (warnings are fine).
    if !config_only {
        if let Some((client, server, older)) = crate::timer::herdr_versions(r) {
            let line = format!("herdr — client {client}, server {server} → herdr update --handoff");
            row(&mut rows, !older, '!', &line);
        } else {
            rows.push("✗ herdr — not on PATH, or no server → herdr".into());
        }
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

fn checks(ctx: &Ctx, cfg: &Config, r: &dyn Runner, rows: &mut Vec<String>) {
    config_rows(ctx, cfg, rows);
    if cfg.runtime.kind == crate::config::RuntimeKind::Berth {
        let fix = format!("set kind = \"shed\" in {}", ctx.config.display());
        rows.push(format!(
            "! runtime — kind = \"berth\" is deprecated, berth is now shed → {fix}"
        ));
    }
    let line = crate::drive::spawns_line(ctx, cfg, r, &crate::events::read(&ctx.state).unwrap_or_default());
    row(rows, !line.contains(" · stale"), '!', &line); // #333
    let (n401, n503) = crate::devices::denials(ctx); // #501: a 503 is the board's devices unread
    let log = ctx.state.with_file_name("serve.log");
    let line = format!("board auth — 24 h: {n401} × 401, {n503} × 503 → see {}", log.display());
    row(rows, n503 == 0, '!', &line);
    let line = format!("worktrees — {0} → mkdir -p {0}", ctx.worktrees.display());
    row(rows, ctx.worktrees.is_dir(), '✗', &line);
    for (name, fleet) in &cfg.fleet {
        let repo = &fleet.repo;
        let read = ok(r, "gh", &["repo", "view", repo, "--json", "name"]).is_ok();
        row(rows, read, '✗', &format!("gh {name} — read {repo} → gh auth refresh"));
        let dir = fleet.local_path().display().to_string();
        let branch = &fleet.integration;
        let origin = format!("origin/{branch}");
        let git = ok(r, "git", &["-C", &dir, "rev-parse", "--verify", &origin]).is_ok();
        let fix = format!("git -C {dir} fetch origin {branch} || git clone https://github.com/{repo}.git {dir}");
        row(rows, git, '✗', &format!("local {name} — {dir} has {origin} → {fix}"));
        // #350: an unknown `mode` silently means full-first.
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
        // #350: each `fast` check must be one the repo's PRs report.
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
        // #173: a broken pipeline stops only its fleet; each rule it breaks, with its fleet.toml line.
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
    for (name, var) in &cfg.accounts.names {
        let svc = crate::secret::service("all", var);
        let line = format!("account {name} — {svc} in the store → herdsman accounts import <tokens.env>");
        let bad = if cfg.legacy_secret(var).is_some() { '!' } else { '✗' }; // #539: the retired file still serves it
        let got = r.secret(&svc);
        // #548: an item the Keychain guards with a dialog gets its one-time fix.
        if got.as_ref().is_err_and(|e| {
            e.downcast_ref::<crate::gate::Refusal>()
                .is_some_and(|f| f.code == "KEYCHAIN_LOCKED")
        }) {
            let fix = crate::runner::keychain_fix(&svc);
            row(
                rows,
                false,
                '✗',
                &format!("account {name} — {svc} prompts for Keychain access → approve herdsman in the Keychain dialog: {fix}"),
            );
            continue;
        }
        row(rows, got.is_ok_and(|v| v.is_some()), bad, &line);
    }
    // #556: signed by the one local identity, the keychain's "Always Allow" outlasts a reinstall.
    if cfg!(target_os = "macos") {
        let req = [
            "-v",
            "-R",
            "=certificate leaf[subject.CN] = \"herdsman-local\"",
            &ctx.exe,
        ];
        let signed = ok(r, "codesign", &req).is_ok();
        // #568: the identity is there but signing never took: codesign waits on the key's dialog.
        let has = !signed && ok(r, "security", &["find-certificate", "-c", "herdsman-local"]).is_ok();
        let fix = if has {
            format!("security set-key-partition-list -S apple-tool:,apple:,codesign: -s ~/Library/Keychains/login.keychain-db (asks the login password), then sh scripts/signing-identity.sh sign {}", ctx.exe)
        } else {
            "sh scripts/signing-identity.sh in the herdsman checkout (once), then reinstall".into()
        };
        let line = format!("signature — {} signed by \"herdsman-local\" → {fix}", ctx.exe);
        row(rows, signed, '✗', &line);
    }
    let roles: Vec<_> = cfg.fleet.values().flat_map(|fleet| &fleet.role).collect();
    let harnesses: BTreeSet<_> = roles.iter().map(|role| role.1.harness.as_str()).collect();
    for binary in harnesses {
        let line = format!("harness — {binary} on PATH → install {binary}");
        row(rows, ok(r, "which", &[binary]).is_ok(), '✗', &line);
    }
    let hermes = roles.iter().filter(|role| role.1.harness == "hermes");
    let hermes: BTreeSet<_> = hermes.map(|role| role.1.profile_or(role.0)).collect();
    for profile in &hermes {
        let line = format!("hermes {profile} — profile → hermes profile create {profile}");
        row(rows, ctx.hermes.join(profile).join("config.yaml").exists(), '✗', &line);
    }
    if !hermes.is_empty() {
        let synced = crate::spawn::profiles_check(ctx, cfg, r, &mut std::io::sink()).unwrap_or(false);
        let line = "hermes hooks — profiles sync --check → herdsman profiles sync";
        row(rows, synced, '✗', line);
    }
    let hosts = cfg.serve.allowed_hosts.join(", ");
    let line = format!("serve hosts — [{hosts}] → set [serve] allowed_hosts = [\"<name>.ts.net\"]");
    row(rows, !hosts.is_empty(), '✗', &line);
    let addr = "127.0.0.1:7777";
    let line = format!("serve — listening on {addr} → herdsman serve --addr {addr}");
    let up = ok(r, "lsof", &["-nP", "-iTCP:7777", "-sTCP:LISTEN"]).is_ok();
    row(rows, up, '✗', &line);
    let tailscale = r.run("tailscale", &argv(&["serve", "status"]));
    let line = format!("tailscale — serve maps to {addr} → tailscale serve --bg --https=8443 http://{addr}");
    let bad = if tailscale.is_err() { '!' } else { '✗' }; // not installed is a warning
    let maps = tailscale.is_ok_and(|o| o.code == 0 && o.stdout.contains(addr));
    row(rows, maps, bad, &line);
    match serve_plist(ctx) {
        _ if !cfg!(target_os = "macos") => {}
        None => rows.push("✗ launchd — no plist runs herdsman serve → see docs/setup.md".into()),
        Some(plist) => {
            let label = plist.file_stem().unwrap_or_default().to_string_lossy();
            let uid = std::fs::metadata(&ctx.home).map_or(0, |meta| meta.uid());
            let target = format!("gui/{uid}/{label}");
            let fix = format!("launchctl bootstrap gui/$(id -u) {}", plist.display());
            let line = format!("launchd — {target} loaded → {fix}");
            row(rows, ok(r, "launchctl", &["print", &target]).is_ok(), '✗', &line);
        }
    }
    let paired = crate::devices::load(ctx).len();
    let line = format!("phone — {paired} paired → open the page, then herdsman serve approve <code>");
    row(rows, paired > 0, '!', &line);
    let key = ctx.state.with_file_name("webpush/vapid.key");
    let perm = mode(&key);
    let line = format!("vapid.key — {perm} → herdsman push test && chmod 600 {}", key.display());
    row(rows, perm == "mode 600", '✗', &line);
    let subs = crate::webpush::load(&ctx.state).len();
    let notifier = cfg.webpush_sub().is_some();
    let line = format!("push — {subs} subscriptions, notifier {notifier} → turn alerts on; add a webpush [[notify]]");
    row(rows, subs > 0 && notifier, '!', &line);
    // `check` and `drive` stamp `<state>/<what>.at` each run: the cron's proof.
    for what in ["check", "drive"] {
        let stamp = std::fs::read_to_string(ctx.state.with_file_name(format!("{what}.at")));
        let at = stamp.ok().and_then(|text| text.trim().parse::<u64>().ok());
        let age = at.map_or("never".into(), |at| crate::events::age(ctx.now - at));
        let line = format!("watchdog {what} — ran {age} → herdsman timer install");
        row(rows, at.is_some_and(|at| ctx.now < at + 900), '✗', &line);
    }
    for (label, state) in crate::timer::drift(ctx, cfg, r) {
        let line = format!("timer — {label} {state} → herdsman timer install (the owner or a conductor)");
        row(rows, state == "loaded", '✗', &line);
    }
    if cfg.shed().is_some() {
        shed_rows(ctx, cfg, r, rows);
    }
    // #235: each enabled remote host's health, its failures as rows `host <name> <check>`.
    for (name, h) in cfg.hosts.iter().filter(|h| h.1.enabled) {
        let Some(m) = h.machine.as_deref() else { continue };
        let on = crate::hosts::On::new(r, Some(m));
        let Some((tags, home)) = crate::hosts::probe(&on).filter(|_| crate::host::agents(&on).is_ok()) else {
            rows.push(format!(
                "✗ host {name} — unreachable → herdr machine status; ssh {m} true"
            ));
            continue;
        };
        let tags = crate::hosts::tags(
            h,
            Some(&crate::hosts::Seen {
                tags,
                ..Default::default()
            }),
        );
        let bad = crate::hosts::health(ctx, cfg, &on, &tags, &home);
        let h = crate::harness::all().filter_map(|h| h.1.health_check());
        let logins: String = h.map(|c| format!("{}, ", c.0)).collect();
        let ok = format!(
            "✓ host {name} — herdr, session, git, gh, {logins}herdsman {}",
            crate::VERSION
        );
        rows.extend(bad.iter().map(|w| w.replacen(' ', &format!(" host {name} "), 1)));
        rows.extend(bad.is_empty().then_some(ok));
        // #592: lanes there read only their rules file; the owner removes an old copy by hand, herdsman never does.
        let conf = crate::hosts::rehome(vec![ctx.config.display().to_string()], Some(&home)).remove(0);
        if on.exists(Path::new(&conf)) {
            rows.push(format!(
                "! stale fleet.toml on {name}: no longer read → rm {conf} there"
            ));
        }
    }
}

/// The launchd job: a plist under ~/Library/LaunchAgents that runs `herdsman serve`.
pub fn serve_plist(ctx: &Ctx) -> Option<std::path::PathBuf> {
    let agents = std::fs::read_dir(ctx.home.join("Library/LaunchAgents"));
    let serves = |text: String| text.contains("herdsman") && text.contains("<string>serve</string>");
    let mut plists = agents.into_iter().flatten().flatten().map(|entry| entry.path());
    plists.find(|plist| std::fs::read_to_string(plist).is_ok_and(serves))
}

/// #600: shed, and each ACP adapter's command (`shed agents --json`), on the PATH the timer's jobs run with.
fn shed_rows(ctx: &Ctx, cfg: &Config, r: &dyn Runner, rows: &mut Vec<String>) {
    let path = crate::timer::job_path(ctx, cfg, r);
    let find = |b: &str| path.split(':').map(|d| Path::new(d).join(b)).find(|p| p.is_file());
    rows.push(find("shed").map_or_else(
        || "✗ shed — not on the timer's PATH → herdsman timer install from a shell where `which shed` finds it".into(),
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
