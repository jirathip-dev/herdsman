//! #18: web push: RFC 8291 aes128gcm (its Appendix A vector, and a decrypt round trip), the ES256
//! VAPID JWT, subscribe/unsubscribe on the page (Host/Origin + paired cookie), and delivery by a
//! fake `curl`: 201 sent, 410 pruned, 500 `notify-failed`.
mod common;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes128Gcm;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use common::*;
use herdsman::config::Config;
use herdsman::events::{emit, read, Event};
use herdsman::serve::Server;
use herdsman::webpush;
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use p256::{PublicKey, SecretKey};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const ADDR: &str = "127.0.0.1:7777";
const PLAIN: &str = "When I grow up, I want to be a watermelon";
const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const UA_PUBLIC: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

fn d(s: &str) -> Vec<u8> {
    B64.decode(s).unwrap()
}

/// The user agent's side of RFC 8291: the plaintext, or None when the body does not decrypt.
fn decrypt(body: &[u8], ua: &SecretKey, auth: &[u8]) -> Option<Vec<u8>> {
    let (salt, idlen) = (&body[..16], body[20] as usize);
    let as_pub = PublicKey::from_sec1_bytes(&body[21..21 + idlen]).ok()?;
    let shared = p256::ecdh::diffie_hellman(ua.to_nonzero_scalar(), as_pub.as_affine());
    let ua_pub = webpush::public(ua);
    let info = [b"WebPush: info\0".as_slice(), &ua_pub, &body[21..21 + idlen]].concat();
    let hk = |salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]| hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), ikm).expand(info, out).unwrap();
    let (mut ikm, mut cek, mut nonce) = ([0u8; 32], [0u8; 16], [0u8; 12]);
    hk(auth, shared.raw_secret_bytes().as_slice(), &info, &mut ikm);
    hk(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek);
    hk(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce);
    let mut pt = Aes128Gcm::new(&cek.into()).decrypt(&nonce.into(), &body[21 + idlen..]).ok()?;
    (pt.pop() == Some(2)).then_some(pt)
}

#[test]
fn rfc8291_vector_encrypts_and_decrypts_back() {
    let (eph, ua) = (SecretKey::from_slice(&d(AS_PRIVATE)).unwrap(), SecretKey::from_slice(&d(UA_PRIVATE)).unwrap());
    assert_eq!(webpush::public(&ua), d(UA_PUBLIC));
    let salt: [u8; 16] = d(SALT).try_into().unwrap();
    let body = webpush::encrypt(&d(UA_PUBLIC), &d(AUTH), PLAIN.as_bytes(), &eph, &salt).unwrap();
    assert_eq!(B64.encode(&body), BODY, "RFC 8291 Appendix A");
    assert_eq!(decrypt(&body, &ua, &d(AUTH)).unwrap(), PLAIN.as_bytes());
    assert_eq!(decrypt(&body, &ua, &[0; 16]), None, "a wrong auth secret does not decrypt");
}

#[test]
fn vapid_jwt_verifies_with_the_public_key() {
    let dir = tmpdir("wp-jwt");
    let log = dir.join("state/events.jsonl");
    let k = webpush::vapid(&log).unwrap();
    let file = dir.join("state/webpush/vapid.key");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(webpush::vapid(&log).unwrap().to_bytes(), k.to_bytes(), "made once, then reused");
    let jwt = webpush::jwt(&k, "https://web.push.apple.com/QGuQyavXutnMH", "mailto:owner@example.com", 1_000);
    let (msg, sig) = jwt.rsplit_once('.').unwrap();
    let vk = VerifyingKey::from_sec1_bytes(&webpush::public(&k)).unwrap();
    vk.verify(msg.as_bytes(), &Signature::from_slice(&d(sig)).unwrap()).expect("ES256 signature verifies");
    let (h, c) = msg.split_once('.').unwrap();
    let head: serde_json::Value = serde_json::from_slice(&d(h)).unwrap();
    let claims: serde_json::Value = serde_json::from_slice(&d(c)).unwrap();
    assert_eq!(head["alg"], "ES256");
    assert_eq!(claims, serde_json::json!({"aud": "https://web.push.apple.com", "exp": 1_000 + 12 * 3600, "sub": "mailto:owner@example.com"}));
}

fn sub_form(ua: &SecretKey, endpoint: &str) -> String {
    format!("endpoint={endpoint}&p256dh={}&auth={AUTH}", B64.encode(webpush::public(ua)))
}

fn post(s: &Server, path: &str, body: &str, extra: &str) -> String {
    let req = format!("POST {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
    String::from_utf8_lossy(&s.respond(&req, 1_000_000)).into_owned()
}

#[test]
fn subscribe_needs_a_paired_device_and_unsubscribe_removes_it() {
    let dir = tmpdir("wp-sub");
    let c = ctx(&dir, CONFIG);
    let r = Fake::default().reply("herdr", NO_AGENTS);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ua = SecretKey::from_slice(&d(UA_PRIVATE)).unwrap();
    let form = sub_form(&ua, "https://web.push.apple.com/secret-endpoint");
    assert!(post(&s, "/push/subscribe", &form, "").starts_with("HTTP/1.1 401"), "unpaired");
    assert!(post(&s, "/push/subscribe", &form, "Origin: https://evil.example\r\n").starts_with("HTTP/1.1 403"), "foreign Origin");
    let code = herdsman::devices::new_code(&c).unwrap();
    let res = post(&s, "/pair", &format!("code={code}"), "User-Agent: Mozilla (iPhone)\r\n");
    let set = res.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).expect(&res);
    let cookie = format!("Cookie: {}\r\nUser-Agent: Mozilla (iPhone)\r\n", &set[..set.find(';').unwrap()]);
    assert!(post(&s, "/push/subscribe", "endpoint=http://x&p256dh=AA&auth=AA", &cookie).starts_with("HTTP/1.1 400"));
    assert!(post(&s, "/push/subscribe", &form, &cookie).starts_with("HTTP/1.1 200"));
    let subs = webpush::load(&c.state);
    assert_eq!((subs.len(), subs[0].label.as_str()), (1, "iPhone"));
    let file = dir.join("state/webpush/subs.json");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let page = String::from_utf8_lossy(&s.respond(&format!("GET / HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), 1_000_000)).into_owned();
    assert!(page.contains("Enable alerts") && page.contains(&subs[0].id()), "Host tab lists the device");
    assert!(!page.contains("secret-endpoint") && !page.contains(AUTH), "endpoint and keys stay off the page");
    let body = format!("id={}", subs[0].id());
    assert!(post(&s, "/push/unsubscribe", &body, "").starts_with("HTTP/1.1 401"), "unpaired cannot remove");
    assert!(post(&s, "/push/unsubscribe", &body, &cookie).starts_with("HTTP/1.1 303"));
    assert!(webpush::load(&c.state).is_empty());
}

/// A fake `curl` on PATH: saves its stdin, answers the status in `<dir>/code`.
fn fake_curl(dir: &Path) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!("#!/bin/sh\ncat > '{0}/body'\necho \"$@\" > '{0}/args'\ncat '{0}/code'\n", dir.display());
    std::fs::write(bin.join("curl"), script).unwrap();
    std::fs::set_permissions(bin.join("curl"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
}

#[test]
fn delivery_201_sent_410_pruned_500_notify_failed() {
    let dir = tmpdir("wp-curl");
    fake_curl(&dir);
    let c = ctx(&dir, CONFIG);
    let cfg = Config::parse("[[notify]]\ntype = \"webpush\"\nsub = \"mailto:owner@example.com\"\n").unwrap();
    let ua = SecretKey::from_slice(&d(UA_PRIVATE)).unwrap();
    let f: std::collections::BTreeMap<String, String> =
        sub_form(&ua, "https://push.example/ep").split('&').map(|kv| kv.split_once('=').unwrap()).map(|(k, v)| (k.into(), v.into())).collect();
    webpush::subscribe(&c.state, &f, "Mac", 1).unwrap();
    let merged = || Event::new(1, "sendmeter", Some(7), "merged", serde_json::json!({"into": "staging", "merge_sha": "abc", "pr": 9}));
    let failed = || read(&c.state).unwrap().iter().filter(|e| e.kind == "notify-failed").count();

    std::fs::write(dir.join("code"), "201").unwrap();
    emit(&c.state, &cfg, merged()).unwrap();
    assert_eq!((failed(), webpush::load(&c.state).len()), (0, 1), "201: sent, kept");
    let body = std::fs::read(dir.join("body")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&decrypt(&body, &ua, &d(AUTH)).expect("decrypts")).unwrap();
    assert_eq!(json["url"], "/?issue=7#queue");
    assert!(json["body"].as_str().unwrap().contains("sendmeter #7 merged"));
    let args = std::fs::read_to_string(dir.join("args")).unwrap();
    assert!(args.contains("--max-time 10") && args.contains("Authorization: vapid t=") && args.ends_with("https://push.example/ep\n"), "{args}");

    emit(&c.state, &cfg, Event::new(1, "sendmeter", Some(7), "spawn", serde_json::json!({}))).unwrap();
    assert_eq!(std::fs::read(dir.join("body")).unwrap(), body, "spawn is not in the default `on`");

    std::fs::write(dir.join("code"), "500").unwrap();
    emit(&c.state, &cfg, merged()).unwrap();
    assert_eq!((failed(), webpush::load(&c.state).len()), (1, 1), "500: notify-failed, kept, no retry");
    let ev = read(&c.state).unwrap().into_iter().rfind(|e| e.kind == "notify-failed").unwrap();
    assert_eq!(ev.str("cmd"), Some("webpush"));
    assert!(!serde_json::to_string(&ev).unwrap().contains("push.example"), "endpoint stays out of the log");

    std::fs::write(dir.join("code"), "410").unwrap();
    emit(&c.state, &cfg, merged()).unwrap();
    assert_eq!((failed(), webpush::load(&c.state).len()), (1, 0), "410: pruned, not a failure");
}

#[test]
fn webpush_notifier_needs_sub() {
    assert!(Config::parse("[[notify]]\ntype = \"webpush\"\n").is_err());
}
