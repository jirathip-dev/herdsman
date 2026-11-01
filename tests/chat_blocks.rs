//! #138: the lane sheet's chat. The rolling per-lane transcript (append past the overlap, once; a gap line where reads
//! do not meet; the last 5,000 lines; mode 0600), and serve.js's pure chat part under node: SGR to the board's tokens,
//! markdown escaping, role runs from a captured Claude (ANSI) and dsh screen, capped tool output. `BLESS=1` rewrites
//! the `.blocks.json` goldens.
mod common;
use common::*;
use herdsman::lanes::{body, roll, strip, GAP};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn lines(s: &[&str]) -> Vec<String> {
    s.iter().map(|l| l.to_string()).collect()
}

fn file(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p).unwrap().lines().map(String::from).collect()
}

#[test]
fn overlapping_reads_append_once_and_a_miss_writes_a_gap() {
    let f = tmpdir("roll").join("transcripts/a.log");
    // The last block (`⏺ b`) is live: sent, not written.
    let (all, live) = roll(&f, &lines(&["> p", "⏺ a", "  x", "⏺ b"]));
    assert_eq!((all.len(), live), (4, 1));
    assert_eq!(file(&f), ["> p", "⏺ a", "  x"]);
    assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    // The same read again, then one that overlaps by a line (in other colours: compared as text).
    roll(&f, &lines(&["> p", "⏺ a", "  x", "⏺ b"]));
    roll(&f, &lines(&["\x1b[2m  x\x1b[0m", "⏺ b", "  y", "⏺ c"]));
    assert_eq!(file(&f), ["> p", "⏺ a", "  x", "⏺ b", "  y"]);
    // No overlap: the lane scrolled past between reads.
    let (all, _) = roll(&f, &lines(&["⏺ z", "  q", "⏺ w"]));
    assert_eq!(file(&f)[5..], [GAP, "⏺ z", "  q"]);
    assert_eq!(all.last().unwrap(), "⏺ w");
}

#[test]
fn the_transcript_keeps_the_last_5000_lines() {
    let f = tmpdir("roll-cap").join("transcripts/a.log");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, (0..4995).map(|i| format!("  {i}\n")).collect::<String>()).unwrap();
    let read: Vec<String> = (0..10).map(|i| format!("  new {i}")).chain(["⏺ live".into()]).collect();
    roll(&f, &[lines(&["  4994"]), read].concat());
    let got = file(&f);
    assert_eq!(
        (got.len(), got[0].as_str(), got[4999].as_str()),
        (5000, "  5", "  new 9")
    );
}

#[test]
fn ansi_lines_mask_run_by_run_and_strip_to_text() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let screen = format!("\x1b[0m\x1b[38;2;255;255;255m⏺ \x1b[0mtoken {secret} \x1b[1mbold\x1b[0m\n");
    let b = body(&screen);
    assert!(!b[0].contains(secret) && b[0].contains("\x1b[1mbold"), "{b:?}");
    assert_eq!(strip(&b[0]), "⏺ token ghp_••• bold");
}

/// serve.js's pure chat part plus `js`, run under node: its stdout. None where node is missing.
fn node(js: &str) -> Option<String> {
    if std::process::Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: no node here");
        return None;
    }
    let src = include_str!("../assets/serve.js");
    let part = &src[src.find("// #138 chat blocks").unwrap()..src.find("// chat:end").unwrap()];
    let d = tmpdir("chat-js");
    std::fs::write(d.join("t.js"), format!("{part}\n{js}")).unwrap();
    let o = std::process::Command::new("node").arg(d.join("t.js")).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    Some(String::from_utf8(o.stdout).unwrap())
}

/// The SGRs measured on two lanes (docs/design/chat-blocks/README.md) to token classes: never a terminal hex.
#[test]
fn sgr_maps_to_the_nearest_board_token() {
    let cases = [
        ("38;2;248;248;242", ""),
        ("38;2;153;153;153", "t-m"),
        ("38;2;117;113;94", "t-m"),
        ("2", "t-m"),
        ("38;2;177;185;249", "t-l"),
        ("1", "t-b"),
        ("38;2;80;200;80", "t-g"),
        ("38;2;166;226;46", "t-g"),
        ("38;2;78;186;101", "t-g"),
        ("38;2;220;90;90", "t-r"),
        ("38;2;255;107;128", "t-r"),
        ("38;2;230;219;116", "t-a"),
        ("38;2;215;119;87", "t-a"),
        ("38;2;255;193;7", "t-a"),
        ("38;2;102;217;239", "t-c"),
        ("38;2;0;204;204", "t-c"),
        ("38;2;188;140;255", "t-p"),
        ("48;2;61;1;0", "bg-r"),
        ("48;2;2;40;0", "bg-g"),
        ("48;2;92;2;0", "bg-r2"),
        ("48;2;4;71;0", "bg-g2"),
        ("31", "t-r"),
        ("92", "t-g"),
        ("90", "t-m"),
        ("38;5;196", "t-r"),
        ("38;5;244", "t-m"),
        ("1;38;2;220;90;90", "t-r t-b"),
    ];
    let js = format!(
        "console.log(JSON.stringify({}.map(c => runs(`\\x1b[${{c}}mx\\x1b[0m y`))))",
        serde_json::to_string(&cases.map(|c| c.0)).unwrap()
    );
    let Some(out) = node(&js) else { return };
    let got: Vec<Vec<(String, String)>> = serde_json::from_str(&out).unwrap();
    for ((code, want), g) in cases.iter().zip(got) {
        assert_eq!(g[0].1, *want, "SGR {code}");
        assert_eq!(g.last().unwrap().1, "", "SGR {code}: 0 resets");
    }
    assert!(!out.contains('#'), "{out}");
}

/// Assistant prose: escaped text plus spans only; `<script>` stays text. Bold, `code`, lists, headings.
#[test]
fn markdown_is_escaped_text_plus_spans() {
    let js = r#"console.log(prose(['⏺ # Plan <script>alert(1)</script>', '  Fix **the bug** in `src/a.rs` & <b>x</b>,', '  wrapped.',
      '', '  - one', '  - two `<i>`', '', '  1. first', '  2. second', '', '  then \x1b[38;2;177;185;249mcargo test\x1b[0m runs']))"#;
    let Some(out) = node(js) else { return };
    assert_eq!(
        out.trim(),
        "<p class=h>Plan &#60;script&#62;alert(1)&#60;/script&#62;</p><p>Fix <b>the bug</b> in <code>src/a.rs</code> &#38; \
         &#60;b&#62;x&#60;/b&#62;, wrapped.</p><ul><li>one</li><li>two <code>&#60;i&#62;</code></li></ul>\
         <ol><li>first</li><li>second</li></ol><p>then <code>cargo test</code> runs</p>"
    );
}

/// Role runs and their blocks from real screens: Claude Code in ANSI (#139's lane) and dsh-TUI.
#[test]
fn screens_group_into_role_blocks() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/screens");
    for (name, harness) in [("claude-ansi", "claude"), ("dsh-working", "dsh")] {
        let screen = std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap();
        let js = format!(
            "console.log(JSON.stringify(blocks({}, '{harness}', true).map(b => [b.k, b.html]), null, 1))",
            serde_json::to_string(&body(&screen)).unwrap()
        );
        let Some(out) = node(&js) else { return };
        let golden = dir.join(format!("{name}.blocks.json"));
        if std::env::var_os("BLESS").is_some() {
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}: blocks changed");
        let kinds: Vec<(String, String)> = serde_json::from_str(&out).unwrap();
        let kinds: Vec<&str> = kinds.iter().map(|k| k.0.as_str()).collect();
        let want: &[&str] = if harness == "claude" {
            &["ai", "ai", "ai", "run", "qs", "ai", "qs", "ai", "qs", "ai"]
        } else {
            &["ai", "me", "ai", "run", "ai"]
        };
        assert_eq!(kinds, want, "{name}");
    }
}

/// A tool run's header is its call; a red `⏺` with `Exit code N` tags `exit N`; over 20 lines the output keeps its
/// head (success) or its head and tail around `⋯ N lines` (failure); a last run with no output while busy is `running`.
#[test]
fn tool_runs_tag_cap_and_keep_the_failure() {
    let js = r#"
const out = n => Array.from({ length: n }, (_, i) => (i ? '     ' : '  ⎿  ') + 'line ' + i);
const ok = blocks(['⏺ Bash(cargo test)', ...out(29)], 'claude', false)[0].html;
const bad = blocks(['\x1b[38;2;255;107;128m⏺\x1b[0m Bash(cargo test)', '  ⎿  Error: Exit code 101', ...out(28).map(l => l.replace('⎿', ' '))], 'claude', false)[0].html;
const run = blocks(['⏺ Read(src/a.rs)'], 'claude', true)[0].html;
const diff = blocks(['⏺ Update(src/a.rs)', '  ⎿  Updated src/a.rs', '      52 \x1b[48;2;61;1;0m-        .filter(|s| s.state)\x1b[0m'], 'claude', false)[0].html;
console.log(JSON.stringify([ok, bad, run, diff]));
"#;
    let Some(out) = node(js) else { return };
    let [ok, bad, run, diff]: [String; 4] = serde_json::from_str(&out).unwrap();
    assert!(
        ok.starts_with(
            "<details class=\"blk run\"><summary><span class=k>$</span><span class=cmd>cargo test</span><i class=cv>"
        ),
        "{ok}"
    );
    assert!(
        ok.contains("line 19</span><span class=rest>") && !ok.contains("cut") && ok.contains("20 of 29 lines"),
        "{ok}"
    );
    assert!(bad.contains("<span class=t-r>exit 101</span>"), "{bad}");
    assert!(
        bad.contains("line 8</span><span class=rest>")
            && bad.contains("⋯ 9 lines</span><span class=\"ol\" style=\"--i:0\">line 18"),
        "{bad}"
    );
    assert!(
        run.contains("<span class=k>Read</span><span class=cmd>src/a.rs</span><span class=t-a>running</span>"),
        "{run}"
    );
    assert!(diff.contains("<span class=\"ol bg-r\" style=\"--i:13\">"), "{diff}");
}
