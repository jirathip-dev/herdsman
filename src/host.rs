//! Host-load gate for `herdsman spawn` (an unreadable signal admits) and the live-agent view.
use crate::config::Limits;
pub use crate::herdr::agents;
use crate::runner::{ok, Runner};
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

/// Live agents named `name` or running inside `dir`.
pub fn live_in<'a>(agents: &'a [Value], name: &str, dir: &Path) -> Vec<&'a Value> {
    let inside = |a: &Value, k: &str| a[k].as_str().is_some_and(|c| Path::new(c).starts_with(dir));
    agents.iter().filter(|a| a["name"] == name || inside(a, "cwd") || inside(a, "foreground_cwd")).collect()
}

#[derive(Debug, Default, Clone)]
pub struct Host {
    /// normal | warn | critical
    pub pressure: Option<String>,
    pub swapout: Option<f64>,
    pub load_per_cpu: Option<f64>,
}

fn stdout(r: &dyn Runner, prog: &str, args: &[&str]) -> Option<String> {
    ok(r, prog, args).ok()
}

fn swapouts(r: &dyn Runner) -> Option<f64> {
    let s = stdout(r, "vm_stat", &[])?;
    let line = s.lines().find(|l| l.trim_start().starts_with("Swapouts:"))?;
    line.split(':').nth(1)?.trim().trim_end_matches('.').parse().ok()
}

/// Sample the host (macOS: sysctl + two vm_stat samples 2 s apart).
pub fn sample(r: &dyn Runner) -> Host {
    let pressure = stdout(r, "sysctl", &["-n", "kern.memorystatus_vm_pressure_level"]).and_then(|s| {
        match s.trim() {
            "1" | "normal" => Some("normal"),
            "2" | "warn" => Some("warn"),
            "4" | "critical" => Some("critical"),
            _ => None,
        }
        .map(String::from)
    });
    let swapout = swapouts(r).and_then(|a| {
        r.sleep(2);
        swapouts(r).map(|b| (b - a).max(0.0))
    });
    let load = stdout(r, "sysctl", &["-n", "vm.loadavg"]).and_then(|s| s.split_whitespace().find_map(|w| w.parse::<f64>().ok()));
    let ncpu = stdout(r, "sysctl", &["-n", "hw.ncpu"]).and_then(|s| s.trim().parse::<f64>().ok());
    let load_per_cpu = load.zip(ncpu.filter(|n| *n > 0.0)).map(|(l, n)| l / n);
    Host { pressure, swapout, load_per_cpu }
}

/// Err(reason) when any signal is over its limit.
pub fn admit(h: &Host, l: &Limits) -> Result<(), String> {
    let mut why = vec![];
    if let Some(p) = h.pressure.as_deref().filter(|p| *p != "normal") {
        why.push(format!("memory pressure {p}"));
    }
    if let Some(s) = h.swapout.filter(|s| *s > l.swapout_rate) {
        why.push(format!("swap-out {s} pages/2s > {}", l.swapout_rate));
    }
    if let Some(x) = h.load_per_cpu.filter(|x| *x > l.load_per_cpu) {
        why.push(format!("load/cpu {x:.2} > {}", l.load_per_cpu));
    }
    if why.is_empty() {
        Ok(())
    } else {
        Err(why.join("; "))
    }
}
