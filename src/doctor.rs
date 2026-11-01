//! `herdsman doctor`: read-only host check, a `✓|✗|! <check> — <detail> [→ <fix>]` row each. Never prints a secret.
use crate::config::{expand, Config};
use crate::runner::{argv, ok, Runner};
use crate::Ctx;
use anyhow::Result;
use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// Add `row`: ✓ without its ` → <fix>` when `ok`, else marked `bad` (✗, or ! for a warning).
fn row(d: &mut Vec<String>, ok: bool, bad: char, row: &str) {
    let cut = row.find(" → ").filter(|_| ok).unwrap_or(row.len());
    d.push(format!("{} {}", if ok { '✓' } else { bad }, &row[..cut]));
}

fn mode(p: &Path) -> String {
    let m = std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777);
    m.map_or("missing".into(), |m| format!("mode {m:o}"))
}

pub fn doctor(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, json: bool) -> Result<i32> {
    let mut d = vec![];
    let status = crate::herdr::status(r).unwrap_or_default();
    let v = status.lines().map(str::trim);
    let v: Vec<_> = v.filter_map(|l| l.strip_prefix("version: ")).collect();
    let ver = |v: &str| v.split('.').map(|n| n.parse().unwrap_or(0)).collect::<Vec<u32>>();
    if let [client, server, ..] = v[..] {
        let l = format!("herdr — client {client}, server {server} → herdr update --handoff");
        row(&mut d, ver(server) >= ver(client), '!', &l);
    } else {
        d.push("✗ herdr — not on PATH, or no server → herdr".into());
    }
    let h = crate::host::sample(r);
    let host = h.pressure.is_some() && h.swapout.is_some() && h.load_per_cpu.is_some();
    row(&mut d, host, '✗', "host — sysctl + vm_stat → put /usr/sbin on PATH");
    let gh = ok(r, "gh", &["auth", "status"]).is_ok();
    row(&mut d, gh, '✗', "gh — auth status → gh auth login");
    match Config::load(&ctx.config) {
        Ok(cfg) => checks(ctx, &cfg, r, &mut d),
        Err(e) => d.push(format!("✗ config — {e:#} → edit {}", ctx.config.display())),
    }
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&d)?)?;
    } else {
        writeln!(out, "{}", d.join("\n"))?;
    }
    Ok(i32::from(d.iter().any(|l| l.starts_with('✗'))))
}

fn checks(ctx: &Ctx, cfg: &Config, r: &dyn Runner, d: &mut Vec<String>) {
    d.push(format!("✓ config — {}", ctx.config.display()));
    let wt = ctx.worktrees.display();
    let l = format!("worktrees — {wt} → mkdir -p {wt}");
    row(d, ctx.worktrees.is_dir(), '✗', &l);
    for (name, f) in &cfg.fleet {
        let read = ok(r, "gh", &["repo", "view", &f.repo, "--json", "name"]).is_ok();
        row(d, read, '✗', &format!("gh {name} — read {} → gh auth refresh", f.repo));
        let dir = f.local_path().display().to_string();
        let b = &f.integration;
        let origin = format!("origin/{b}");
        let git = ok(r, "git", &["-C", &dir, "rev-parse", "--verify", &origin]).is_ok();
        let fix = format!("git -C {dir} fetch origin {b} || gh repo clone {} {dir}", f.repo);
        row(d, git, '✗', &format!("local {name} — {dir} has {origin} → {fix}"));
    }
    if let Some(path) = cfg.accounts.accounts_env.as_deref().map(expand) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let m = mode(&path);
        let p = path.display();
        let l = format!("accounts_env — {p} {m} → touch {p} && chmod 600 {p}");
        row(d, m == "mode 600", '✗', &l);
        for (name, var) in &cfg.accounts.names {
            let set = crate::access::has_var(&text, var);
            row(d, set, '✗', &format!("account {name} — {var} set → add {var}=… to {p}"));
        }
    }
    let roles: Vec<_> = cfg.fleet.values().flat_map(|f| &f.role).collect();
    for b in roles.iter().map(|r| r.1.harness.as_str()).collect::<BTreeSet<_>>() {
        let l = format!("harness — {b} on PATH → install {b}");
        row(d, ok(r, "which", &[b]).is_ok(), '✗', &l);
    }
    let hermes = roles.iter().filter(|r| r.1.harness == "hermes");
    let hermes: BTreeSet<_> = hermes.map(|r| r.1.profile_or(r.0)).collect();
    for p in &hermes {
        let l = format!("hermes {p} — profile → hermes profile create {p}");
        row(d, ctx.hermes.join(p).join("config.yaml").exists(), '✗', &l);
    }
    if !hermes.is_empty() {
        let synced = crate::spawn::profiles_check(ctx, cfg, r, &mut std::io::sink()).unwrap_or(false);
        let l = "hermes hooks — profiles sync --check → herdsman profiles sync";
        row(d, synced, '✗', l);
    }
    let hosts = cfg.serve.allowed_hosts.join(", ");
    let l = format!("serve hosts — [{hosts}] → set [serve] allowed_hosts = [\"<name>.ts.net\"]");
    row(d, !hosts.is_empty(), '✗', &l);
    let addr = "127.0.0.1:7777";
    let l = format!("serve — listening on {addr} → herdsman serve --addr {addr}");
    let up = ok(r, "lsof", &["-nP", "-iTCP:7777", "-sTCP:LISTEN"]).is_ok();
    row(d, up, '✗', &l);
    let ts = r.run("tailscale", &argv(&["serve", "status"]));
    let l = format!("tailscale — serve maps to {addr} → tailscale serve --bg --https=8443 http://{addr}");
    let bad = if ts.is_err() { '!' } else { '✗' }; // not installed is a warning
    row(d, ts.is_ok_and(|o| o.code == 0 && o.stdout.contains(addr)), bad, &l);
    // The launchd job: a plist under ~/Library/LaunchAgents that runs `herdsman serve`.
    let dir = std::fs::read_dir(ctx.home.join("Library/LaunchAgents"));
    let serves = |t: String| t.contains("herdsman") && t.contains("<string>serve</string>");
    let mut plists = dir.into_iter().flatten().flatten().map(|e| e.path());
    match plists.find(|p| std::fs::read_to_string(p).is_ok_and(serves)) {
        _ if !cfg!(target_os = "macos") => {}
        None => d.push("✗ launchd — no plist runs herdsman serve → see docs/setup.md".into()),
        Some(p) => {
            let label = p.file_stem().unwrap_or_default().to_string_lossy();
            let uid = std::fs::metadata(&ctx.home).map_or(0, |m| m.uid());
            let target = format!("gui/{uid}/{label}");
            let fix = format!("launchctl bootstrap gui/$(id -u) {}", p.display());
            let l = format!("launchd — {target} loaded → {fix}");
            row(d, ok(r, "launchctl", &["print", &target]).is_ok(), '✗', &l);
        }
    }
    let n = crate::devices::load(ctx).len();
    let l = format!("phone — {n} paired → open the page, then herdsman serve approve <code>");
    row(d, n > 0, '!', &l);
    let key = ctx.state.with_file_name("webpush/vapid.key");
    let m = mode(&key);
    let fix = [format!("chmod 600 {}", key.display()), "herdsman push test".into()];
    let l = format!("vapid.key — {m} → {}", fix[usize::from(m == "missing")]);
    row(d, m == "mode 600", '✗', &l);
    let n = crate::webpush::load(&ctx.state).len();
    let sub = cfg.webpush_sub().is_some();
    let l = format!("push — {n} subscriptions, webpush notifier {sub} → turn alerts on; add a webpush [[notify]]");
    row(d, n > 0 && sub, '!', &l);
    // `check` and `drive` stamp `<state>/<what>.at` each run: the cron's proof.
    let driven = cfg.fleet.values().any(crate::config::Fleet::driven);
    for what in ["check", "drive"].into_iter().filter(|w| *w == "check" || driven) {
        let at = std::fs::read_to_string(ctx.state.with_file_name(format!("{what}.at")));
        let at = at.ok().and_then(|s| s.trim().parse::<u64>().ok());
        let age = at.map_or("never".into(), |t| crate::events::age(ctx.now - t));
        let exe = &ctx.exe;
        let l = format!("watchdog {what} — last ran {age} → (crontab -l; echo '*/5 * * * * {exe} {what}') | crontab -");
        row(d, at.is_some_and(|t| ctx.now < t + 900), '✗', &l);
    }
}
