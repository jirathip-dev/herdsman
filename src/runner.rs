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

pub trait Runner {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out>;
    /// Pause between polls/retries; fakes skip it.
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

pub struct Real;

impl Runner for Real {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        let o = Command::new(prog)
            .args(args)
            .output()
            .with_context(|| format!("cannot run {prog}"))?;
        Ok(Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        })
    }
}

pub fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// Run and fail on a non-zero exit; returns stdout.
pub fn ok(r: &dyn Runner, prog: &str, args: &[&str]) -> Result<String> {
    let o = r.run(prog, &argv(args))?;
    if o.code != 0 {
        bail!("{prog} {} failed ({}): {}", args.join(" "), o.code, o.stderr.trim());
    }
    Ok(o.stdout)
}

pub fn json(r: &dyn Runner, prog: &str, args: &[&str]) -> Result<Value> {
    let s = ok(r, prog, args)?;
    serde_json::from_str(&s).with_context(|| format!("{prog} {}: not JSON", args.join(" ")))
}
