//! Paired phones for the page's reply box (docs/serve.md): only each token's SHA-256 is kept.
use crate::config::Config;
use crate::events::{append, emit, read, Event};
use crate::gate::refused;
use crate::Ctx;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// #59: a `code` is a phone's open pairing request (from `ip`, 10 min from `created`): its cookie works once `serve approve` clears it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Device {
    pub hash: String,
    pub label: String,
    pub created: u64,
    pub used: u64,
    pub code: Option<String>,
    pub ip: String,
}

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.state.parent().unwrap_or(&ctx.state).join("serve")
}

/// Write `v` as JSON, mode 0600.
pub(crate) fn save(p: &Path, v: &impl Serialize) -> Result<()> {
    write0600(p, serde_json::to_string_pretty(v)?.as_bytes())
}

/// Replace `p` with `b`, mode 0600, atomically (#122): a temp file renamed over `p`; on any error `p` is left intact.
pub(crate) fn write0600(p: &Path, b: &[u8]) -> Result<()> {
    std::fs::create_dir_all(p.parent().unwrap_or(p))?;
    let tmp = PathBuf::from(format!("{}.tmp-{}", p.display(), std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let r = (|| {
        let mut o = std::fs::OpenOptions::new();
        let mut f = o.write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(b)?;
        f.sync_all()?;
        std::fs::rename(&tmp, p)?;
        let _ = std::fs::File::open(p.parent().unwrap_or(p)).and_then(|d| d.sync_all());
        Ok::<_, io::Error>(())
    })();
    Ok(r.inspect_err(|_| drop(std::fs::remove_file(&tmp)))?)
}

pub(crate) fn random(n: usize) -> Result<Vec<u8>> {
    let mut b = vec![0; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

pub fn load(ctx: &Ctx) -> Vec<Device> {
    std::fs::read_to_string(dir(ctx).join("devices.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// #122: a failed write logs `state-write-failed` (notified, path and error kind only), at most once an hour.
pub fn store(ctx: &Ctx, d: &[Device]) -> Result<()> {
    let fresh = |e: &Event| e.kind == "state-write-failed" && e.ts + 3600 > ctx.now;
    let p = dir(ctx).join("devices.json");
    let r = save(&p, &d);
    let seen = || read(&ctx.state).unwrap_or_default().iter().any(fresh);
    if let Some(e) = r.as_ref().err().filter(|_| !seen()) {
        let kind = e.downcast_ref().map_or(io::ErrorKind::Other, io::Error::kind);
        let d = json!({"path": p, "error": kind.to_string(), "reason": format!("{}: {kind}", p.display())});
        let ev = Event::new(ctx.now, "herdsman", None, "state-write-failed", d);
        let _ = emit(&ctx.state, &Config::load(&ctx.config).unwrap_or_default(), ev);
    }
    r
}

/// The open (unexpired) pairing requests.
pub fn pending(ctx: &Ctx) -> Vec<Device> {
    load(ctx)
        .into_iter()
        .filter(|d| d.code.is_some() && d.created + 600 > ctx.now)
        .collect()
}

/// A pairing request: (its code, unique among the open ones; the `hd=` token for the asking browser alone); None while 3 are open, 2 from `ip`, or on a code clash.
pub fn request(ctx: &Ctx, label: &str, ip: &str) -> Result<Option<(String, String)>> {
    let (mut all, b, token) = (
        load(ctx),
        random(4)?,
        random(32)?.iter().map(|b| format!("{b:02x}")).collect::<String>(),
    );
    all.retain(|d| d.code.is_none() || d.created + 3600 > ctx.now);
    let (open, code, hash) = (
        pending(ctx),
        format!("{:06}", u32::from_be_bytes([b[0], b[1], b[2], b[3]]) % 1_000_000),
        sha256(token.as_bytes()),
    );
    if open.len() >= 3
        || open.iter().filter(|d| d.ip == ip).count() >= 2
        || open.iter().any(|d| d.code.as_ref() == Some(&code))
    {
        return Ok(None);
    }
    all.push(Device {
        label: format!("{label} {}", &hash[..4]),
        hash,
        created: ctx.now,
        used: ctx.now,
        code: Some(code.clone()),
        ip: ip.into(),
    });
    store(ctx, &all)?;
    append(
        &ctx.state,
        &Event::new(ctx.now, "herdsman", None, "pair-request", json!({"device": label})),
    )?;
    Ok(Some((code, token)))
}

/// `serve approve <code>`: pair the request's browser (single use); its device label. Logged without code or token.
pub fn approve(ctx: &Ctx, code: &str) -> Result<String> {
    let mut all = load(ctx);
    let Some(d) = all
        .iter_mut()
        .find(|d| d.code.as_deref().is_some_and(|c| same(c, code.trim())))
    else {
        return refused(
            "NO_SUCH_CODE",
            "no pairing request with that code: tap Pair on the phone",
        );
    };
    if d.created + 600 <= ctx.now {
        return refused("EXPIRED", "that pairing code expired: tap Pair on the phone again");
    }
    let label = {
        (d.code, d.created, d.used) = (None, ctx.now, ctx.now);
        d.label.clone()
    };
    store(ctx, &all)?;
    append(
        &ctx.state,
        &Event::new(ctx.now, "herdsman", None, "pair-approve", json!({"device": label})),
    )?;
    Ok(label)
}

/// Constant-time string equality (codes, token hashes).
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// The paired (approved) device whose token the `Cookie` header carries (`hd=`); marks it used at most hourly (#122).
pub fn find(ctx: &Ctx, cookie: &str) -> Option<Device> {
    let tok = sha256(cookie.split(';').find_map(|c| c.trim().strip_prefix("hd="))?.as_bytes());
    let mut all = load(ctx);
    let i = all.iter().position(|d| d.code.is_none() && same(&d.hash, &tok))?;
    let stale = std::mem::replace(&mut all[i].used, ctx.now) + 3600 <= ctx.now;
    let _ = stale.then(|| store(ctx, &all));
    Some(all.swap_remove(i))
}

/// SHA-256, lowercase hex.
pub fn sha256(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}
