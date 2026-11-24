//! The レポート tab: the Markdown report as it stands, drawn as text
//! (headings larger, list items as lines).

use noa_gpui_core::widgets::Label;
use noa_gpui_core::{Role, UiTree, WidgetId};

use review::model::export;
use review::model::types::Session;

use super::{keep_scroll, scroll_column};

/// One report line as shown: emphasis and code marks dropped.
fn plain(line: &str) -> String {
    line.replace("**", "").replace('`', "")
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
