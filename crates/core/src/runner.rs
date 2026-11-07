//! The one seam for external tools (herdr, gh, git). Tests swap in a fake.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait Runner: Sync {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out>;
    /// `run` with `input` on stdin (secrets go here, never in argv); fakes ignore it.
    fn run_in(&self, prog: &str, args: &[String], _input: &str) -> Result<Out> {
        self.run(prog, args)
    }
    /// #561: `run_in`, its child killed (exit 124) once `limit` has passed; fakes answer at once.
    fn run_for(&self, prog: &str, args: &[String], input: &str, _limit: Duration) -> Result<Out> {
        self.run_in(prog, args, input)
    }
    /// Pause between polls/retries; fakes skip it.
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
    /// #340: start `exe args` detached, stdin null, both streams appended to `log`; its pid. `Real` gives it its own
    /// session, out of reach of launchd's group kill when the tick/fast job exits; fakes see this `sh -c nohup` line.
    fn detach(&self, exe: &str, args: &[String], log: &std::path::Path) -> Result<u32> {
        let (args, log) = (args.join(" "), log.display());
        let sh = format!("nohup '{exe}' {args} >> '{log}' 2>&1 < /dev/null & echo $!");
        Ok(self.run("sh", &argv(&["-c", &sh]))?.stdout.trim().parse()?)
    }
    /// #298: the shed runtime this runner's lanes use (`[runtime] kind = "shed"`); None: herdr only.
    fn shed(&self) -> Option<crate::shed::Shed> {
        None
    }
    /// #529: whether `herdr` runs on this host's own (the service's streamed agent list stands for its `agent list`);
    /// fakes and other hosts: no.
    fn local(&self) -> bool {
        false
    }
    /// #441: keychain service `svc`'s secret in the main host's OS store (None: not set); fakes and wrappers forward.
    fn secret(&self, svc: &str) -> Result<Option<String>> {
        bail!("no secret store on this host (wanted {svc})")
    }
    /// #441: save `svc`'s secret (None: delete it) in the main host's OS store.
    fn set_secret(&self, svc: &str, _value: Option<&str>) -> Result<()> {
        bail!("no secret store on this host (wanted {svc})")
    }
}

pub struct Real;

/// #441: the OS store's entry for `svc`, under the `herdsman` account `security` used (#387), so old items keep working.
fn entry(svc: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(svc, "herdsman").with_context(|| format!("secret store: {svc}"))
}

impl Runner for Real {
    fn local(&self) -> bool {
        true
    }
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        self.run_in(prog, args, "")
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        self.within(prog, args, input, limit(prog, args))
    }
    fn run_for(&self, prog: &str, args: &[String], input: &str, limit: Duration) -> Result<Out> {
        self.within(prog, args, input, Some(limit))
    }
    fn detach(&self, exe: &str, args: &[String], log: &std::path::Path) -> Result<u32> {
        use std::os::unix::process::CommandExt;
        extern "C" {
            fn setsid() -> i32;
        }
        let log = std::fs::File::options().create(true).append(true).open(log)?;
        let mut c = Command::new(exe);
        c.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        // SAFETY: setsid is async-signal-safe; between fork and exec it touches nothing else.
        unsafe {
            c.pre_exec(|| match setsid() {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
        let mut child = c.spawn().with_context(|| format!("cannot run {exe}"))?;
        let pid = child.id();
        std::thread::spawn(move || child.wait()); // a long-lived parent (serve) reaps it
        Ok(pid)
    }
    fn secret(&self, svc: &str) -> Result<Option<String>> {
        store_get(self, svc)
    }
    fn set_secret(&self, svc: &str, value: Option<&str>) -> Result<()> {
        store_set(self, svc, value)
    }
}

/// #631: the `herdsman-keys` helper the store goes through: `$HERDSMAN_KEYS` (the install gates), else beside this
/// binary. Its cdhash outlasts herdsman builds, so the Keychain's one approval does too.
pub fn keys_helper() -> std::path::PathBuf {
    std::env::var_os("HERDSMAN_KEYS").map_or_else(
        || {
            let exe = std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .unwrap_or_default();
            exe.with_file_name("herdsman-keys")
        },
        Into::into,
    )
}

/// #631: `herdsman-keys <cmd> <svc>` (`input` on stdin) within `KEYCHAIN_SECS`; exit 1 is None, 3 or a timeout is
/// `KEYCHAIN_LOCKED`.
fn keys(r: &dyn Runner, cmd: &str, svc: &str, input: &str) -> Result<Option<String>> {
    let k = keys_helper();
    let o = r.run_for(
        &k.to_string_lossy(),
        &argv(&[cmd, svc]),
        input,
        Duration::from_secs(KEYCHAIN_SECS),
    )?;
    match o.code {
        0 => Ok(Some(o.stdout)),
        1 => Ok(None),
        3 | 124 => locked(r, svc),
        _ => bail!("secret store: {svc}: {}", o.stderr.trim()),
    }
}

/// #441: `svc`'s secret in the OS store (None: not set); #548: on `r`'s clock, `KEYCHAIN_LOCKED` past `KEYCHAIN_SECS`.
/// #631: through `herdsman-keys` when it is installed.
pub fn store_get(r: &dyn Runner, svc: &str) -> Result<Option<String>> {
    if keys_helper().is_file() {
        return keys(r, "get", svc, "");
    }
    let e = entry(svc)?;
    match keychain(r, svc, move || e.get_password())? {
        Err(keyring::Error::NoEntry) => Ok(None),
        x => Ok(Some(x.with_context(|| format!("secret store: {svc}"))?)),
    }
}

/// #441: save `svc`'s secret (None: delete it) in the OS store; #548: as `store_get`, never blocking.
pub fn store_set(r: &dyn Runner, svc: &str, value: Option<&str>) -> Result<()> {
    if keys_helper().is_file() {
        let (cmd, v) = value.map_or(("delete", ""), |v| ("set", v));
        return keys(r, cmd, svc, v).map(drop);
    }
    let (e, v) = (entry(svc)?, value.map(String::from));
    let done = keychain(r, svc, move || {
        v.map_or_else(|| e.delete_credential(), |v| e.set_password(&v))
    })?;
    done.with_context(|| format!("secret store: {svc}"))
}

/// #548: how long a keychain call may wait (a macOS access dialog nobody clicks blocks it for good).
pub const KEYCHAIN_SECS: u64 = 5;

/// #548: `f` (a keychain call on `svc`) on a worker thread, given `KEYCHAIN_SECS` (`r`'s clock) to answer; past that
/// `KEYCHAIN_LOCKED` with the fix, and the worker is left waiting on the dialog.
fn keychain<T: Send + 'static>(r: &dyn Runner, svc: &str, f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    let mut got = rx.recv_timeout(Duration::from_millis(200)).ok();
    for _ in 0..KEYCHAIN_SECS {
        if got.is_some() {
            break;
        }
        r.sleep(1);
        got = rx.try_recv().ok();
    }
    got.map_or_else(|| locked(r, svc), Ok)
}

/// #631: the one-time fix for `svc`: authorise `herdsman-keys`, whose cdhash lasts across herdsman builds.
pub fn keychain_fix(svc: &str) -> String {
    let k = keys_helper();
    if !k.is_file() {
        return format!(
            "install {} (herdsman self-update or scripts/install-gate.sh; docs/troubleshooting.md §3)",
            k.display()
        );
    }
    let k = k.display();
    format!(
        "authorise {k} once: codesign -dvvv {k} 2>&1 | grep CDHash; security set-generic-password-partition-list \
         -S apple-tool:,apple:,unsigned:,cdhash:<CDHash> -s {svc} ~/Library/Keychains/login.keychain-db (asks the \
         keychain password), or {k} get {svc} >/dev/null at the Mac and Always Allow (docs/troubleshooting.md §3)"
    )
}

fn locked<T>(r: &dyn Runner, svc: &str) -> Result<T> {
    let host = ok(r, "hostname", &["-s"]).map_or_else(|_| "this host".into(), |h| h.trim().to_string());
    crate::gate::refused(
        "KEYCHAIN_LOCKED",
        format!("the Keychain wants approval for {svc} on {host}: {}", keychain_fix(svc)),
    )
}

impl Real {
    /// `run_in`, killed (exit 124) once `limit` has passed.
    pub fn within(&self, prog: &str, args: &[String], input: &str, limit: Option<Duration>) -> Result<Out> {
        use std::io::Write;
        use std::process::Stdio;
        let mut c = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot run {prog}"))?;
        let _ = c.stdin.take().unwrap().write_all(input.as_bytes());
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let Some(limit) = limit else {
            let o = c.wait_with_output()?;
            let code = o.status.code().unwrap_or(-1);
            return Ok(Out {
                code,
                stdout: text(&o.stdout),
                stderr: text(&o.stderr),
            });
        };
        let (so, se) = (drain(c.stdout.take()), drain(c.stderr.take()));
        let end = Instant::now() + limit;
        while Instant::now() < end {
            if let Some(st) = c.try_wait()? {
                let (stdout, stderr) = (
                    text(&so.join().unwrap_or_default()),
                    text(&se.join().unwrap_or_default()),
                );
                return Ok(Out {
                    code: st.code().unwrap_or(-1),
                    stdout,
                    stderr,
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = (c.kill(), c.wait()); // its readers may outlive it while a grandchild (ssh) holds the pipes
        let stderr = format!("{prog} {}: no answer in {:?}", args.join(" "), limit);
        Ok(Out {
            code: 124,
            stderr,
            ..Out::default()
        })
    }
}

/// #478: another host's `herdr --machine m … list` (its ssh has no timeout of its own) answers in 20 s.
pub fn limit(prog: &str, args: &[String]) -> Option<Duration> {
    let remote = prog == crate::herdr::BIN && args.first().is_some_and(|a| a == "--machine");
    (remote && args.get(3).is_some_and(|a| a == "list")).then(|| Duration::from_secs(20))
}

fn drain(p: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut v = vec![];
        let _ = p.map(|mut p| p.read_to_end(&mut v));
        v
    })
}

pub fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(std::string::ToString::to_string).collect()
}

/// Run and fail on a non-zero exit; returns stdout.
pub fn ok<S: AsRef<str>>(r: &dyn Runner, prog: &str, args: &[S]) -> Result<String> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let o = r.run(prog, &argv(&args))?;
    if o.code != 0 {
        bail!("{prog} {} failed ({}): {}", args.join(" "), o.code, o.stderr.trim());
    }
    Ok(o.stdout)
}

pub fn json<S: AsRef<str>>(r: &dyn Runner, prog: &str, args: &[S]) -> Result<Value> {
    let s = ok(r, prog, args)?;
    serde_json::from_str(&s).with_context(|| {
        format!(
            "{prog} {}: not JSON",
            args.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(" ")
        )
    })
}
