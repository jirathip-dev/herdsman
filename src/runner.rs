//! The one seam for external tools (herdr, gh, git). Tests swap in a fake.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Command;

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
    /// Pause between polls/retries; fakes skip it.
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

pub struct Real;

impl Runner for Real {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        let o = Command::new(prog).args(args).output().with_context(|| format!("cannot run {prog}"))?;
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        Ok(Out { code: o.status.code().unwrap_or(-1), stdout: text(&o.stdout), stderr: text(&o.stderr) })
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        use std::io::Write;
        use std::process::Stdio;
        let mut c = Command::new(prog).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().with_context(|| format!("cannot run {prog}"))?;
        let _ = c.stdin.take().unwrap().write_all(input.as_bytes());
        let o = c.wait_with_output()?;
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        Ok(Out { code: o.status.code().unwrap_or(-1), stdout: text(&o.stdout), stderr: text(&o.stderr) })
    }
}

pub fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
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
    serde_json::from_str(&s).with_context(|| format!("{prog} {}: not JSON", args.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(" ")))
}
