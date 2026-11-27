//! Fit single-line sidebar text to terminal cells before adding SGR colors.

use std::borrow::Cow;
use std::fmt::Write as _;

use noa_core::Rgb;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn width(text: &str) -> usize {
    text.graphemes(true)
        .map(|grapheme| {
            // Match Screen::cluster_wants_wide: a keycap without VS16 still
            // occupies two cells, although unicode-width reports only one.
            if grapheme.contains('\u{20e3}')
                && matches!(grapheme.chars().next(), Some('#' | '*' | '0'..='9'))
            {
                2
            } else {
                UnicodeWidthStr::width(grapheme)
            }
        })
        .sum()
}

fn single_line(text: &str) -> Cow<'_, str> {
    if text.chars().any(char::is_control) {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .into()
    } else {
        text.into()
    }
}

fn prefix(text: &str, cols: usize) -> &str {
    let mut used = 0;
    let mut end = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let next = used + width(grapheme);
        if next > cols {
            break;
        }
        used = next;
        end = offset + grapheme.len();
    }
    &text[..end]
}

fn suffix(text: &str, cols: usize) -> &str {
    let mut used = 0;
    let mut start = text.len();
    for (offset, grapheme) in text.grapheme_indices(true).rev() {
        let next = used + width(grapheme);
        if next > cols {
            break;
        }
        used = next;
        start = offset;
    }
    &text[start..]
}

pub(super) fn elide_end(text: &str, cols: usize) -> String {
    let text = single_line(text);
    if cols == 0 {
        String::new()
    } else if width(&text) <= cols {
        text.into_owned()
    } else {
        format!("{}…", prefix(&text, cols - 1))
    }
}

/// Inline rename keeps the insertion point and the end of the user's input.
pub(super) fn elide_start(text: &str, cols: usize) -> String {
    let text = single_line(text);
    if cols == 0 {
        String::new()
    } else if width(&text) <= cols {
        text.into_owned()
    } else {
        format!("…{}", suffix(&text, cols - 1))
    }
}

/// Paths and branches keep their identifying tail as well as a short root.
pub(super) fn elide_middle(text: &str, cols: usize) -> String {
    let text = single_line(text);
    if cols == 0 {
        return String::new();
    }
    if width(&text) <= cols {
        return text.into_owned();
    }
    let head = prefix(&text, (cols - 1) / 3);
    let tail = suffix(&text, cols - 1 - width(head));
    format!("{head}…{tail}")
}

/// Segment the joined preview text so color boundaries cannot split an emoji
/// or combining sequence, and escape bytes never consume the width budget.
pub(super) fn colored_prefix(spans: &[(&str, Rgb)], cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    let mut plain = String::new();
    let mut colors = Vec::new();
    for &(text, color) in spans {
        let text = single_line(text);
        if !text.is_empty() {
            colors.push((plain.len(), color));
            plain.push_str(&text);
        }
    }
    let clipped = width(&plain) > cols;
    let visible = prefix(&plain, cols - usize::from(clipped));
    let mut output = String::new();
    let mut span_index = 0;
    let mut last_color = None;
    for (offset, grapheme) in visible.grapheme_indices(true) {
        while span_index + 1 < colors.len() && colors[span_index + 1].0 <= offset {
            span_index += 1;
        }
        let color = colors[span_index].1;
        if last_color != Some(color) {
            let _ = write!(output, "\x1b[38;2;{};{};{}m", color.r, color.g, color.b);
            last_color = Some(color);
        }
        output.push_str(grapheme);
    }
    if clipped {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elision_fits_cells_and_preserves_graphemes() {
        for text in [
            "long session name",
            "日本語の作業",
            "e\u{301}e\u{301}e\u{301}",
            "👩‍💻🇯🇵👍🏽",
        ] {
            for cols in 0..=20 {
                for fitted in [
                    elide_end(text, cols),
                    elide_start(text, cols),
                    elide_middle(text, cols),
                ] {
                    assert!(width(&fitted) <= cols, "{text:?} in {cols}: {fitted:?}");
                    if width(text) <= cols {
                        assert_eq!(fitted, text);
                    }
                }
            }
        }
        assert_eq!(elide_end("日本語", 4), "日…");
        assert_eq!(elide_end("e\u{301}e\u{301}e\u{301}", 2), "e\u{301}…");
        assert_eq!(elide_end("👩‍💻 task", 4), "👩‍💻 …");
        assert_eq!(elide_start("長い名前▏", 5), "…前▏");
    }

    #[test]
    fn path_elision_keeps_root_and_identifying_tail() {
        assert_eq!(elide_middle("~/work/company/Noa", 12), "~/w…pany/Noa");
        assert_eq!(elide_middle("feature/agent-sidebar", 15), "feat…nt-sidebar");
    }

    #[test]
    fn text_cannot_move_the_sidebar_cursor() {
        assert_eq!(elide_end("line\nnext\t\x1b", 30), "line next  ");
    }

    #[test]
    fn preview_colors_do_not_take_columns_or_split_clusters() {
        let red = Rgb::new(255, 0, 0);
        let blue = Rgb::new(0, 0, 255);
        assert_eq!(
            colored_prefix(&[("日", red), ("本語", blue)], 4),
            "\x1b[38;2;255;0;0m日…"
        );
        assert_eq!(
            colored_prefix(&[("e", red), ("\u{301}OK", blue)], 3),
            "\x1b[38;2;255;0;0me\u{301}\x1b[38;2;0;0;255mOK"
        );
        assert_eq!(colored_prefix(&[("text", red)], 0), "");
    }

    #[test]
    fn keycaps_without_variation_selectors_match_the_terminal_grid() {
        for text in [
            "1\u{20e3}",
            "*\u{20e3}",
            "#\u{20e3}",
            "1\u{20e3}\u{301}",
            "1\u{20e3}\u{fe0f}",
        ] {
            assert_eq!(width(text), 2);
            assert_eq!(elide_end(&format!("{text}x"), 2), "…");
            let mut term = noa_grid::Terminal::new(noa_core::GridSize { cols: 10, rows: 1 });
            noa_vt::Stream::new().feed(text.as_bytes(), &mut term);
            assert_eq!(term.active().cursor.x as usize, width(text));
        }
    }
}
