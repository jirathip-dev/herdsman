//! #122: state writes are atomic (a failed rewrite keeps the old file and logs `state-write-failed` once an
//! hour), `vapid.key` is never rewritten, and a device's last-seen is written at most hourly.
mod common;
use common::*;
use herdsman::devices;
use herdsman::events::read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn chmod(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_failed_write_keeps_the_old_devices_json_and_logs_once_an_hour() {
    let mut c = ctx(&tmpdir("state-write-fail"), CONFIG);
    devices::request(&c, "iPhone", "10.0.0.2").unwrap().unwrap();
    let file = c.state.parent().unwrap().join("serve/devices.json");
    let before = std::fs::read(&file).unwrap();
    chmod(file.parent().unwrap(), 0o500);
    assert!(devices::store(&c, &[]).is_err());
    c.now += 60;
    assert!(devices::store(&c, &[]).is_err());
    let after = std::fs::read(&file).unwrap();
    let dir: Vec<_> = std::fs::read_dir(file.parent().unwrap()).unwrap().collect();
    chmod(file.parent().unwrap(), 0o700);
    assert_eq!(after, before, "the previous devices.json survives byte-for-byte");
    assert_eq!(dir.len(), 1, "no temp file left");
    let failed: Vec<_> = read(&c.state)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "state-write-failed")
        .collect();
    assert_eq!(failed.len(), 1, "one event an hour");
    assert_eq!(failed[0].str("path"), Some(file.to_str().unwrap()));
    assert_eq!(failed[0].str("error"), Some("permission denied"), "the error kind only");
    let hash = &devices::load(&c)[0].hash;
    assert!(
        !std::fs::read_to_string(&c.state).unwrap().contains(hash.as_str()),
        "no token hash in the log"
    );
    assert!(
        herdsman::events::plain(&failed[0]).starts_with(&format!("herdsman state-write-failed: {}: ", file.display()))
    );
    // A write that succeeds replaces the file, still 0600.
    devices::store(&c, &[]).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[]");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn an_existing_vapid_key_is_never_rewritten() {
    let c = ctx(&tmpdir("state-write-vapid"), CONFIG);
    let k = herdsman::webpush::vapid(&c.state).unwrap();
    let file = c.state.parent().unwrap().join("webpush/vapid.key");
    let before = std::fs::read(&file).unwrap();
    chmod(file.parent().unwrap(), 0o500); // any rewrite (temp + rename) would now fail
    let again = herdsman::webpush::vapid(&c.state);
    chmod(file.parent().unwrap(), 0o700);
    assert_eq!(again.unwrap().to_bytes(), k.to_bytes());
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn last_seen_is_written_at_most_hourly() {
    let mut c = ctx(&tmpdir("state-write-seen"), CONFIG);
    let (code, token) = devices::request(&c, "iPhone", "10.0.0.2").unwrap().unwrap();
    devices::approve(&c, &code).unwrap();
    let cookie = format!("hd={token}");
    let used = |c: &herdsman::Ctx| devices::load(c)[0].used;
    let t0 = c.now;
    c.now = t0 + 3599;
    assert!(devices::find(&c, &cookie).is_some());
    assert_eq!(used(&c), t0, "not rewritten within the hour");
    c.now = t0 + 3600;
    assert!(devices::find(&c, &cookie).is_some());
    assert_eq!(used(&c), t0 + 3600);
    // A failed last-seen write still lets the paired device in.
    let dir = c.state.parent().unwrap().join("serve");
    chmod(&dir, 0o500);
    c.now = t0 + 7200;
    let found = devices::find(&c, &cookie);
    chmod(&dir, 0o700);
    assert!(found.is_some());
    assert_eq!(used(&c), t0 + 3600);
}
