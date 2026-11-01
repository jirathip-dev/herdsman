//! `herdsman doctor`: read-only host check, a `✓|✗|! <check> — <detail> [→ <fix>]` row each. Never prints a secret.
use crate::config::{expand, Config};
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

pub fn doctor(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, json: bool) -> Result<i32> {
    let mut rows = vec![];
    let status = crate::herdr::status(r).unwrap_or_default();
    let versions = status.lines().map(str::trim);
    let versions: Vec<_> = versions.filter_map(|line| line.strip_prefix("version: ")).collect();
    let parse = |v: &str| v.split('.').map(|n| n.parse().unwrap_or(0)).collect::<Vec<u32>>();
    if let [client, server, ..] = versions[..] {
        let line = format!("herdr — client {client}, server {server} → herdr update --handoff");
        row(&mut rows, parse(server) >= parse(client), '!', &line);
    } else {
        rows.push("✗ herdr — not on PATH, or no server → herdr".into());
    }
    let sample = crate::host::sample(r);
    let host = sample.pressure.is_some() && sample.swapout.is_some() && sample.load_per_cpu.is_some();
    row(&mut rows, host, '✗', "host — sysctl + vm_stat → put /usr/sbin on PATH");
    let gh = ok(r, "gh", &["auth", "status"]).is_ok();
    row(&mut rows, gh, '✗', "gh — auth status → gh auth login");
    match Config::load(&ctx.config) {
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

fn checks(ctx: &Ctx, cfg: &Config, r: &dyn Runner, rows: &mut Vec<String>) {
    rows.push(format!("✓ config — {}", ctx.config.display()));
    let worktrees = ctx.worktrees.display();
    let line = format!("worktrees — {worktrees} → mkdir -p {worktrees}");
    row(rows, ctx.worktrees.is_dir(), '✗', &line);
    for (name, fleet) in &cfg.fleet {
        let repo = &fleet.repo;
        let read = ok(r, "gh", &["repo", "view", repo, "--json", "name"]).is_ok();
        row(rows, read, '✗', &format!("gh {name} — read {repo} → gh auth refresh"));
        let dir = fleet.local_path().display().to_string();
        let branch = &fleet.integration;
        let origin = format!("origin/{branch}");
        let git = ok(r, "git", &["-C", &dir, "rev-parse", "--verify", &origin]).is_ok();
        let fix = format!("git -C {dir} fetch origin {branch} || gh repo clone {repo} {dir}");
        row(rows, git, '✗', &format!("local {name} — {dir} has {origin} → {fix}"));
    }
    if let Some(path) = cfg.accounts.accounts_env.as_deref().map(expand) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let perm = mode(&path);
        let shown = path.display();
        let line = format!("accounts_env — {shown} {perm} → touch {shown} && chmod 600 {shown}");
        row(rows, perm == "mode 600", '✗', &line);
        for (name, var) in &cfg.accounts.names {
            let line = format!("account {name} — {var} set → add {var}=… to {shown}");
            row(rows, crate::access::has_var(&text, var), '✗', &line);
        }
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
    // The launchd job: a plist under ~/Library/LaunchAgents that runs `herdsman serve`.
    let agents = std::fs::read_dir(ctx.home.join("Library/LaunchAgents"));
    let serves = |text: String| text.contains("herdsman") && text.contains("<string>serve</string>");
    let mut plists = agents.into_iter().flatten().flatten().map(|entry| entry.path());
    match plists.find(|plist| std::fs::read_to_string(plist).is_ok_and(serves)) {
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
    let fix = [format!("chmod 600 {}", key.display()), "herdsman push test".into()];
    let line = format!("vapid.key — {perm} → {}", fix[usize::from(perm == "missing")]);
    row(rows, perm == "mode 600", '✗', &line);
    let subs = crate::webpush::load(&ctx.state).len();
    let notifier = cfg.webpush_sub().is_some();
    let line =
        format!("push — {subs} subscriptions, webpush notifier {notifier} → turn alerts on; add a webpush [[notify]]");
    row(rows, subs > 0 && notifier, '!', &line);
    // `check` and `drive` stamp `<state>/<what>.at` each run: the cron's proof.
    let driven = cfg.fleet.values().any(crate::config::Fleet::driven);
    for what in ["check", "drive"].into_iter().filter(|what| *what == "check" || driven) {
        let stamp = std::fs::read_to_string(ctx.state.with_file_name(format!("{what}.at")));
        let at = stamp.ok().and_then(|text| text.trim().parse::<u64>().ok());
        let age = at.map_or("never".into(), |at| crate::events::age(ctx.now - at));
        let exe = &ctx.exe;
        let line =
            format!("watchdog {what} — last ran {age} → (crontab -l; echo '*/5 * * * * {exe} {what}') | crontab -");
        row(rows, at.is_some_and(|at| ctx.now < at + 900), '✗', &line);
    }
}
