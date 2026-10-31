//! Paired phones for the page's reply box (docs/serve.md): only each token's SHA-256 is kept.
use crate::Ctx;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Device {
    pub hash: String,
    pub label: String,
    pub created: u64,
    pub used: u64,
}

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.state.parent().unwrap_or(&ctx.state).join("serve")
}

/// Write `v` as JSON, mode 0600.
pub(crate) fn save(p: &Path, v: &impl Serialize) -> Result<()> {
    write0600(p, serde_json::to_string_pretty(v)?.as_bytes())
}

/// Replace `p` with `b`, mode 0600.
pub(crate) fn write0600(p: &Path, b: &[u8]) -> Result<()> {
    std::fs::create_dir_all(p.parent().unwrap_or(p))?;
    let _ = std::fs::remove_file(p);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)?;
    Ok(f.write_all(b)?)
}

pub(crate) fn random(n: usize) -> Result<Vec<u8>> {
    let mut b = vec![0; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

pub fn load(ctx: &Ctx) -> Vec<Device> {
    std::fs::read_to_string(dir(ctx).join("devices.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn store(ctx: &Ctx, d: &[Device]) -> Result<()> {
    save(&dir(ctx).join("devices.json"), &d)
}

/// A fresh pairing code; replaces any earlier one.
pub fn new_code(ctx: &Ctx) -> Result<String> {
    let b = random(4)?;
    let code = format!("{:06}", u32::from_be_bytes([b[0], b[1], b[2], b[3]]) % 1_000_000);
    save(&dir(ctx).join("pair.json"), &json!({"hash": sha256(code.as_bytes()), "expires": ctx.now + 300}))?;
    Ok(code)
}

/// Spend the pending code (any attempt burns it): the new device's cookie token if it matched in time.
pub fn redeem(ctx: &Ctx, code: &str, label: &str) -> Result<Option<String>> {
    let p = dir(ctx).join("pair.json");
    let v: serde_json::Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let _ = std::fs::remove_file(&p);
    if v["hash"].as_str() != Some(&sha256(code.trim().as_bytes())) || v["expires"].as_u64().unwrap_or(0) < ctx.now {
        return Ok(None);
    }
    let token: String = random(32)?.iter().map(|b| format!("{b:02x}")).collect();
    let mut all = load(ctx);
    let hash = sha256(token.as_bytes());
    all.push(Device { label: format!("{label} {}", &hash[..4]), hash, created: ctx.now, used: ctx.now });
    store(ctx, &all)?;
    Ok(Some(token))
}

/// The paired device whose token the `Cookie` header carries (`hd=`); marks it used.
pub fn find(ctx: &Ctx, cookie: &str) -> Option<Device> {
    let tok = cookie.split(';').find_map(|c| c.trim().strip_prefix("hd="))?;
    let mut all = load(ctx);
    let d = all.iter_mut().find(|d| d.hash == sha256(tok.as_bytes()))?;
    d.used = ctx.now;
    let d = d.clone();
    store(ctx, &all).ok()?;
    Some(d)
}

/// SHA-256, lowercase hex.
pub fn sha256(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}
