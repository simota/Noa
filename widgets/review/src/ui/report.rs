//! The レポート tab: the Markdown report as it stands, drawn as text
//! (headings larger, list items as lines).

use noa_gpui_core::widgets::Label;
use noa_gpui_core::{Role, UiTree, WidgetId};

use review::model::export;
use review::model::types::Session;

use super::{keep_scroll, scroll_column};

/// One report line as shown, as Markdown renders its inline marks: a code
/// span (a run of backticks closed by a run of the same length) loses its
/// backticks and keeps its text as written; outside code, `**` pairs that
/// open before text and close after it are dropped, across code spans too.
/// A mark that pairs with nothing is text, and stays.
fn plain(line: &str) -> String {
    // Each char, and whether it is code (never a mark).
    let mut chars: Vec<(char, bool)> = Vec::with_capacity(line.len());
    let src: Vec<char> = line.chars().collect();
    let run = |at: usize| src[at..].iter().take_while(|c| **c == '`').count();
    let mut i = 0;
    while i < src.len() {
        if src[i] != '`' {
            chars.push((src[i], false));
            i += 1;
            continue;
        }
        let n = run(i);
        let mut j = i + n;
        let close = loop {
            match src[j..].iter().position(|c| *c == '`') {
                None => break None,
                Some(k) => {
                    let at = j + k;
                    let m = run(at);
                    if m == n {
                        break Some(at);
                    }
                    j = at + m;
                }
            }
        };
        match close {
            Some(end) => {
                let mut code = &src[i + n..end];
                if code.len() > 2
                    && code[0] == ' '
                    && code[code.len() - 1] == ' '
                    && code.iter().any(|c| *c != ' ')
                {
                    code = &code[1..code.len() - 1];
                }
                chars.extend(code.iter().map(|c| (*c, true)));
                i = end + n;
            }
            None => {
                chars.extend(src[i..i + n].iter().map(|c| (*c, false)));
                i += n;
            }
        }
    }
    // `**` delimiters outside code: an opener has text after it, a closer
    // text before it; a closer pairs with the nearest open opener.
    let mut dropped = vec![false; chars.len()];
    let mut openers: Vec<usize> = Vec::new();
    let mut k = 0;
    while k + 1 < chars.len() {
        if chars[k] != ('*', false) || chars[k + 1] != ('*', false) {
            k += 1;
            continue;
        }
        let before = k.checked_sub(1).map(|b| chars[b].0);
        let after = chars.get(k + 2).map(|c| c.0);
        let can_close = before.is_some_and(|c| !c.is_whitespace());
        let can_open = after.is_some_and(|c| !c.is_whitespace());
        if can_close && let Some(open) = openers.pop() {
            for at in [open, open + 1, k, k + 1] {
                dropped[at] = true;
            }
        } else if can_open {
            openers.push(k);
        }
        k += 2;
    }
    chars
        .iter()
        .zip(dropped)
        .filter(|(_, gone)| !gone)
        .map(|((c, _), _)| *c)
        .collect()
}

pub(super) fn insert(tree: &mut UiTree, host: WidgetId, session: &Session) {
    let list = scroll_column(tree, host, 4.0);
    keep_scroll(tree, list, session, "report");
    let md = export::report_markdown(session);
    for line in md.lines() {
        let label = if let Some(rest) = line.strip_prefix("# ") {
            Label::new(plain(rest)).font_size(17.0).weight(700)
        } else if let Some(rest) = line.strip_prefix("## ") {
            // A little air above each section.
            tree.insert(
                Some(list),
                noa_gpui_core::widgets::Container::column().padding(4.0),
            );
            Label::new(plain(rest)).font_size(15.0).weight(600)
        } else if let Some(rest) = line.strip_prefix("### ") {
            Label::new(plain(rest))
                .font_size(13.0)
                .weight(600)
                .wrap(true)
        } else if line.trim().is_empty() {
            continue;
        } else if line.starts_with("  ") {
            Label::new(plain(line.trim_start()))
                .font_size(12.0)
                .color(Role::Muted)
                .wrap(true)
        } else {
            Label::new(plain(line)).font_size(12.5).wrap(true)
        };
        tree.insert(Some(list), label.selectable(true));
    }
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn the_report_view_drops_only_the_marks_markdown_renders() {
        // (input, as shown) for each form of inline mark.
        let cases = [
            // Marks the report itself writes.
            ("- **F2** [致命的] 依頼文", "- F2 [致命的] 依頼文"),
            ("- 場所: `src/a.rs:12`", "- 場所: src/a.rs:12"),
            // Code keeps its symbols.
            ("- 根拠: `a ** b` で再現", "- 根拠: a ** b で再現"),
            ("- 根拠: `x` と `**`", "- 根拠: x と **"),
            ("**F1** は `x**y` を見る", "F1 は x**y を見る"),
            // A code span opened by two backticks holds a single one.
            ("``a`b``", "a`b"),
            ("`` `x` ``", "`x`"),
            // Emphasis around code.
            ("**before `x` after**", "before x after"),
            // A mark without a partner, or that cannot open or close, is text.
            ("- 根拠: 2 ** 3 を返す", "- 根拠: 2 ** 3 を返す"),
            ("2 ** 3 と 4 ** 5", "2 ** 3 と 4 ** 5"),
            ("- 根拠: a`b", "- 根拠: a`b"),
            ("``a`", "``a`"),
        ];
        for (input, shown) in cases {
            assert_eq!(plain(input), shown, "{input}");
        }
    }
}
