//! An agent's screen split at its input box (AU-11), as the board splits it
//! (`board::content_lines`): the work above it, and the bottom block (the
//! input box, or a dialog drawn in its place) that a wait line on a
//! `permission` or `input` shows as `"prompt"`.

use super::board::{content_lines, is_rule};

/// The most lines a `"prompt"` keeps.
pub(crate) const PROMPT_LINES: usize = super::PROMPT_LINES;

/// A screen's (or retained output's) lines, as the board reads them: ends
/// trimmed, and without the blank rows and Noa's hold banner at the bottom,
/// so the input box is found near the end.
pub(crate) fn lines_of(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();
    while lines
        .last()
        .is_some_and(|line| line.is_empty() || line.starts_with(super::HOLD_BANNER))
    {
        lines.pop();
    }
    lines
}

/// The bottom block of an agent's screen, below the work: rules and blank
/// lines removed, at most [`PROMPT_LINES`] lines. `None` when the screen has
/// no input box (nothing is cut off) or the block is empty.
pub(crate) fn block(screen: &str) -> Option<String> {
    let lines = lines_of(screen);
    let work = content_lines(&lines).len();
    let kept: Vec<&str> = lines[work..]
        .iter()
        .map(String::as_str)
        .filter(|line| !line.trim().is_empty() && !is_rule(line))
        .take(PROMPT_LINES)
        .collect();
    (!kept.is_empty()).then(|| kept.join("\n"))
}

/// Whether the bottom of a screen shows an agent's input box.
pub(crate) fn has_input_box(screen: &str) -> bool {
    let lines = lines_of(screen);
    content_lines(&lines).len() < lines.len()
}

/// Whether the bottom of a screen shows Codex's input line: Codex (0.171)
/// draws no rules around it, only `› ` and its placeholder or draft, with
/// a status line or two below. A numbered `› 1. …` is a selected choice
/// of a dialog, not the input line.
pub(crate) fn has_codex_input_line(screen: &str) -> bool {
    const REACH: usize = 4;
    lines_of(screen)
        .iter()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(REACH)
        .filter_map(|line| line.trim_start().strip_prefix('›'))
        .any(|rest| {
            let rest = rest.trim_start();
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            !(digits > 0 && rest[digits..].starts_with('.'))
        })
}

/// The work above an agent's input box, as one text (what `--settle`
/// compares: the box and the status line below it can tick on their own).
pub(crate) fn work(screen: &str) -> String {
    let lines = lines_of(screen);
    content_lines(&lines).join("\n")
}

/// `--tail` of an agent's pane: the last `count` lines above its input box.
pub(crate) fn agent_tail(text: &str, count: usize) -> String {
    let lines = lines_of(text);
    let work = content_lines(&lines);
    let mut end = work.len();
    while end > 0 && work[end - 1].trim().is_empty() {
        end -= 1;
    }
    work[end.saturating_sub(count)..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────";

    fn screen(work: &[&str], bottom: &[&str]) -> String {
        let mut lines: Vec<String> = work.iter().map(|line| line.to_string()).collect();
        lines.extend(bottom.iter().map(|line| line.to_string()));
        lines.extend(["".to_string(), "   ".to_string()]);
        lines.join("\n")
    }

    #[test]
    fn the_block_below_the_work_is_the_prompt_without_rules() {
        let text = screen(
            &["● Edited src/a.rs", "  done"],
            &[
                RULE,
                " Do you want to make this edit?",
                " ❯ 1. Yes",
                "   2. No",
                RULE,
                "  ? for shortcuts",
            ],
        );
        assert_eq!(
            block(&text).as_deref(),
            Some(" Do you want to make this edit?\n ❯ 1. Yes\n   2. No\n  ? for shortcuts")
        );
        assert!(has_input_box(&text));
        assert_eq!(work(&text), "● Edited src/a.rs\n  done");
        // No input box: nothing is cut off, so there is no prompt.
        let plain = screen(&["$ make", "ok"], &[]);
        assert_eq!(block(&plain), None);
        assert!(!has_input_box(&plain));
        assert_eq!(work(&plain), "$ make\nok");
    }

    #[test]
    fn a_long_block_keeps_its_first_lines() {
        let mut bottom = vec![RULE];
        let many: Vec<String> = (0..11).map(|n| format!("choice {n}")).collect();
        bottom.extend(many.iter().map(String::as_str));
        let text = screen(&["work"], &bottom);
        let prompt = block(&text).unwrap();
        assert!(prompt.starts_with("choice 0"));
        assert!(prompt.lines().count() <= PROMPT_LINES);
    }

    #[test]
    fn an_agents_tail_is_taken_from_above_its_input_box() {
        let text = screen(
            &["one", "two", "three", ""],
            &[RULE, " > ", RULE, "  ctx 40%"],
        );
        assert_eq!(agent_tail(&text, 2), "two\nthree");
        assert_eq!(agent_tail(&text, 10), "one\ntwo\nthree");
        assert_eq!(agent_tail("a\nb\n", 1), "b");
    }
}
