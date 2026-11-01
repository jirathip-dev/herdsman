//! Web Push to the page's Home Screen app (docs/serve.md): an RFC 8291 aes128gcm body, an ES256
//! VAPID JWT (RFC 8292), sent by `curl` with the body on stdin. The VAPID key and the subscriptions
//! are secrets: 0600 files, never logged, printed or served (only the public key and labels are).
use crate::devices::{random, save, write0600};
use crate::events::{finding, plain, read, Event};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes128Gcm;
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hkdf::Hkdf;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sub {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
    pub created: u64,
}

impl Sub {
    /// What the page shows and removes by: the endpoint itself never leaves the Mac.
    pub fn id(&self) -> String {
        crate::devices::sha256(self.endpoint.as_bytes())[..16].into()
    }
}

/// `<state>/webpush`, `log` being the event log.
fn dir(log: &Path) -> PathBuf {
    log.parent().unwrap_or(log).join("webpush")
}

pub fn load(log: &Path) -> Vec<Sub> {
    std::fs::read_to_string(dir(log).join("subs.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store(log: &Path, s: &[Sub]) -> Result<()> {
    save(&dir(log).join("subs.json"), &s)
}

fn b64(s: &str) -> Result<Vec<u8>> {
    Ok(B64.decode(s.trim_end_matches('='))?)
}

/// A fresh P-256 key from /dev/urandom.
fn key() -> Result<SecretKey> {
    loop {
        if let Ok(k) = SecretKey::from_slice(&random(32)?) {
            return Ok(k);
        }
    }
}

/// The uncompressed public point (65 bytes).
pub fn public(k: &SecretKey) -> Vec<u8> {
    k.public_key().to_sec1_point(false).as_bytes().to_vec()
}

/// The VAPID key, made on first use: `vapid.key`, 0600, the base64url scalar.
pub fn vapid(log: &Path) -> Result<SecretKey> {
    let p = dir(log).join("vapid.key");
    if p.exists() {
        return Ok(SecretKey::from_slice(&b64(std::fs::read_to_string(&p)?.trim())?)?);
    }
    let k = key()?;
    write0600(&p, B64.encode(k.to_bytes()).as_bytes())?;
    Ok(k)
}

/// RFC 8291 aes128gcm body for one subscription: a single record, `salt` and `eph`'s public key in its header.
pub fn encrypt(p256dh: &[u8], auth: &[u8], plain: &[u8], eph: &SecretKey, salt: &[u8; 16]) -> Result<Vec<u8>> {
    let ua = PublicKey::from_sec1_bytes(p256dh)?;
    let shared = p256::ecdh::diffie_hellman(eph.to_nonzero_scalar(), ua.as_affine());
    let as_pub = public(eph);
    let info = [b"WebPush: info\0".as_slice(), p256dh, &as_pub].concat();
    let hk = |salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]| {
        Hkdf::<Sha256>::new(Some(salt), ikm)
            .expand(info, out)
            .map_err(|_| anyhow!("hkdf"))
    };
    let (mut ikm, mut cek, mut nonce) = ([0u8; 32], [0u8; 16], [0u8; 12]);
    hk(auth, shared.raw_secret_bytes().as_slice(), &info, &mut ikm)?;
    hk(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek)?;
    hk(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce)?;
    let ct = Aes128Gcm::new(&cek.into())
        .encrypt(&nonce.into(), [plain, &[2]].concat().as_slice())
        .map_err(|_| anyhow!("aes-gcm"))?;
    Ok([salt.as_slice(), &4096u32.to_be_bytes(), &[65], &as_pub, &ct].concat())
}

/// The ES256 VAPID JWT for `endpoint`'s origin, valid 12 h.
pub fn jwt(k: &SecretKey, endpoint: &str, sub: &str, now: u64) -> String {
    let aud = endpoint.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    let claims = serde_json::json!({"aud": aud, "exp": now + 12 * 3600, "sub": sub}).to_string();
    let msg = format!(
        "{}.{}",
        B64.encode(r#"{"typ":"JWT","alg":"ES256"}"#),
        B64.encode(claims)
    );
    let sig: Signature = SigningKey::from(k).sign(msg.as_bytes());
    format!("{msg}.{}", B64.encode(sig.to_bytes()))
}

/// What a subscribe says with no webpush notifier: the device is kept, nothing is sent (#87).
pub const OFF: &str = "alerts are saved but off: add `[[notify]] type = \"webpush\"` to fleet.toml (docs/serve.md)";

/// `POST /push/subscribe` (endpoint, p256dh, auth): add or replace this endpoint's subscription, then (#87) push
/// "alerts are on" to it alone; with no webpush notifier (`sub`), say the alerts are saved but off.
pub fn subscribe(log: &Path, f: &BTreeMap<String, String>, label: &str, now: u64, sub: Option<&str>) -> Result<String> {
    let g = |k: &str| f.get(k).cloned().unwrap_or_default();
    let s = Sub {
        endpoint: g("endpoint"),
        p256dh: g("p256dh"),
        auth: g("auth"),
        label: label.into(),
        created: now,
    };
    if !s.endpoint.starts_with("https://") || s.endpoint.chars().any(char::is_whitespace) {
        bail!("endpoint must be an https:// URL");
    }
    if PublicKey::from_sec1_bytes(&b64(&s.p256dh)?).is_err() || b64(&s.auth)?.len() != 16 {
        bail!("bad p256dh or auth key");
    }
    let mut all = load(log);
    all.retain(|x| x.endpoint != s.endpoint);
    all.push(s);
    store(log, &all)?;
    let Some(sub) = sub else { return Ok(OFF.into()) };
    let ev = Event::new(
        now,
        "herdsman",
        None,
        "push-test",
        serde_json::json!({"reason": "alerts are on"}),
    );
    let sent = send(log, sub, &ev, Some(&g("endpoint")));
    Ok(sent.map_or_else(
        |e| format!("alerts on; the test push failed: {e:#}"),
        |_| "alerts on".into(),
    ))
}

/// Drop the subscriptions with these ids.
pub fn remove(log: &Path, ids: &[String]) -> Result<usize> {
    let mut all = load(log);
    let n = all.len();
    all.retain(|s| !ids.contains(&s.id()));
    store(log, &all)?;
    Ok(n - all.len())
}

/// POST one push with curl; the HTTP status it reports (0: no response).
fn post(s: &Sub, k: &SecretKey, sub: &str, body: &[u8]) -> Result<u16> {
    let auth = format!(
        "Authorization: vapid t={}, k={}",
        jwt(k, &s.endpoint, sub, crate::now()),
        B64.encode(public(k))
    );
    let mut c = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "10",
            "-X",
            "POST",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--data-binary",
            "@-",
            "-H",
            &auth,
        ])
        .args([
            "-H",
            "Content-Encoding: aes128gcm",
            "-H",
            "Content-Type: application/octet-stream",
            "-H",
            "TTL: 86400",
            &s.endpoint,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let _ = c.stdin.take().map(|mut i| i.write_all(body));
    Ok(String::from_utf8_lossy(&c.wait_with_output()?.stdout)
        .trim()
        .parse()
        .unwrap_or(0))
}

/// Push `ev` once to every subscription, or only to endpoint `only` (no retry): a 404/410 prunes it, any
/// other failure is the error. A delivery logs one passive `push-sent` with the device count (#87).
pub fn send(log: &Path, sub: &str, ev: &Event, only: Option<&str>) -> Result<String> {
    let all: Vec<Sub> = load(log)
        .into_iter()
        .filter(|s| only.is_none_or(|e| s.endpoint == e))
        .collect();
    if all.is_empty() {
        return Ok("no device has alerts on".into());
    }
    let k = vapid(log)?;
    let tab = if matches!(ev.kind.as_str(), "stall" | "lane-dead" | "orch-stuck") {
        "lanes"
    } else {
        "queue"
    };
    let url = ev.issue.map_or(String::new(), |n| format!("?issue={n}")) + "#" + tab;
    // #130: needs-human says who and why in the title; the body adds the last verdict's first finding.
    let n = ev.issue.filter(|_| ev.kind == "needs-human");
    let why = ev.str("reason").unwrap_or("?");
    let evs = n.map(|_| read(log)).transpose()?.unwrap_or_default();
    let first = n.and_then(|n| finding(&evs, &ev.fleet, n)).unwrap_or("");
    let (title, body) = match n {
        Some(n) => (format!("#{n} needs you: {why}"), format!("{why}\n{first}")),
        None => ("herdsman".into(), plain(ev)),
    };
    let payload = serde_json::json!({"title": title, "body": body.trim(), "url": format!("/{url}")}).to_string();
    let (mut sent, mut gone, mut errs) = (0, vec![], vec![]);
    for s in &all {
        let one = || -> Result<u16> {
            let salt: [u8; 16] = random(16)?.try_into().map_err(|_| anyhow!("salt"))?;
            post(
                s,
                &k,
                sub,
                &encrypt(&b64(&s.p256dh)?, &b64(&s.auth)?, payload.as_bytes(), &key()?, &salt)?,
            )
        };
        match one() {
            Ok(200..=299) => sent += 1,
            Ok(404 | 410) => gone.push(s.id()),
            Ok(code) => errs.push(format!("{}: HTTP {code}", s.label)),
            Err(e) => errs.push(format!("{}: {e:#}", s.label)),
        }
    }
    if !gone.is_empty() {
        remove(log, &gone)?;
    }
    if sent > 0 {
        let d = serde_json::json!({"devices": sent});
        crate::events::append(log, &Event::new(ev.ts, &ev.fleet, None, "push-sent", d))?;
    }
    if !errs.is_empty() {
        bail!("webpush: {}", errs.join("; "));
    }
    Ok(format!("sent to {sent} device(s), pruned {}", gone.len()))
}
