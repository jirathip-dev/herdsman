//! #631 `herdsman-keys get|set|delete <herdsman/…>`: the one program that touches herdsman's Keychain items, so a new
//! herdsman build needs no new approval. `set` reads the value on stdin; `get` prints it with no newline. Run by
//! herdsman (stdin not a terminal) it never shows a dialog; from a terminal it may, for the owner's one Always Allow.
//! Exit 0 ok, 1 no such item, 2 usage or store error, 3 the item wants a Keychain dialog.
use std::io::{IsTerminal, Read};
use std::process::ExitCode;

#[cfg(target_os = "macos")]
#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecKeychainSetUserInteractionAllowed(state: u8) -> i32;
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let [cmd, svc] = &a[..] else {
        return fail(2, "usage: herdsman-keys get|set|delete herdsman/<fleet>/<NAME>");
    };
    if !svc.starts_with("herdsman/") {
        return fail(2, "herdsman-keys: only herdsman/* items");
    }
    #[cfg(target_os = "macos")]
    if !std::io::stdin().is_terminal() {
        // SAFETY: sets one per-process flag; keychain calls then fail instead of showing a dialog.
        unsafe { SecKeychainSetUserInteractionAllowed(0) };
    }
    let e = match keyring::Entry::new(svc, "herdsman") {
        Ok(e) => e,
        Err(e) => return fail(2, &format!("herdsman-keys: {svc}: {e}")),
    };
    let done = match cmd.as_str() {
        "get" => e.get_password().map(|v| print!("{v}")),
        "delete" => e.delete_credential(),
        "set" => {
            let mut v = String::new();
            if std::io::stdin().is_terminal() || std::io::stdin().read_to_string(&mut v).is_err() {
                return fail(2, "herdsman-keys set: the value goes on stdin");
            }
            e.set_password(&v)
        }
        _ => return fail(2, "usage: herdsman-keys get|set|delete herdsman/<fleet>/<NAME>"),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(keyring::Error::NoEntry) => ExitCode::from(1),
        // errSecInteractionNotAllowed, errSecAuthFailed: the item would ask for approval.
        Err(e) if ["-25308", "-25293"].iter().any(|c| format!("{e:?}").contains(c)) => {
            fail(3, &format!("herdsman-keys: {svc} wants a Keychain dialog: {e}"))
        }
        Err(e) => fail(2, &format!("herdsman-keys: {svc}: {e}")),
    }
}

fn fail(code: u8, why: &str) -> ExitCode {
    eprintln!("{why}");
    ExitCode::from(code)
}
