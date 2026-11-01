//! #63 gate 1, the style lint: raw px lengths live only in `assets/serve.css`'s first `:root` block (the
//! tokens, docs/ui.md). Outside it, a sizing property with a px value, a font size that is not a token, a
//! control height that is not `--ctl`, or an inline `style=` with px in the rendered page fails, listing `file:line value`. Each exception is
//! named below with its reason; adding one needs the owner's OK.
mod common;
use common::*;
use herdsman::Ctx;
use std::path::PathBuf;

/// (file, selector, property, reason).
const EXCEPTIONS: &[(&str, &str, &str, &str)] = &[
    (
        "serve.css",
        ".gl",
        "font-size",
        "glyph-mode avatar: a 20 px emoji picture, not text",
    ),
    (
        "serve.css",
        ".av .asc",
        "font",
        "ascii-mode avatar: 10 px character art, not text",
    ),
    (
        "serve.css",
        "#herd-on svg",
        "height",
        "header horse icon: 19×14 px picture geometry",
    ),
    (
        "serve.css",
        ".av",
        "height",
        "lane avatar box: 48×40 px picture geometry",
    ),
    (
        "serve.css",
        ".px",
        "height",
        "lane horse drawing: 48×36 px picture geometry",
    ),
    (
        "herd.css",
        ".hr",
        "height",
        "ranch strip: never shorter than 120 px of sky on a short screen",
    ),
    (
        "herd.css",
        ".hrain",
        "inset",
        "rain overlay: offset by its 60 px tile so the tile can scroll in",
    ),
    (
        "serve.css",
        "button[name=choice],button.pick",
        "height",
        "choice buttons wrap: sentences, not labels; owner-approved 2026-10-31 (#63 A; #139: wherever the form renders)",
    ),
];

const SIZED: &[&str] = &[
    "font",
    "font-size",
    "height",
    "min-height",
    "line-height",
    "gap",
    "row-gap",
    "column-gap",
    "top",
    "left",
    "right",
    "bottom",
    "inset",
];

/// The px lengths in a value, e.g. `12px` (a token var never matches: `--s3` has no px).
fn px(v: &str) -> Vec<String> {
    let b = v.as_bytes();
    let mut out = vec![];
    for i in 0..b.len().saturating_sub(1) {
        if &b[i..i + 2] == b"px" && i > 0 && b[i - 1].is_ascii_digit() {
            let s = v[..i]
                .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
                .map_or(0, |k| k + 1);
            out.push(v[s..i + 2].to_string());
        }
    }
    out
}

/// Every declaration as (line, selector, property, value), skipping the token `:root` block.
fn decls(css: &str) -> Vec<(usize, String, String, String)> {
    let mut src = css.to_string();
    while let Some(a) = src.find("/*") {
        let b = src[a..].find("*/").map_or(src.len(), |k| a + k + 2);
        let nl = "\n".repeat(src[a..b].matches('\n').count());
        src.replace_range(a..b, &nl);
    }
    let (mut out, mut stack, mut seg, mut line, mut start, mut tokens_seen) =
        (vec![], Vec::<String>::new(), String::new(), 1, 1, false);
    for c in src.chars() {
        match c {
            '{' => {
                stack.push(seg.trim().to_string());
                seg.clear();
            }
            ';' | '}' => {
                let in_tokens = stack.last().is_some_and(|s| s == ":root") && !tokens_seen && stack.len() == 1;
                if let Some((p, v)) = seg.split_once(':').filter(|_| !in_tokens) {
                    out.push((
                        start,
                        stack.last().cloned().unwrap_or_default(),
                        p.trim().to_string(),
                        v.trim().to_string(),
                    ));
                }
                seg.clear();
                if c == '}' {
                    if in_tokens {
                        tokens_seen = true;
                    }
                    stack.pop();
                }
            }
            _ => {
                if seg.trim().is_empty() && !c.is_whitespace() {
                    start = line;
                }
                seg.push(c);
            }
        }
        if c == '\n' {
            line += 1;
        }
    }
    out
}

#[test]
fn css_uses_only_the_tokens() {
    let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"));
    let mut bad = vec![];
    for file in ["serve.css", "herd.css"] {
        let css = std::fs::read_to_string(dir.join(file)).unwrap();
        if file == "serve.css" {
            assert!(
                css.starts_with(":root{") && css.contains("--fs:13px") && css.contains("--ctl:28px"),
                "serve.css opens with the token block"
            );
        }
        for (line, sel, p, v) in decls(&css) {
            if EXCEPTIONS.iter().any(|e| e.0 == file && e.1 == sel && e.2 == p) {
                continue;
            }
            let sized = SIZED.contains(&p.as_str()) || p.starts_with("margin") || p.starts_with("padding");
            for n in px(&v).into_iter().filter(|_| sized) {
                bad.push(format!("assets/{file}:{line} {sel} {{{p}:{v}}} raw {n}"));
            }
            // A control is `--ctl` tall.
            if p == "height" && (sel.contains("button") || sel.contains(".pk")) && v != "var(--ctl)" {
                bad.push(format!(
                    "assets/{file}:{line} {sel} {{{p}:{v}}} a control is var(--ctl) tall"
                ));
            }
            // `font:inherit` keeps the parent's token; any other font size names one.
            if (p == "font-size" || (p == "font" && v != "inherit")) && !v.starts_with("var(--fs") {
                bad.push(format!(
                    "assets/{file}:{line} {sel} {{{p}:{v}}} font size is not a --fs token"
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "UI token lint (docs/ui.md), {} outliers:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn rendered_page_has_no_inline_px() {
    let demo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/demo"));
    let d = tmpdir("ui-tokens");
    let c = Ctx {
        config: demo.join("fleet.toml"),
        state: demo.join("events.jsonl"),
        worktrees: d.join("wt"),
        hermes: d.join("hermes"),
        exe: "herdsman".into(),
        now: 1,
        home: d.join("home"),
        hints: vec![],
    };
    herdsman::serve::export(&c, &Fake::default(), &d.join("site"), true).unwrap();
    let page = std::fs::read_to_string(d.join("site/index.html")).unwrap();
    let bad: Vec<String> = page
        .match_indices("style=")
        .filter_map(|(i, _)| {
            let v = &page[i + 6..];
            let end = match v.chars().next() {
                Some(q @ ('"' | '\'')) => v[1..].find(q).map_or(v.len(), |k| k + 2),
                _ => v.find([' ', '>']).unwrap_or(v.len()),
            };
            (!px(&v[..end]).is_empty()).then(|| format!("index.html style={}", &v[..end]))
        })
        .collect();
    assert!(bad.is_empty(), "inline px in the rendered page:\n{}", bad.join("\n"));
}

#[test]
fn the_lint_bites() {
    let css = ":root{--fs:13px}\n.a{font-size:14px;padding:0 var(--s1)}\n.b{margin:-4px 0}";
    let d = decls(css);
    assert_eq!(d.len(), 3, "{d:?}");
    assert_eq!((d[0].0, px(&d[0].3)), (2, vec!["14px".to_string()]));
    assert!(px(&d[1].3).is_empty());
    assert_eq!(px(&d[2].3), ["4px"]);
}
