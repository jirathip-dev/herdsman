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
    fn run_in(&self, prog: &str, args: &[String], _input: &str) -> Result<Out> {
        self.run(prog, args)
    }
    fn run_for(&self, prog: &str, args: &[String], input: &str, _limit: Duration) -> Result<Out> {
        self.run_in(prog, args, input)
    }
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
    fn detach(&self, exe: &str, args: &[String], log: &std::path::Path) -> Result<u32> {
        let (args, log) = (args.join(" "), log.display());
        let sh = format!("nohup '{exe}' {args} >> '{log}' 2>&1 < /dev/null & echo $!");
        Ok(self.run("sh", &argv(&["-c", &sh]))?.stdout.trim().parse()?)
    }
    fn attached(&self, prog: &str, args: &[String]) -> Result<i32> {
        use std::os::unix::process::ExitStatusExt;
        let st = Command::new(prog).args(args).status()?;
        Ok(st.code().or(st.signal().map(|s| 128 + s)).unwrap_or(1))
    }
    fn shed(&self) -> Option<crate::shed::Shed> {
        None
    }
    fn stream(&self, prog: &str, args: &[String], input: &str) -> Result<Box<dyn std::io::Read + Send>> {
        Real::stream(prog, args, input)
    }
    fn secret(&self, svc: &str) -> Result<Option<String>> {
        bail!("drover keeps no secret store (wanted {svc})")
    }
    fn set_secret(&self, svc: &str, _value: Option<&str>) -> Result<()> {
        bail!("drover keeps no secret store (wanted {svc})")
    }
}

thread_local!(static REQUEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// #859: `f` as a board GET request: it may read only what the background refreshed, never start a process.
pub fn request<T>(f: impl FnOnce() -> T) -> T {
    let was = REQUEST.replace(true);
    let t = f();
    REQUEST.set(was);
    t
}

/// #859: this thread is serving a board GET request (`request`).
pub fn in_request() -> bool {
    REQUEST.get()
}

const SERVE_FDS: &str = "DROVER_SERVE_FDS";
extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
}

/// #894: `argv` with the listeners `fds` kept open across exec and named in `DROVER_SERVE_FDS`: a bridge in the background (sh reaps nothing), else exec'd in place.
pub fn hand_fds(argv: &[std::ffi::OsString], fds: &[i32], bridge: bool) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new(if bridge { "sh".as_ref() } else { argv[0].as_os_str() });
    if bridge {
        c.args(["-c", "\"$@\" &", "sh"]).args(argv);
    } else {
        c.args(&argv[1..]);
    }
    let list: Vec<String> = fds.iter().map(i32::to_string).collect();
    c.env(SERVE_FDS, list.join(",")).stdin(std::process::Stdio::null());
    let fds = fds.to_vec();
    // SAFETY: fcntl is async-signal-safe; between fork and exec it only clears close-on-exec on the listeners.
    unsafe {
        c.pre_exec(move || {
            if fds.iter().all(|&fd| fcntl(fd, 2, 0) != -1) {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    if !bridge {
        return Err(c.exec()).context("exec");
    }
    let s = c.status().context("sh")?;
    if !s.success() {
        bail!("sh: {s}");
    }
    Ok(())
}

/// #894: the listeners a predecessor handed over (`hand_fds`), close-on-exec again; none for a fresh start.
pub fn inherited() -> Vec<std::net::TcpListener> {
    use std::os::fd::FromRawFd;
    let Some(v) = std::env::var_os(SERVE_FDS) else {
        return vec![];
    };
    std::env::remove_var(SERVE_FDS);
    let fds = v
        .to_string_lossy()
        .split(',')
        .filter_map(|f| f.parse().ok())
        .collect::<Vec<i32>>();
    // SAFETY: each fd is a listener the predecessor passed down for this process alone; nothing else here owns it.
    unsafe {
        fds.into_iter()
            .map(|fd| {
                fcntl(fd, 2, 1);
                std::net::TcpListener::from_raw_fd(fd)
            })
            .collect()
    }
}

pub struct Real;

impl Runner for Real {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        self.run_in(prog, args, "")
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        self.within(prog, args, input.as_bytes(), limit(prog, args))
    }
    fn run_for(&self, prog: &str, args: &[String], input: &str, limit: Duration) -> Result<Out> {
        self.within(prog, args, input.as_bytes(), limit)
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
        std::thread::spawn(move || child.wait());
        Ok(pid)
    }
}

struct Streamed(std::process::Child, std::process::ChildStdout);

impl std::io::Read for Streamed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.1.read(buf)
    }
}

impl Drop for Streamed {
    fn drop(&mut self) {
        let _ = self.0.wait();
    }
}

impl Real {
    fn stream(prog: &str, args: &[String], input: &str) -> Result<Box<dyn std::io::Read + Send>> {
        use std::io::Write;
        use std::process::Stdio;
        let mut c = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot run {prog}"))?;
        c.stdin.take().context("stdin")?.write_all(input.as_bytes())?;
        let out = c.stdout.take().context("stdout")?;
        Ok(Box::new(Streamed(c, out)))
    }
    /// `run_in` with bytes on stdin, killed (exit 124) once `limit` has passed.
    pub fn within(&self, prog: &str, args: &[String], input: &[u8], limit: Duration) -> Result<Out> {
        use std::io::Write;
        use std::process::Stdio;
        debug_assert!(!in_request(), "#859: `{prog}` started on a board request");
        let dir = args.iter().position(|a| a == "-C").and_then(|i| args.get(i + 1));
        let fail = |why: String| {
            let stderr = format!("{prog} {}: {why}", args.join(" "));
            Ok(Out {
                code: 124,
                stderr,
                ..Out::default()
            })
        };
        if let Some(dir) = dir.filter(|d| hung(d, probe_wait(), list)) {
            return fail(format!("{dir} is on a disk that is not answering"));
        }
        let mut pipes = None;
        let end = Instant::now() + limit;
        let start = || {
            use std::os::unix::process::CommandExt;
            let mut c = Command::new(prog)
                .args(args)
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("cannot run {prog}"))?;
            let _ = c.stdin.take().unwrap().write_all(input);
            pipes = Some((drain(c.stdout.take()), drain(c.stderr.take())));
            Ok(c)
        };
        let ended = settle(dir.map(String::as_str), limit, start)?;
        let text = |b: std::sync::mpsc::Receiver<Vec<u8>>| {
            let left = end.saturating_duration_since(Instant::now());
            let v = b.recv_timeout(left).ok()?;
            Some(String::from_utf8_lossy(&v).into_owned())
        };
        match ended {
            Ended::Exited(st) => {
                let (so, se) = pipes.context("pipes")?;
                match (text(so), text(se)) {
                    (Some(stdout), Some(stderr)) => Ok(Out {
                        code: st.code().unwrap_or(-1),
                        stdout,
                        stderr,
                    }),
                    _ => fail(format!("it ended, but its output stayed open past {limit:?}")),
                }
            }
            Ended::TimedOut => {
                dir.inspect(|d| sicken(d));
                fail(format!("no answer in {limit:?}"))
            }
            Ended::Busy => fail(format!("an earlier call on {} has not ended", dir.map_or("", |d| d))),
        }
    }
}

/// #713: every call's time limit by the program's file name, unless `run_for` gives one: a call that hits it is killed and exits 124. - `security`, `codesign`: 10 s; they wait on the login Keychain, which can stop answering (11-08). - `git clone|fetch|pull|push`: 10 min; a large repo over the network. - `cp`, `rm`: 10 min; copying (spawn's build cache) or deleting a worktree's build folder of many GB. - `cargo`: 30 min; self-update's release build. - `ssh` through `hosts::On`: the limit of the command it carries; `hosts bootstrap`'s installs 30 min by `run_for`. - anything else: 60 s.
pub fn limit(prog: &str, args: &[String]) -> Duration {
    let net = |w: &String| ["clone", "fetch", "pull", "push"].contains(&w.as_str());
    let name = std::path::Path::new(prog)
        .file_name()
        .map_or(prog.into(), |n| n.to_string_lossy());
    let secs = match &*name {
        "security" | "codesign" => 10,
        "git" if args.iter().any(net) => 600,
        "cp" | "rm" => 600,
        "cargo" => 1800,
        _ => 60,
    };
    Duration::from_secs(secs)
}

/// #776: how long a folder's listing may take before its disk counts as not answering (`disk_probe_secs`).
pub fn probe_wait() -> Duration {
    Duration::from_secs(crate::config::limits().disk_probe_secs)
}

/// #972: `dir` listed within `disk_probe_secs` (`hung`'s probe); a dead disk's root is skipped for the pass.
pub fn answers(dir: &std::path::Path) -> bool {
    !hung(&dir.to_string_lossy(), probe_wait(), list)
}

fn sick_for() -> Duration {
    Duration::from_secs(crate::config::limits().disk_sick_secs)
}

fn poll() -> Duration {
    Duration::from_millis(crate::config::limits().poll_millis)
}

fn list(dir: &str) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::metadata(dir).ok()?.dev();
    drop(std::fs::read_dir(dir).map(|mut e| e.next()));
    Some(dev)
}

struct Probe {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dev: std::sync::Arc<std::sync::atomic::AtomicU64>,
    at: Instant,
    end: Instant,
}

fn probes() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, Probe>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Probe>>> =
        std::sync::OnceLock::new();
    let m = SEEN.get_or_init(std::sync::Mutex::default);
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn sick() -> std::sync::MutexGuard<'static, std::collections::HashMap<u64, Instant>> {
    static SICK_AT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, Instant>>> =
        std::sync::OnceLock::new();
    let m = SICK_AT.get_or_init(std::sync::Mutex::default);
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn mark(dev: u64) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::Path::new(&home)
            .join(".cache/drover/sick-disks")
            .join(dev.to_string()),
    )
}

fn is_sick(dev: u64) -> bool {
    let fresh = |t: std::time::SystemTime| t.elapsed().is_ok_and(|e| e < sick_for());
    dev != 0
        && (sick().get(&dev).is_some_and(|t| t.elapsed() < sick_for())
            || mark(dev)
                .and_then(|p| p.metadata().ok()?.modified().ok())
                .is_some_and(fresh))
}

/// #776: mark the disk `dir`'s last probe named as hung for `disk_sick_secs` (a call on it ran out of time).
pub fn sicken(dir: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    let dev = probes().get(dir).map_or(0, |p| p.dev.load(Relaxed));
    if dev != 0 {
        sick().insert(dev, Instant::now());
        if let Some(p) = mark(dev) {
            let _ = p.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(p, b"");
        }
    }
}

/// #776: whether `dir`'s disk has stopped answering: `probe(dir)` (its disk and a listing) runs on its own thread and must finish within `wait`. A probe still running is never repeated: the folder stays hung, with no new thread, until it ends. A folder whose disk `sicken` marked is hung; one that answered is otherwise trusted for a minute.
pub fn hung(dir: &str, wait: Duration, probe: impl FnOnce(&str) -> Option<u64> + Send + 'static) -> bool {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    use std::sync::Arc;
    let (done, dev, end) = {
        let mut m = probes();
        if let Some(p) = m.get(dir) {
            if !p.done.load(Relaxed) && Instant::now() < p.end {
                let (done, dev, end) = (p.done.clone(), p.dev.clone(), p.end);
                drop(m);
                return hangs_past(&done, &dev, end);
            }
            if !p.done.load(Relaxed) || is_sick(p.dev.load(Relaxed)) {
                return true;
            }
            if p.at.elapsed() < Duration::from_secs(crate::config::limits().disk_trust_secs) {
                return false;
            }
        }
        let (done, dev) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)));
        let end = Instant::now() + wait;
        m.insert(
            dir.into(),
            Probe {
                done: done.clone(),
                dev: dev.clone(),
                at: Instant::now(),
                end,
            },
        );
        (done, dev, end)
    };
    let (d, v, owned) = (done.clone(), dev.clone(), dir.to_string());
    std::thread::spawn(move || {
        v.store(probe(&owned).unwrap_or(0), Relaxed);
        d.store(true, Relaxed);
    });
    hangs_past(&done, &dev, end)
}

fn hangs_past(done: &std::sync::atomic::AtomicBool, dev: &std::sync::atomic::AtomicU64, end: Instant) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    while Instant::now() < end {
        if done.load(Relaxed) {
            return is_sick(dev.load(Relaxed));
        }
        std::thread::sleep(poll());
    }
    !done.load(Relaxed)
}

/// #775: `path`'s bytes for a request thread (serve's screenshots, designs, thread files), within `PROBE`: a `TimedOut` error when its folder is hung (#776), an earlier read of it has not ended, or this one takes too long, which marks the folder's disk. The stuck read keeps its own thread, and holds the path until it ends.
pub fn read_file(path: impl AsRef<std::path::Path>) -> std::io::Result<Vec<u8>> {
    let path = path.as_ref().to_path_buf();
    let late = || std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{}: no answer", path.display()));
    let end = Instant::now() + probe_wait();
    let dir = path
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    if hung(&dir, probe_wait(), list) {
        return Err(late());
    }
    let claim = Claim::take(&path.to_string_lossy(), end).ok_or_else(late)?;
    let gone = claim.gone.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let p = path.clone();
    std::thread::spawn(move || {
        let _claim = claim;
        drop(tx.send(std::fs::read(p)));
    });
    rx.recv_timeout(end.saturating_duration_since(Instant::now()))
        .unwrap_or_else(|_| {
            gone.store(true, std::sync::atomic::Ordering::Relaxed);
            sicken(&dir);
            Err(late())
        })
}

/// #775: a started child as `settle` sees it. It has no blocking wait: one stuck in disk I/O outlives SIGKILL, so nothing may wait on it. Tests use one that never ends.
pub trait Proc: Send + 'static {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    fn kill(&mut self) -> std::io::Result<()>;
}

impl Proc for std::process::Child {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        std::process::Child::try_wait(self)
    }
    fn kill(&mut self) -> std::io::Result<()> {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let group = i32::try_from(self.id()).map_or(0, |p| -p);
        // SAFETY: kill(2) only sends SIGKILL to the child's own process group, which `within` made for it alone.
        unsafe { kill(group, 9) };
        std::process::Child::kill(self)
    }
}

/// #775: how a call `settle` watched ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    Exited(std::process::ExitStatus),
    TimedOut,
    Busy,
}

type Calls = std::collections::HashMap<String, std::sync::Arc<std::sync::atomic::AtomicBool>>;

fn calls() -> std::sync::MutexGuard<'static, Calls> {
    static CALLS: std::sync::OnceLock<std::sync::Mutex<Calls>> = std::sync::OnceLock::new();
    let m = CALLS.get_or_init(std::sync::Mutex::default);
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

static FREED: std::sync::Condvar = std::sync::Condvar::new();

type Abandoned = (Box<dyn Proc>, Option<Claim>);

fn gone() -> std::sync::MutexGuard<'static, Vec<Abandoned>> {
    static GONE: std::sync::Mutex<Vec<Abandoned>> = std::sync::Mutex::new(Vec::new());
    GONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Claim {
    key: String,
    gone: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Claim {
    fn take(key: &str, until: Instant) -> Option<Claim> {
        use std::sync::atomic::Ordering::Relaxed;
        let mut m = calls();
        loop {
            match m.get(key) {
                None => {
                    let gone = std::sync::Arc::default();
                    m.insert(key.into(), std::sync::Arc::clone(&gone));
                    return Some(Claim { key: key.into(), gone });
                }
                Some(g) if g.load(Relaxed) => return None,
                Some(_) => {
                    let left = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
                    m = FREED
                        .wait_timeout(m, left)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            }
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut m = calls();
        if m.get(&self.key).is_some_and(|g| std::sync::Arc::ptr_eq(g, &self.gone)) {
            m.remove(&self.key);
        }
        drop(m);
        FREED.notify_all();
    }
}

/// #775: `start` a call on `key` (None: no path) and watch it up to `limit`. At the limit its child is killed and abandoned, never waited on, and the caller gets `TimedOut` even when the child cannot die. No second call on `key` starts while one is in flight: it waits for it (within its own limit), or is `Busy` at once while an abandoned one lives.
pub fn settle<P: Proc>(key: Option<&str>, limit: Duration, start: impl FnOnce() -> Result<P>) -> Result<Ended> {
    let end = Instant::now() + limit;
    gone().retain_mut(|(c, _)| matches!(c.try_wait(), Ok(None)));
    let claim = match key.map(|k| Claim::take(k, end)) {
        Some(None) => return Ok(Ended::Busy),
        c => c.flatten(),
    };
    let mut c = start()?;
    while Instant::now() < end {
        if let Some(st) = c.try_wait()? {
            return Ok(Ended::Exited(st));
        }
        std::thread::sleep(poll());
    }
    let _ = c.kill();
    if let Some(cl) = &claim {
        cl.gone.store(true, std::sync::atomic::Ordering::Relaxed);
        FREED.notify_all();
    }
    gone().push((Box::new(c), claim));
    Ok(Ended::TimedOut)
}

fn drain(p: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut v = vec![];
        let _ = p.map(|mut p| p.read_to_end(&mut v));
        let _ = tx.send(v);
    });
    rx
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
