//! #82: shipped code and docs stay setup-neutral: no install's own names (people, repos, paths,
//! hosts). The person in charge is "the owner"; examples use `acme/*` and `<owner>/<repo>`.
use std::path::{Path, PathBuf};

/// Matched case-insensitively anywhere in a shipped file.
const WORDS: [&str; 7] = ["conductor", "jirathip", "synergy", "sendmeter", "claude-accounts", "tail8c3301", "hermes-brain"];

/// (file, exact text with whitespace collapsed, why it may stay). Each one is removed before the scan.
const EXCEPTIONS: [(&str, &str, &str); 8] = [
    ("assets/herd.js", "jirathip-dev/corral", "MIT notice: the source Herd mode is ported from"),
    ("assets/herd.js", "MIT © jirathip-dev", "MIT notice of the ported Corral code"),
    ("assets/horses.svg", "(c) 2026 jirathip-dev, MIT", "MIT notice of the ported Corral horses"),
    ("docs/serve.md", "jirathip-dev/corral", "MIT notice: the source the horses, icon and Herd mode are ported from"),
    ("docs/serve.md", "MIT © jirathip-dev", "MIT notice of the ported Corral code and icon"),
    ("docs/serve.md", "https://jirathip-dev.github.io/herdsman/", "the project's own public demo"),
    ("docs/dsh.md", "https://github.com/jirathip-dev/herdsman/issues/", "the project's own issue links"),
    ("docs/setup.md", "https://github.com/jirathip-dev/herdsman/issues/", "the project's own issue links"),
];

fn files(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        for e in std::fs::read_dir(p).unwrap() {
            files(&e.unwrap().path(), out);
        }
    } else {
        out.push(p.to_path_buf());
    }
}

#[test]
fn shipped_files_name_no_install() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = vec![];
    for p in ["src", "assets", "roles", "docs", "README.md", "fleet.example.toml"] {
        files(&root.join(p), &mut all);
    }
    let (mut bad, mut used) = (vec![], [false; EXCEPTIONS.len()]);
    for f in all {
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        // Images and other binaries hold no text to check.
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for (i, (file, allowed, _why)) in EXCEPTIONS.iter().enumerate() {
            if *file == rel && text.contains(allowed) {
                used[i] = true;
                text = text.replace(allowed, "");
            }
        }
        let lower = text.to_lowercase();
        bad.extend(WORDS.iter().filter(|w| lower.contains(*w)).map(|w| format!("{rel}: {w}")));
    }
    // A stale exception goes too, so the list never outgrows what ships.
    bad.extend(EXCEPTIONS.iter().zip(used).filter(|(_, u)| !u).map(|((f, a, _), _)| format!("{f}: unused exception {a:?}")));
    assert!(bad.is_empty(), "setup-specific names in shipped files (#82):\n{}", bad.join("\n"));
}
