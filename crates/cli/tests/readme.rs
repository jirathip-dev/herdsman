mod common;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_drover");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn readme() -> String {
    std::fs::read_to_string(root().join("README.md")).unwrap()
}

fn doc_files() -> Vec<PathBuf> {
    let mut v = vec![root().join("README.md")];
    for e in std::fs::read_dir(root().join("docs")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "md") {
            v.push(p);
        }
    }
    v
}

#[test]
fn readme_is_short() {
    let n = readme().lines().count();
    assert!(n <= 150, "README.md has {n} lines");
}

#[test]
fn readme_commands_are_in_help_agent() {
    let help = Command::new(BIN).args(["help", "--agent"]).output().unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    let text = readme();
    let table = text.lines().filter(|l| l.starts_with("| `"));
    let cells = table.flat_map(|row| row[2..].split(" | ").next().unwrap().split(" / "));
    let mut cmds: Vec<&str> = cells
        .map(|c| c.trim_matches('`').split_whitespace().next().unwrap())
        .collect();
    cmds.extend(
        text.lines()
            .filter_map(|l| l.strip_prefix("drover ")?.split_whitespace().next()),
    );
    assert!(cmds.len() >= 20, "{cmds:?}");
    for cmd in cmds {
        let named = |l: &&str| {
            l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .any(|w| w == cmd)
        };
        assert!(
            help.lines().filter(|l| l.contains("drover ")).any(|l| named(&l)),
            "README names `{cmd}`, not in drover help --agent"
        );
    }
}

#[test]
fn table_commands_exist() {
    let help = Command::new(BIN).arg("--help").output().unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    let known: Vec<&str> = help
        .lines()
        .filter_map(|l| l.strip_prefix("  ")?.split_whitespace().next())
        .collect();
    let mut n = 0;
    for row in readme().lines().filter(|l| l.starts_with("| `")) {
        for cell in row[2..].split(" | ").next().unwrap().split(" / ") {
            let cmd = cell.trim_matches('`').split_whitespace().next().unwrap();
            assert!(known.contains(&cmd), "README lists `{cmd}`, not in drover --help");
            n += 1;
        }
    }
    assert!(n >= 10, "commands table not found");
}

#[test]
fn doc_links_resolve() {
    for f in doc_files() {
        let text = std::fs::read_to_string(&f).unwrap();
        for part in text.split("](").skip(1) {
            let target = part.split(')').next().unwrap();
            if target.starts_with("http") {
                continue;
            }
            let p = f.parent().unwrap().join(target.split('#').next().unwrap());
            assert!(p.exists(), "{}: broken link {target}", f.display());
        }
    }
}

#[test]
fn docs_leak_no_ids_tokens_or_personal_paths() {
    for f in doc_files() {
        let text = std::fs::read_to_string(&f).unwrap();
        for bad in ["/Users/", "/home/", "telegram:-", "ghp_", "github_pat_", "sk-ant-"] {
            assert!(!text.contains(bad), "{}: contains {bad}", f.display());
        }
        let text: String = text
            .split("\"ts\":")
            .map(|p| p.trim_start_matches(|c: char| c.is_ascii_digit()))
            .collect();
        let longest = text
            .split(|c: char| !c.is_ascii_digit())
            .map(str::len)
            .max()
            .unwrap_or(0);
        assert!(
            longest < 9,
            "{}: a {longest}-digit number looks like a chat ID",
            f.display()
        );
    }
}

#[test]
fn quick_start_runs_as_written() {
    let text = readme();
    let block = text.split("```sh\n").nth(1).unwrap().split("```").next().unwrap();
    let home = common::tmpdir("readme-quick-start");
    let bindir = Path::new(BIN).parent().unwrap();
    let shed = home.join("bin/shed");
    std::fs::create_dir_all(shed.parent().unwrap()).unwrap();
    std::fs::write(&shed, "#!/bin/sh\n").unwrap();
    let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&shed, mode).unwrap();
    let path = format!(
        "{}:{}/bin:{}",
        bindir.display(),
        home.display(),
        std::env::var("PATH").unwrap()
    );
    let cmd = |prog: &str| {
        let mut c = Command::new(prog);
        c.env("HOME", &home).env("DROVER_HOME", &home).env("PATH", &path);
        for k in [
            "FLEET_CONFIG",
            "FLEET_STATE",
            "FLEET_WORKTREES",
            "FLEET_HERMES_PROFILES",
            "DROVER_LANE",
            "FLEET_LANE",
        ] {
            c.env_remove(k);
        }
        c
    };
    let script: String = block
        .lines()
        .filter(|l| !l.starts_with("cargo install") && !l.starts_with("drover serve"))
        .filter(|l| !l.starts_with("drover timer"))
        .map(|l| format!("{l}\n"))
        .collect();
    let out = cmd("sh").args(["-e", "-c", &script]).output().unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "quick start failed:\n{script}\n{all}");
    assert!(all.contains("issue-12"), "spawn --dry-run printed no plan:\n{all}");
    assert!(all.contains("no tracked issues"), "{all}");

    let serve = block.lines().find(|l| l.starts_with("drover serve")).unwrap();
    let mut args: Vec<String> = serve
        .split('#')
        .next()
        .unwrap()
        .split_whitespace()
        .skip(1)
        .map(String::from)
        .collect();
    let i = args.iter().position(|a| a == "--addr").unwrap() + 1;
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    args[i] = addr.clone();
    let mut child = cmd(BIN).args(&args).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let body = loop {
        if let Ok(mut s) = std::net::TcpStream::connect(&addr) {
            s.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
            let mut b = String::new();
            s.read_to_string(&mut b).unwrap();
            break b;
        }
        assert!(Instant::now() < deadline, "serve never listened on {addr}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(body.starts_with("HTTP/1.") && body.contains(" 200 "), "{body}");
}

fn setup() -> String {
    std::fs::read_to_string(root().join("docs/setup.md")).unwrap()
}

fn blocks<'a>(text: &'a str, lang: &str) -> Vec<&'a str> {
    let fence = format!("```{lang}\n");
    text.split(fence.as_str())
        .skip(1)
        .map(|b| b.split("```").next().unwrap())
        .collect()
}

#[test]
fn readme_links_setup() {
    assert!(readme().contains("](docs/setup.md)"));
}

#[test]
fn setup_commands_exist() {
    let help = Command::new(BIN).arg("--help").output().unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    let known: Vec<&str> = help
        .lines()
        .filter_map(|l| l.strip_prefix("  ")?.split_whitespace().next())
        .collect();
    let text = setup();
    let mut n = 0;
    for b in blocks(&text, "sh").into_iter().chain(blocks(&text, "ini")) {
        for part in b.split("drover ").skip(1) {
            let cmd = part.split_whitespace().next().unwrap();
            if cmd.starts_with('-') || cmd.starts_with('#') {
                continue;
            }
            assert!(
                known.contains(&cmd),
                "setup.md runs `drover {cmd}`, not in drover --help"
            );
            n += 1;
        }
    }
    assert!(n >= 5, "only {n} drover commands found");
}

#[test]
fn setup_toml_parses() {
    let text = setup();
    let all = blocks(&text, "toml");
    assert!(all.len() >= 3, "toml blocks not found");
    let mut file = String::new();
    for b in all {
        file.push_str(b);
        drover::config::Config::parse(&file).unwrap_or_else(|e| panic!("{e:#}\n{file}"));
    }
}

#[test]
fn docs_name_no_real_tailnet_host() {
    for f in doc_files() {
        let text = std::fs::read_to_string(&f).unwrap();
        for (i, _) in text.match_indices(".ts.net") {
            let before = text[..i].chars().last().unwrap();
            assert!(
                before == '>' || before == '*',
                "{}: a real ts.net hostname near byte {i}",
                f.display()
            );
        }
    }
}
