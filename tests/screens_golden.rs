//! #105: harness-neutral screen parsing, golden-tested on real captured screens (paths masked) from Claude Code,
//! dsh-TUI and Hermes: no divider, input line, status bar, spinner or tip in `turns()`; the spinner (or status bar)
//! is the status line; the transcript's messages and tool rows are kept. `BLESS=1` rewrites the `.turns.json` files.
use herdsman::lanes::{activity, chat, tail, turns};
use std::path::Path;

/// (fixture, harness, status line, text that must not be chat, text that must stay).
type Screen = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

const SCREENS: [Screen; 7] = [
    (
        "claude-working",
        "claude",
        "✶ Pontificating… (12m 0s · ↓ 3.1k tokens)",
        &[
            "Pontificating",
            "herdsman · rev #97 r1",
            "bypass permissions",
            "esc to interrupt",
        ],
        &[
            "❯ Also read /Users/me/",
            "⏺ Running cd /Users/me/.herdr/worktrees/herdsman/rev-… · 16s",
            "(ctrl+b to run in background)",
        ],
    ),
    (
        "claude-tip",
        "claude",
        "✻ Deliberating… (1m 40s · ↓ 3.1k tokens)",
        &[
            "Deliberating",
            "Tip:",
            "Update installed",
            "bypass permissions",
            "2 shells · esc",
        ],
        &[
            "❯ #2726 pushed fix 2b71a5f",
            "✻ Brewed for 1m 40s",
            "⏺ Brief and spawn the #2726 round 2 reviewer",
        ],
    ),
    (
        "claude-idle",
        "claude",
        "✻ Churned for 2m 52s · done 1:01 AM",
        &[
            "Churned",
            "herdsman · rev #59 r1",
            "herdsman verdict herdsman 59 FAIL",
            "bypass permissions",
        ],
        &["Not verified: a real phone", "Checked and holding:"],
    ),
    (
        "dsh-working",
        "dsh",
        "🌓 Reticulating splines · total 22s · ↑ 6.7k · ↓ 9 tokens",
        &[
            "Reticulating",
            "⌸ ❯",
            "deepseek-flash · max · cache",
            "esc to interrupt",
        ],
        &[
            "❯ Run the shell command",
            "• Bash(sleep 8; ls) · 10s",
            "⠦ Thinking…",
            "│ The output is",
        ],
    ),
    (
        "dsh-idle",
        "dsh",
        "",
        &["⌸ ❯", "deepseek-flash · max · cache", "7.1k/1.0M"],
        &[
            "⚓ Thinking · 5s",
            "• Bash(ls -la) · 0s",
            "⏺ The directory contains a single entry",
        ],
    ),
    (
        "hermes-working",
        "hermes",
        "☤ glm-5.3-flash │ 17.7K/1M │ [░░░░░░░░░░] 2% │ ◷ 7.3s │ ↑ 7 t/s │ 46s │ ⏱ 14s",
        &["glm-5.3-flash │", "msg=interrupt", "Ctrl+C cancel"],
        &[
            "● Run the shell command",
            "┊ 💻 preparing terminal…",
            "💻 sleep 8 + 1 command",
        ],
    ),
    (
        "hermes-idle",
        "hermes",
        "☤ glm-5.3-flash │ ~19.9K/1M │ [░░░░░░░░░░] ~2% │ ◎ 46.7% │ ◷ 6.0s │ ↑ 14 t/s │ 59s │ ⏲ 27s │ ✓ 0s",
        &["glm-5.3-flash │", "Set a reminder or schedule", "☤ Hermes ───"],
        &[
            "┊ 💻 $         sleep 8 + 1 command  8.9s",
            "Your home directory holds mostly macOS folders",
        ],
    ),
];

#[test]
fn real_screens_keep_the_transcript_and_lift_the_frame_and_spinner() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/screens");
    for (name, harness, want, gone, kept) in SCREENS {
        let screen = std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap();
        let (body, status) = chat(&screen);
        assert_eq!(status.unwrap_or_default(), want, "{name}: status line");
        let t = turns(&tail(&body.join("\n"), 2000));
        let text = serde_json::to_string_pretty(&t).unwrap();
        let rule = text.split(|c| !"─━═╌┄▔▁".contains(c)).any(|r| r.chars().count() >= 20);
        assert!(!rule, "{name}: a divider is chat\n{text}");
        for g in gone {
            assert!(!text.contains(g), "{name}: `{g}` is chat\n{text}");
        }
        for k in kept {
            assert!(text.contains(k), "{name}: `{k}` is missing\n{text}");
        }
        if !want.is_empty() {
            let a = activity(harness, &screen);
            assert!(want.starts_with(a.trim_end_matches('…')), "{name}: activity `{a}`");
        }
        let golden = dir.join(format!("{name}.turns.json"));
        if std::env::var_os("BLESS").is_some() {
            std::fs::write(&golden, text + "\n").unwrap();
        } else {
            assert_eq!(
                text + "\n",
                std::fs::read_to_string(golden).unwrap(),
                "{name}: turns changed"
            );
        }
    }
}
