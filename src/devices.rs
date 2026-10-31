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
    std::fs::create_dir_all(p.parent().unwrap_or(p))?;
    let _ = std::fs::remove_file(p);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)?;
    Ok(f.write_all(serde_json::to_string_pretty(v)?.as_bytes())?)
}

fn random(n: usize) -> Result<Vec<u8>> {
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

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
    0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
    0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
    0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
    0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256, lowercase hex (std has none; no crypto dependency for one hash).
pub fn sha256(data: &[u8]) -> String {
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut m = data.to_vec();
    m.push(0x80);
    m.resize(m.len() + (120 - m.len() % 64) % 64, 0);
    m.extend((data.len() as u64 * 8).to_be_bytes());
    for c in m.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..64 {
            w[i] = if i < 16 {
                u32::from_be_bytes([c[4 * i], c[4 * i + 1], c[4 * i + 2], c[4 * i + 3]])
            } else {
                let (a, b) = (w[i - 15], w[i - 2]);
                let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
                let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
                w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1)
            };
        }
        let mut v = h;
        for (k, w) in K.iter().zip(w) {
            let [a, b, c, d, e, f, g, hh] = v;
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let t1 = hh.wrapping_add(s1).wrapping_add((e & f) ^ (!e & g)).wrapping_add(*k).wrapping_add(w);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (x, y) in h.iter_mut().zip(v) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
