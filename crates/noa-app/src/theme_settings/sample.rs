use noa_core::Rgb;

/// A swatch shown in the sample pane (R-5): the 16 ANSI palette entries,
/// fg/bg/cursor/selection, and one fixed truecolor sample — all derived from
/// a `ThemeDef`, never hand-authored (AC-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Swatch {
    /// One of the 16 base ANSI palette slots (0..16) and its resolved color.
    Ansi(u8, Rgb),
    Foreground(Rgb),
    Background(Rgb),
    Cursor(Rgb),
    Selection(Rgb),
    /// A fixed truecolor sample outside the 16-slot palette — proves the
    /// pane isn't limited to indexed color (R-5's "truecolor見本").
    Truecolor(Rgb),
}

/// The sample-pane swatch list for `theme` (AC-3): 16 ANSI + 4 semantic +
/// 1 truecolor, always in this fixed order.
pub(crate) fn sample_swatches(theme: &noa_theme::ThemeDef) -> Vec<Swatch> {
    let mut swatches = Vec::with_capacity(16 + 4 + 1);
    for (index, color) in theme.palette.iter().take(16).enumerate() {
        swatches.push(Swatch::Ansi(index as u8, *color));
    }
    swatches.push(Swatch::Foreground(theme.default_fg));
    swatches.push(Swatch::Background(theme.default_bg));
    swatches.push(Swatch::Cursor(theme.cursor));
    swatches.push(Swatch::Selection(theme.selection_bg));
    swatches.push(Swatch::Truecolor(Rgb::new(0x40, 0x80, 0xc0)));
    swatches
}
