//! The レポート tab: the Markdown report as it stands, drawn as text
//! (headings larger, list items as lines).

use noa_gpui_core::widgets::Label;
use noa_gpui_core::{Role, UiTree, WidgetId};

use review::model::export;
use review::model::types::Session;

use super::{keep_scroll, scroll_column};

/// One report line as shown, as Markdown renders its inline marks: a code
/// span's backticks are dropped and its text kept as written (a `**` in it
/// stays); outside code, `**` around text is dropped. A mark without a
/// partner is text, and stays.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let Some(len) = rest[open + 1..].find('`') else {
            break;
        };
        out.push_str(&unbold(&rest[..open]));
        out.push_str(&rest[open + 1..open + 1 + len]);
        rest = &rest[open + 2 + len..];
    }
    out.push_str(&unbold(rest));
    out
}

/// `text` with each pair of `**` around a run dropped.
fn unbold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("**") {
        let Some(len) = rest[open + 2..].find("**") else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 2..open + 2 + len]);
        rest = &rest[open + 4 + len..];
    }
    out.push_str(rest);
    out
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
        // Marks the report itself writes.
        assert_eq!(plain("- **F2** [致命的] 依頼文"), "- F2 [致命的] 依頼文");
        assert_eq!(plain("- 場所: `src/a.rs:12`"), "- 場所: src/a.rs:12");
        // Code quoted in evidence keeps its symbols.
        assert_eq!(plain("- 根拠: `a ** b` で再現"), "- 根拠: a ** b で再現");
        assert_eq!(plain("- 根拠: `x` と `**`"), "- 根拠: x と **");
        // A mark without a partner is text.
        assert_eq!(plain("- 根拠: 2 ** 3 を返す"), "- 根拠: 2 ** 3 を返す");
        assert_eq!(plain("- 根拠: a`b"), "- 根拠: a`b");
        assert_eq!(plain("**F1** は `x**y` を見る"), "F1 は x**y を見る");
    }
}
