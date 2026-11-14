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

fn src_files() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![];
    for e in std::fs::read_dir(&crates).unwrap() {
        let src = e.unwrap().path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    assert!(
        files.iter().any(|f| f.ends_with("core/src/term.rs")),
        "src/term.rs exists"
    );
    files
}

fn lines(files: &[PathBuf], hit: impl Fn(&Path, &str) -> bool) -> Vec<String> {
    let each = |f: &PathBuf| {
        let text = std::fs::read_to_string(f).unwrap();
        let found: Vec<String> = (text.lines().enumerate())
            .filter(|(_, l)| hit(f, l))
            .map(|(i, l)| format!("{}:{}: {}", f.display(), i + 1, l.trim()))
            .collect();
        found
    };
    files.iter().flat_map(each).collect()
}

#[test]
fn only_term_rs_builds_pane_words() {
    let heads = ["[\"pane\", \"", "[\"agent\", \""];
    let bad = lines(&src_files(), |f, l| {
        !f.ends_with("src/term.rs") && heads.iter().any(|h| l.contains(h))
    });
    assert!(bad.is_empty(), "pane argv outside src/term.rs:\n{}", bad.join("\n"));
}

#[test]
fn no_src_line_names_herdr_but_its_load_error() {
    let config = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/src/config.rs")).unwrap();
    assert!(
        config.contains("pub const RETIRED_RUNTIME"),
        "the load error is in config.rs"
    );
    let bad = lines(&src_files(), |f, l| {
        let mine = f.ends_with("core/src/config.rs") && (l.contains("RETIRED_RUNTIME") || l.contains("\"herdr\" =>"));
        l.to_lowercase().contains("herdr") && !mine
    });
    assert!(bad.is_empty(), "herdr named in src:\n{}", bad.join("\n"));
}
