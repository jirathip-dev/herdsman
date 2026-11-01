//! #60 (amendment C): src/herdr.rs is the one herdr adapter; no other src file builds a `herdr` argv.
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn only_herdr_rs_runs_herdr() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![];
    rust_files(&src, &mut files);
    assert!(files.iter().any(|f| f.ends_with("herdr.rs")), "src/herdr.rs exists");
    // The program name, or the head of a herdr argv (`"pane", "read"`, `"agent", "prompt"`, …).
    let heads = [
        "\"herdr\"",
        "[\"pane\", \"",
        "[\"agent\", \"",
        "[\"workspace\", \"",
        "[\"worktree\", \"",
    ];
    let bad: Vec<String> = files
        .iter()
        .filter(|f| !f.ends_with("src/herdr.rs"))
        .flat_map(|f| {
            let text = std::fs::read_to_string(f).unwrap();
            let lines: Vec<String> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| heads.iter().any(|h| l.contains(h)))
                .map(|(i, l)| format!("{}:{}: {}", f.display(), i + 1, l.trim()))
                .collect();
            lines
        })
        .collect();
    assert!(bad.is_empty(), "herdr argv outside src/herdr.rs:\n{}", bad.join("\n"));
}
