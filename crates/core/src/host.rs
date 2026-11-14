use crate::config::Limits;
use crate::runner::{ok, Runner};
pub use crate::term::agents;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

/// Live agents named `name` or running inside `dir`.
pub fn live_in<'a>(agents: &'a [Value], name: &str, dir: &Path) -> Vec<&'a Value> {
    let inside = |a: &Value, k: &str| a[k].as_str().is_some_and(|c| Path::new(c).starts_with(dir));
    agents
        .iter()
        .filter(|a| a["name"] == name || inside(a, "cwd") || inside(a, "foreground_cwd"))
        .collect()
}

const SPARE: &[&str] = &["shed", "drover", "herdsman", "launchd"];

/// #186: `(pid, command, cwd)` of every process `lsof` sees, but `SPARE`, pid 1, this process and its parent.
pub fn cwds(r: &dyn Runner) -> Vec<(String, String, String)> {
    let me = [1, std::process::id(), std::os::unix::process::parent_id()].map(|p| p.to_string());
    let (mut out, mut p) = (vec![], (String::new(), String::new()));
    for l in stdout(r, "lsof", &["-n", "-d", "cwd", "-F", "pcn"])
        .unwrap_or_default()
        .lines()
    {
        match l.split_at(l.len().min(1)) {
            ("p", pid) => p = (pid.into(), String::new()),
            ("c", c) => p.1 = c.into(),
            ("n", n) if !me.contains(&p.0) && !SPARE.contains(&p.1.as_str()) => {
                out.push((p.0.clone(), p.1.clone(), n.into()));
            }
            _ => {}
        }
    }
    out
}

/// #186: the processes of `ps` whose cwd is inside `dir` (an absolute path at least two levels deep).
pub fn inside(ps: &[(String, String, String)], dir: &Path) -> Vec<(String, String, String)> {
    let deep = dir.is_absolute() && dir.components().count() > 2;
    ps.iter()
        .filter(|p| deep && Path::new(&p.2).starts_with(dir))
        .cloned()
        .collect()
}

/// #186: TERM every process inside `dir` (#223: with `trim`, all but the pane's shell and the agent, the top two levels there; #679: and never `spare`'s tree, a shed lane's agent), wait 5 s, KILL those still there; the commands signalled and the MB they held.
pub fn reap(r: &dyn Runner, dir: &Path, trim: bool, spare: Option<&str>) -> (Vec<String>, u64) {
    let (mut first, ps) = (inside(&cwds(r), dir), stdout(r, "ps", &["-axo", "pid=,ppid=,rss="]));
    let ps = ps.filter(|_| trim).unwrap_or_default();
    let ps: Vec<Vec<&str>> = ps.lines().map(|l| l.split_whitespace().collect()).collect();
    let col = |pid: &str, i| Some(*ps.iter().find(|w| w.first() == Some(&pid))?.get(i)?);
    let all = first.clone();
    let here = |p: Option<&str>| all.iter().any(|a| Some(a.0.as_str()) == p);
    first.retain(|p| !trim || here(col(&p.0, 1)) && here(col(&p.0, 1).and_then(|q| col(q, 1))));
    let agents = |p: &str| {
        std::iter::successors(Some(p), |q| col(q, 1))
            .take(64)
            .any(|q| Some(q) == spare)
    };
    first.retain(|p| !agents(&p.0));
    let kb: u64 = first.iter().filter_map(|p| col(&p.0, 2)?.parse::<u64>().ok()).sum();
    let kill = |sig: &str, ps: &[(String, String, String)]| {
        let mut args = vec![sig.to_string()];
        args.extend(ps.iter().map(|p| p.0.clone()));
        let _ = r.run("kill", &args);
    };
    if first.is_empty() {
        return (vec![], 0);
    }
    kill("-TERM", &first);
    r.sleep(5);
    let mut left = inside(&cwds(r), dir);
    left.retain(|p| first.contains(p));
    if !left.is_empty() {
        kill("-KILL", &left);
    }
    (first.into_iter().map(|p| p.1).collect(), kb / 1024)
}

/// #710: a process stuck on the Keychain: pid, seconds up, its parent's name (never an argument).
pub type Stuck = (String, u64, String);

/// #710: `security find-generic-password` readers up over 60 s and a `SecurityAgent` up over 10 min (troubleshooting §2); securityd and every other `security` verb never match.
pub fn keychain_jam(r: &dyn Runner) -> (Vec<Stuck>, Option<Stuck>) {
    let ps = stdout(r, "ps", &["-axo", "pid=,ppid=,etime=,command="]).unwrap_or_default();
    let rows: Vec<Vec<&str>> = ps.lines().map(|l| l.split_whitespace().collect()).collect();
    let name = |w: Option<&&str>| w.map_or("?", |w| w.rsplit('/').next().unwrap_or(w)).to_string();
    let parent = |ppid: &str| name(rows.iter().find(|w| w.first() == Some(&ppid)).and_then(|w| w.get(3)));
    let (mut readers, mut agent) = (vec![], None);
    for w in rows.iter().filter(|w| w.len() > 3) {
        let up = w[2].split(['-', ':']).rev().zip([1, 60, 3600, 86_400]);
        let up: u64 = up.map(|(n, k)| n.parse::<u64>().unwrap_or(0) * k).sum();
        let stuck = (w[0].to_string(), up, parent(w[1]));
        match (name(w.get(3)).as_str(), w.get(4)) {
            ("security", Some(&"find-generic-password")) if up > 60 => readers.push(stuck),
            ("SecurityAgent", _) if up > 600 => agent = Some(stuck),
            _ => {}
        }
    }
    (readers, agent)
}

/// #710: `keychain_jam`'s readers and agent as one line, with troubleshooting §2's fix for those pids.
pub fn jam_line((readers, agent): &(Vec<Stuck>, Option<Stuck>)) -> String {
    let each: Vec<String> = readers.iter().map(|p| format!("{} ({})", p.0, p.2)).collect();
    let pids: Vec<&str> = readers.iter().map(|p| p.0.as_str()).collect();
    let oldest = readers.iter().map(|p| p.1).max();
    let oldest = oldest.map_or(String::new(), |t| format!(", oldest {}", crate::events::age(t)));
    let up = agent.as_ref().map_or(String::new(), |a| {
        format!("; SecurityAgent {} up {}", a.0, crate::events::age(a.1))
    });
    format!(
        "{} waiting over 60 s{oldest}: {}{up} → kill {} && kill \"$(pgrep -x SecurityAgent)\" (docs/troubleshooting.md §2)",
        readers.len(),
        each.join(", "),
        pids.join(" ")
    )
}

/// #248: the MB each agent's processes hold (those whose cwd is inside the agent's cwd).
pub fn held(r: &dyn Runner, agents: &[&Value]) -> Vec<u64> {
    let (ps, rss) = (cwds(r), stdout(r, "ps", &["-axo", "pid=,rss="]).unwrap_or_default());
    let rss: std::collections::BTreeMap<&str, &str> = rss.lines().filter_map(|l| l.trim().split_once(' ')).collect();
    let kb = |pid: &str| rss.get(pid)?.trim().parse::<u64>().ok();
    let mb = |a: &&Value| {
        let dir = Path::new(a["cwd"].as_str().unwrap_or("\0"));
        inside(&ps, dir).iter().filter_map(|p| kb(&p.0)).sum::<u64>() / 1024
    };
    agents.iter().map(mb).collect()
}

/// #223: the top 3 memory users by footprint (`top`) as `mem command (owner)`: `lane <agent>` when inside a live agent's cwd, else `other`; a virtual machine is `mem VM (colima)` (#234: no nested brackets).
pub fn top(r: &dyn Runner, agents: &[Value]) -> Vec<String> {
    let (ps, out) = (cwds(r), stdout(r, "top", &TOP).unwrap_or_default());
    let row = |l: &str| {
        let (pid, w) = l.trim().split_once(' ')?;
        let (cmd, mem) = w.trim().rsplit_once(' ')?;
        let cwd = Path::new(ps.iter().find(|p| p.0 == pid).map_or("\0", |p| &p.2));
        let lane = agents
            .iter()
            .find(|a| a["cwd"].as_str().is_some_and(|d| cwd.starts_with(d)));
        if l.contains("Virtua") || l.contains("qemu") {
            return Some(format!("{mem} VM (colima)"));
        }
        let owner = lane.map_or("other".into(), |a| {
            format!("lane {}", a["name"].as_str().unwrap_or_default())
        });
        Some(format!("{mem} {} ({owner})", cmd.trim()))
    };
    out.split_once("MEM")
        .map_or(vec![], |(_, t)| t.lines().filter_map(row).collect())
}
/// #352: the MB `ps` sees per group, never per PID: lanes (a harness's process), CI/Colima, browsers, drover, other.
pub fn groups(r: &dyn Runner) -> [u64; 5] {
    const G: [&str; 3] = ["colima qemu lima virtualiz", "chrom safari firefox", "herd drover"];
    let (ps, mut kb) = (stdout(r, "ps", &["-axo", "rss=,command="]).unwrap_or_default(), [0; 5]);
    for (k, cmd) in ps.to_lowercase().lines().filter_map(|l| l.trim().split_once(' ')) {
        let lane = crate::providers::lanes().any(|h| h.process_names().iter().any(|w| cmd.contains(w)));
        let i = G.iter().position(|g| g.split(' ').any(|w| cmd.contains(w)));
        kb[if lane { 0 } else { i.map_or(4, |i| i + 1) }] += k.parse::<u64>().unwrap_or(0);
    }
    kb.map(|k| k / 1024)
}
const TOP: [&str; 8] = ["-l", "1", "-o", "mem", "-stats", "pid,command,mem", "-n", "3"];

#[derive(Debug, Default, Clone)]
pub struct Host {
    /// normal | warn | critical
    pub pressure: Option<String>,
    pub swapout: Option<f64>,
    /// #168: the median of the last `WINDOW` swap-out samples (`window`); the gate reads this, not one spike.
    pub swap_median: Option<f64>,
    pub load_per_cpu: Option<f64>,
    /// #168: `vm_stat` free + inactive pages, in GB.
    pub free_mem_gb: Option<f64>,
    /// #234: the memory size in GB.
    pub mem_total_gb: Option<f64>,
    /// #825: the 3 busiest processes drover did not start, `name cpu%`.
    pub top: Vec<String>,
}

fn own_load(ps: &str) -> (f64, Vec<String>) {
    let row = |l: &str| {
        let mut w = l.split_whitespace();
        let (pid, parent, cpu) = (
            w.next()?.parse::<u64>().ok()?,
            w.next()?.parse::<u64>().ok()?,
            w.next()?,
        );
        let comm = w.collect::<Vec<_>>().join(" ");
        let name = comm.rsplit('/').next().unwrap_or_default().to_string();
        Some((pid, parent, cpu.parse::<f64>().ok()?, name))
    };
    let procs: Vec<_> = ps.lines().filter_map(row).collect();
    let ours: Vec<u64> = (procs.iter().filter(|p| ["drover", "herdsman"].contains(&p.3.as_str())))
        .map(|p| p.0)
        .collect();
    let (own, mut other): (Vec<_>, Vec<_>) =
        (procs.iter()).partition(|p| ours.contains(&p.0) || p.3 == "cp" && ours.contains(&p.1));
    other.sort_by(|a, b| b.2.total_cmp(&a.2));
    let top = other.iter().take(3).map(|p| format!("{} {:.0}%", p.3, p.2)).collect();
    (own.iter().map(|p| p.2 / 100.0).sum(), top)
}

fn stdout(r: &dyn Runner, prog: &str, args: &[&str]) -> Option<String> {
    ok(r, prog, args).ok()
}

fn field(s: &str, name: &str) -> Option<f64> {
    let line = s.lines().find(|l| l.trim_start().starts_with(name))?;
    line.split(':').nth(1)?.trim().trim_end_matches('.').parse().ok()
}

fn mem_gb(s: &str) -> Option<f64> {
    let page: f64 = s
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some((field(s, "Pages free:")? + field(s, "Pages inactive:")?) * page / 1_073_741_824.0)
}

/// Sample the host (macOS: sysctl + two `vm_stat` samples 2 s apart).
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
    let a = stdout(r, "vm_stat", &[]);
    let b = a.as_ref().and_then(|_| {
        r.sleep(2);
        stdout(r, "vm_stat", &[])
    });
    let (a, b) = (a.unwrap_or_default(), b.unwrap_or_default());
    let swapout = field(&a, "Swapouts:")
        .zip(field(&b, "Swapouts:"))
        .map(|(a, b)| (b - a).max(0.0));
    let load = stdout(r, "sysctl", &["-n", "vm.loadavg"])
        .and_then(|s| s.split_whitespace().find_map(|w| w.parse::<f64>().ok()));
    let ncpu = stdout(r, "sysctl", &["-n", "hw.ncpu"]).and_then(|s| s.trim().parse::<f64>().ok());
    let linux = |p: &str, a: &[&str]| b.is_empty().then(|| stdout(r, p, a)).flatten().unwrap_or_default();
    let word = |s: String, n: usize| s.split_whitespace().nth(n)?.parse::<f64>().ok();
    let load = load.or_else(|| word(linux("cat", &["/proc/loadavg"]), 0));
    let ncpu = ncpu.or_else(|| word(linux("nproc", &[]), 0));
    let avail = || Some(word(linux("grep", &["MemAvailable", "/proc/meminfo"]), 1)? / 1_048_576.0);
    let (own, top) = own_load(&stdout(r, "ps", &["-Ao", "pid=,ppid=,pcpu=,comm="]).unwrap_or_default());
    let load_per_cpu = load.zip(ncpu.filter(|n| *n > 0.0)).map(|(l, n)| (l - own).max(0.0) / n);
    let size = stdout(r, "sysctl", &["-n", "hw.memsize"]).and_then(|s| s.trim().parse::<f64>().ok());
    let size = size.map(|b| b / 1_073_741_824.0);
    let size = size.or_else(|| Some(word(linux("grep", &["MemTotal", "/proc/meminfo"]), 1)? / 1_048_576.0));
    Host {
        pressure,
        swapout,
        swap_median: None,
        load_per_cpu,
        free_mem_gb: mem_gb(&b).or_else(avail),
        mem_total_gb: size,
        top,
    }
}

const WINDOW: usize = 5;

/// Add `h`'s swap-out to the rolling window and set `h.swap_median` from it.
pub fn window(state: &Path, h: &mut Host) {
    let file = state.with_file_name("swapouts");
    let old = std::fs::read_to_string(&file).unwrap_or_default();
    let mut w: Vec<f64> = old.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    w.extend(h.swapout);
    let w = &w[w.len().saturating_sub(WINDOW)..];
    let _ = std::fs::write(&file, w.iter().map(f64::to_string).collect::<Vec<_>>().join(" "));
    let mut m = w.to_vec();
    m.sort_by(f64::total_cmp);
    h.swap_median = m.get(m.len() / 2).copied();
}

/// #103: free GB on `/` (`df -k`, when readable); a disk row is amber within 25 % of `min_free_gb`, red under it (#141).
pub fn free_gb(r: &dyn Runner) -> Option<f64> {
    df(r).map(|d| d.0)
}

/// #234: free and total GB on `/`.
pub fn df(r: &dyn Runner) -> Option<(f64, f64)> {
    let s = stdout(r, "df", &["-k", "/"])?;
    let w: Vec<&str> = s.lines().nth(1)?.split_whitespace().collect();
    let gb = |i: usize| Some(w.get(i)?.parse::<f64>().ok()? / 1_048_576.0);
    gb(3).zip(gb(1))
}

/// Err(reason) when any signal is over its limit.
pub fn admit(h: &Host, l: &Limits) -> Result<(), String> {
    let mut why = vec![];
    let pressed = h.pressure.as_deref().filter(|p| *p != "normal");
    if let Some(p) = pressed {
        why.push(format!("memory pressure {p}"));
    }
    let median = h
        .swap_median
        .or(h.swapout)
        .filter(|s| *s > l.swapout_rate && pressed.is_some());
    if let Some(s) = median {
        why.push(format!("swap-out median {s} pages/2s > {}", l.swapout_rate));
    }
    if let Some(x) = h.load_per_cpu.filter(|x| *x > l.load_per_cpu) {
        let top = (!h.top.is_empty()).then(|| format!(" (top: {})", h.top.join(", ")));
        why.push(format!(
            "load/cpu {x:.2} > {}{}",
            l.load_per_cpu,
            top.unwrap_or_default()
        ));
    }
    if why.is_empty() {
        Ok(())
    } else {
        Err(why.join("; "))
    }
}
