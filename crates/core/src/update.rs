//! #557 `herdsman self-update`, the `dev.herdsman.update` job (every 5 min), no Hermes: the herdsman fleet's
//! integration head is installed once its CI is all green. Built beside the live binary, signed with "herdsman-local"
//! (#556), gated by the new binary's `doctor --config-only` on the live config (#539) and its `doctor --tokens` read
//! through `herdsman-keys` (#631, reinstalled only when its source changes), swapped in atomically, then
//! `profiles sync` and a serve restart. A failed step keeps the old binary and alerts once per head.
use crate::config::Config;
use crate::events::{emit, read, Event};
use crate::runner::{ok, Runner};
use crate::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `herdsman self-update [--dry-run | --pause | --resume]`.
pub fn self_update(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    mode: (bool, bool, bool),
) -> Result<i32> {
    let (dry, pause, resume) = mode;
    let events = read(&ctx.state)?;
    if pause || resume {
        let (kind, word) = [("self-update-resumed", "resumed"), ("self-update-paused", "paused")][usize::from(pause)];
        let by = std::env::var("HERDSMAN_LANE").ok().filter(|l| !l.is_empty());
        let d = json!({"by": by.as_deref().unwrap_or("owner")});
        emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, kind, d))?;
        writeln!(out, "✓ self-update {word}")?;
        return Ok(0);
    }
    let toggle = events
        .iter()
        .rfind(|e| e.kind == "self-update-paused" || e.kind == "self-update-resumed");
    if toggle.is_some_and(|e| e.kind == "self-update-paused") {
        writeln!(out, "= self-update paused → herdsman self-update --resume")?;
        return Ok(0);
    }
    let Some(f) = cfg.fleet.values().find(|f| f.repo_name() == "herdsman") else {
        writeln!(out, "= self-update: no fleet for the herdsman repo")?;
        return Ok(0);
    };
    let local = f.local_path();
    let l = local.to_string_lossy();
    let git = |a: &[&str]| ok(r, "git", &[&["-C", &*l], a].concat());
    git(&["fetch", "-q", "origin", &f.integration])?;
    let sha = git(&["rev-parse", &format!("origin/{}", f.integration)])?;
    let (sha, home) = (sha.trim(), ctx.state.with_file_name("update"));
    if std::fs::read_to_string(home.join("sha")).is_ok_and(|s| s.trim() == sha) {
        writeln!(out, "= up to date @ {sha:.7}")?;
        return Ok(0);
    }
    let (ci, checks) = crate::land::suite(r, f, sha);
    if ci != "green" {
        let first = crate::gh::not_green(&checks).map_or(String::new(), |c| format!(": {c}"));
        writeln!(out, "= {} @ {sha:.7}: CI {ci}{first}; not installed", f.integration)?;
        return Ok(0);
    }
    let live = std::fs::canonicalize(&ctx.exe).unwrap_or_else(|_| ctx.exe.clone().into());
    let new = PathBuf::from(format!("{}.new", live.display()));
    if dry {
        let plan = [
            format!("would build {sha:.7} in {} (CI green)", home.join("src").display()),
            format!(
                "would sign it \"herdsman-local\", run its doctor --config-only and --tokens, then swap it in at {}",
                live.display()
            ),
            "would run profiles sync and restart serve".into(),
        ];
        writeln!(out, "{}", plan.join("\n"))?;
        return Ok(0);
    }
    let keys = live.with_file_name("herdsman-keys");
    let staged = stage(r, &local, &home, sha, &new).and_then(|hash| {
        let k = place_keys(&home, &keys, hash.as_deref())?;
        gate(r, &home.join("src"), &new, &k).map(|()| (k, hash))
    });
    let res = staged.and_then(|(k, hash)| {
        if k != keys {
            std::fs::rename(&k, &keys).context("swap in herdsman-keys")?;
            std::fs::write(keys.with_extension("src"), hash.unwrap_or_default())?;
        }
        std::fs::rename(&new, &live).with_context(|| format!("swap into {}", live.display()))?;
        std::fs::write(home.join("sha"), sha)?;
        let synced = ok(r, &live.to_string_lossy(), &["profiles", "sync"]).context("installed, then profiles sync");
        synced.and_then(|_| crate::timer::restart(ctx, cfg, r).context("installed, then the serve restart"))
    });
    let Err(e) = res else {
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, "herdsman", None, "self-updated", json!({"sha": sha})),
        )?;
        writeln!(
            out,
            "✓ installed {sha:.7} at {}; profiles synced, serve restarted",
            live.display()
        )?;
        return Ok(0);
    };
    let _ = std::fs::remove_file(&new);
    let why = format!("{sha:.7}: {e:#}");
    let kept = if format!("{e:#}").starts_with("installed, then") {
        "now live at"
    } else {
        "kept"
    };
    writeln!(out, "✗ self-update {why}; {kept} {}", live.display())?;
    if !events
        .iter()
        .any(|e| e.kind == "self-update-failed" && e.str("key") == Some(sha))
    {
        let d = json!({"key": sha, "reason": format!("self-update {why}")});
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, "herdsman", None, "self-update-failed", d),
        )?;
    }
    Ok(1)
}

/// #631: the helper's source; the sha256 of these files is its version (`herdsman-keys.src` beside it).
const KEYS_SRC: [&str; 2] = ["crates/keys/Cargo.toml", "crates/keys/src/main.rs"];

/// Check `sha` out in herdsman's own worktree (`<home>/src`), build it there and copy the binary to `new`; the
/// helper's source hash (None: a head from before it).
fn stage(r: &dyn Runner, local: &Path, home: &Path, sha: &str, new: &Path) -> Result<Option<String>> {
    let (src, target) = (home.join("src"), home.join("target"));
    let (s, t) = (src.to_string_lossy(), target.to_string_lossy());
    std::fs::create_dir_all(home)?;
    if src.join(".git").exists() {
        ok(r, "git", &["-C", &s, "checkout", "-q", "--detach", sha]).context("checkout")?;
    } else {
        let l = local.to_string_lossy();
        ok(r, "git", &["-C", &l, "worktree", "add", "-q", "--detach", &s, sha]).context("checkout")?;
    }
    let manifest = src.join("Cargo.toml");
    let files: Option<Vec<_>> = KEYS_SRC.iter().map(|f| std::fs::read(src.join(f)).ok()).collect();
    let hash = files.map(|f| crate::devices::sha256(&f.concat()));
    let keys: &[&str] = if hash.is_some() { &["-p", "herdsman-keys"] } else { &[] };
    let build = ["build", "--release", "--locked", "-p", "herdsman"];
    let rest = ["--manifest-path", &manifest.to_string_lossy(), "--target-dir", &t];
    ok(r, "cargo", &[&build[..], keys, &rest].concat()).context("build")?;
    std::fs::copy(target.join("release/herdsman"), new).context("copy the build")?;
    Ok(hash)
}

/// #631: where the gate finds the build's `herdsman-keys`: `keys` itself while its source hash is the installed one's
/// (never reinstalled, so its cdhash and the owner's approval stay) and the first time (nothing reads it yet), else
/// `herdsman-keys.new`, kept on a failed gate so the owner can authorise it.
fn place_keys(home: &Path, keys: &Path, hash: Option<&str>) -> Result<PathBuf> {
    let stamp = keys.with_extension("src");
    let same = |h: &str| std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == h);
    let Some(h) = hash.filter(|h| !same(h)) else {
        return Ok(keys.into());
    };
    let (built, first) = (home.join("target/release/herdsman-keys"), !keys.exists());
    let to = if first { keys.into() } else { keys.with_extension("new") };
    let _ = std::fs::remove_file(&to); // a fresh inode: macOS kills a signed binary rewritten in place
    std::fs::copy(built, &to).context("copy herdsman-keys")?;
    if first {
        std::fs::write(stamp, h)?;
    }
    Ok(to)
}

/// Sign `new` with the stable identity by hash (#556, #568: the build's own `signing-identity.sh sign`; refused
/// without it), run its `doctor --config-only` (#539) and its token check through `keys` (#631).
fn gate(r: &dyn Runner, src: &Path, new: &Path, keys: &Path) -> Result<()> {
    let n = new.to_string_lossy();
    if ok(r, "which", &["codesign"]).is_ok() {
        let script = src.join("scripts/signing-identity.sh");
        let o = r.run("sh", &crate::runner::argv(&[&script.to_string_lossy(), "sign", &n]))?;
        match o.code {
            0 => {}
            2 => bail!("sign: no \"herdsman-local\" identity → sh scripts/signing-identity.sh, once"),
            _ => bail!("sign: {}", format!("{}{}", o.stdout, o.stderr).trim()),
        }
    }
    let o = r.run(&n, &crate::runner::argv(&["doctor", "--config-only"]))?;
    if o.code != 0 {
        bail!("doctor --config-only on the live config: {}", o.stdout.trim());
    }
    let k = format!("HERDSMAN_KEYS={}", keys.display());
    let o = r.run("env", &crate::runner::argv(&[&k, &n, "doctor", "--tokens"]))?;
    if o.code != 0 {
        bail!(
            "doctor --tokens, the new build can't read the tokens: {}",
            o.stdout.trim()
        );
    }
    Ok(())
}
