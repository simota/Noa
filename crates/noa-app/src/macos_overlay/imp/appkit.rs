use crate::macos_overlay::model::{
    OverlayColors, PaneRectPt, ThemeSettingsViewModel, Tone, overlay_scroll_window,
};
use noa_render::{CommandPaletteSnapshot, ConfirmDialogSnapshot, PaletteRow};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize, NSString};
use winit::window::Window;

// NSVisualEffectView constants (ABI-stable enum values).
const MATERIAL_POPOVER: isize = 6;
const MATERIAL_HUD_WINDOW: isize = 13;
const BLENDING_WITHIN_WINDOW: isize = 1;
const STATE_ACTIVE: isize = 1;
// NSTextAlignment (macOS values: right is 1, center is 2).
const ALIGN_RIGHT: isize = 1;
const ALIGN_CENTER: isize = 2;
// NSLineBreakMode.byTruncatingTail.
const TRUNCATE_TAIL: usize = 4;
// NSFontWeight (CGFloat constants from NSFontDescriptor.h).
const WEIGHT_REGULAR: f64 = 0.0;
const WEIGHT_MEDIUM: f64 = 0.23;
const WEIGHT_SEMIBOLD: f64 = 0.3;

const ID_PALETTE: &str = "noa.native-overlay.palette";
const ID_THEME: &str = "noa.native-overlay.theme-settings";
const ID_CONFIRM: &str = "noa.native-overlay.confirm";
const ID_TITLE_PROMPT: &str = "noa.native-overlay.title-prompt";
const ID_TOAST: &str = "noa.native-overlay.toast";

/// Palette metrics (points).
const PALETTE_WIDTH: f64 = 560.0;
const QUERY_ROW_H: f64 = 44.0;
const ENTRY_ROW_H: f64 = 26.0;
const HEADER_ROW_H: f64 = 24.0;
const LIST_PAD_V: f64 = 6.0;
const CARD_PAD_H: f64 = 16.0;
const CARD_RADIUS: f64 = 12.0;
/// Max list rows (headers + entries) visible at once — matches the wgpu
/// card's 12-row window.
const PALETTE_CAPACITY: usize = 12;
const SCRIM_ALPHA: f64 = 0.25;

/// Balance a `+1` (alloc/init) object that a superview now retains:
/// adopting it into a dropped [`Retained`] performs the release the
/// `msg_send!` macro (correctly) refuses to express.
unsafe fn release_owned(obj: *mut AnyObject) {
    if !obj.is_null() {
        let _ = unsafe { Retained::from_raw(obj) };
    }
}

/// The winit content view (`NSView`) for `window`, or null.
fn content_view(window: &Window) -> *mut AnyObject {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else {
        return std::ptr::null_mut();
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return std::ptr::null_mut();
    };
    appkit.ns_view.as_ptr().cast::<AnyObject>()
}

/// Remove the subview of `view` carrying `identifier`, if present.
///
/// SAFETY (all helpers below): `view` is winit's live `NSView` and every
/// call happens on the main thread (the redraw path); every selector is
/// documented AppKit API and every object pointer is nil-checked.
unsafe fn remove_subview(view: *mut AnyObject, identifier: &str) {
    let identifier = NSString::from_str(identifier);
    unsafe {
        let subviews: *mut AnyObject = msg_send![view, subviews];
        if subviews.is_null() {
            return;
        }
        let count: usize = msg_send![subviews, count];
        for i in 0..count {
            let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            if subview.is_null() {
                continue;
            }
            let ident: *mut AnyObject = msg_send![subview, identifier];
            if !ident.is_null() {
                let same: bool = msg_send![ident, isEqualToString: &*identifier];
                if same {
                    let _: () = msg_send![subview, removeFromSuperview];
                    return;
                }
            }
        }
    }
}

/// `rect` (top-left-origin points) converted to `view`'s coordinate
/// space. winit's view is flipped (top-left origin), but query instead of
/// assuming.
unsafe fn frame_in_view(view: *mut AnyObject, rect: PaneRectPt) -> NSRect {
    let flipped: bool = unsafe { msg_send![view, isFlipped] };
    let bounds: NSRect = unsafe { msg_send![view, bounds] };
    let y = if flipped {
        rect.y
    } else {
        bounds.size.height - rect.y - rect.h
    };
    NSRect::new(NSPoint::new(rect.x, y), NSSize::new(rect.w, rect.h))
}

/// A y coordinate `top` points below the top edge of an unflipped parent
/// of height `parent_h`, for a child of height `h`.
fn from_top(parent_h: f64, top: f64, h: f64) -> f64 {
    parent_h - top - h
}

unsafe fn ns_color(rgba: [f32; 4], alpha_scale: f64) -> *mut AnyObject {
    let Some(class) = AnyClass::get(c"NSColor") else {
        return std::ptr::null_mut();
    };
    unsafe {
        msg_send![
            class,
            colorWithSRGBRed: rgba[0] as f64,
            green: rgba[1] as f64,
            blue: rgba[2] as f64,
            alpha: rgba[3] as f64 * alpha_scale,
        ]
    }
}

unsafe fn system_font(size: f64, weight: f64) -> *mut AnyObject {
    let Some(class) = AnyClass::get(c"NSFont") else {
        return std::ptr::null_mut();
    };
    unsafe { msg_send![class, systemFontOfSize: size, weight: weight] }
}

unsafe fn mono_digit_font(size: f64) -> *mut AnyObject {
    let Some(class) = AnyClass::get(c"NSFont") else {
        return std::ptr::null_mut();
    };
    unsafe { msg_send![class, monospacedDigitSystemFontOfSize: size, weight: WEIGHT_REGULAR] }
}

/// A plain layer-backed `NSView`.
unsafe fn make_view(frame: NSRect) -> *mut AnyObject {
    let Some(class) = AnyClass::get(c"NSView") else {
        return std::ptr::null_mut();
    };
    unsafe {
        let alloc: *mut AnyObject = msg_send![class, alloc];
        let view: *mut AnyObject = msg_send![alloc, initWithFrame: frame];
        if !view.is_null() {
            let _: () = msg_send![view, setWantsLayer: true];
        }
        view
    }
}

/// Tint `view`'s layer: background color, corner radius.
unsafe fn tint_layer(view: *mut AnyObject, color: *mut AnyObject, radius: f64) {
    unsafe {
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return;
        }
        if !color.is_null() {
            let cg: *mut crate::macos_overlay::cg::CGColor = msg_send![color, CGColor];
            let _: () = msg_send![layer, setBackgroundColor: cg];
        }
        let _: () = msg_send![layer, setCornerRadius: radius];
    }
}

/// The blur card: an `NSVisualEffectView` with rounded corners, hairline
/// border, and the vibrant appearance matching the theme polarity, inside
/// a shadow-casting host view. Returns `(host, effect_view)`.
unsafe fn make_card(
    frame: NSRect,
    material: isize,
    colors: &OverlayColors,
    radius: f64,
) -> (*mut AnyObject, *mut AnyObject) {
    unsafe {
        let host = make_view(frame);
        if host.is_null() {
            return (host, std::ptr::null_mut());
        }
        // The host casts the shadow; the effect view clips to the rounded
        // shape, so the shadow must live one level up.
        let layer: *mut AnyObject = msg_send![host, layer];
        if !layer.is_null() {
            let shadow_color = ns_color([0.0, 0.0, 0.0, 1.0], 1.0);
            let cg: *mut crate::macos_overlay::cg::CGColor = msg_send![shadow_color, CGColor];
            let _: () = msg_send![layer, setShadowColor: cg];
            let _: () = msg_send![layer, setShadowOpacity: 0.42_f32];
            let _: () = msg_send![layer, setShadowRadius: 22.0_f64];
            let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, -8.0)];
            let _: () = msg_send![layer, setMasksToBounds: false];
        }

        let Some(effect_class) = AnyClass::get(c"NSVisualEffectView") else {
            return (host, std::ptr::null_mut());
        };
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
        let alloc: *mut AnyObject = msg_send![effect_class, alloc];
        let effect: *mut AnyObject = msg_send![alloc, initWithFrame: bounds];
        if effect.is_null() {
            return (host, effect);
        }
        let _: () = msg_send![effect, setMaterial: material];
        let _: () = msg_send![effect, setBlendingMode: BLENDING_WITHIN_WINDOW];
        let _: () = msg_send![effect, setState: STATE_ACTIVE];
        let _: () = msg_send![effect, setWantsLayer: true];
        let effect_layer: *mut AnyObject = msg_send![effect, layer];
        if !effect_layer.is_null() {
            let _: () = msg_send![effect_layer, setCornerRadius: radius];
            let _: () = msg_send![effect_layer, setMasksToBounds: true];
            let _: () = msg_send![effect_layer, setBorderWidth: 1.0_f64];
            let border = ns_color(colors.border, 0.55);
            if !border.is_null() {
                let cg: *mut crate::macos_overlay::cg::CGColor = msg_send![border, CGColor];
                let _: () = msg_send![effect_layer, setBorderColor: cg];
            }
        }
        // Vibrant appearance following the theme's polarity, not the OS
        // setting, so a dark terminal theme keeps a dark card on a light
        // desktop (and vice versa).
        if let Some(appearance_class) = AnyClass::get(c"NSAppearance") {
            let name = NSString::from_str(if colors.is_dark() {
                "NSAppearanceNameVibrantDark"
            } else {
                "NSAppearanceNameVibrantLight"
            });
            let appearance: *mut AnyObject = msg_send![appearance_class, appearanceNamed: &*name];
            if !appearance.is_null() {
                let _: () = msg_send![effect, setAppearance: appearance];
            }
        }
        // A translucent wash of the theme surface over the blur, pulling
        // the material toward the terminal theme.
        let wash = make_view(bounds);
        if !wash.is_null() {
            tint_layer(wash, ns_color(colors.surface_bg, 0.55), radius);
            let _: () = msg_send![wash, setAutoresizingMask: (1usize << 1) | (1usize << 4)];
            let _: () = msg_send![effect, addSubview: wash];
            release_owned(wash);
        }
        let _: () = msg_send![host, addSubview: effect];
        release_owned(effect);
        (host, effect)
    }
}

/// A single-line, non-editable label.
unsafe fn make_label(
    text: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    frame: NSRect,
) -> *mut AnyObject {
    let Some(class) = AnyClass::get(c"NSTextField") else {
        return std::ptr::null_mut();
    };
    let string = NSString::from_str(text);
    unsafe {
        let label: *mut AnyObject = msg_send![class, labelWithString: &*string];
        if label.is_null() {
            return label;
        }
        if !font.is_null() {
            let _: () = msg_send![label, setFont: font];
        }
        if !color.is_null() {
            let _: () = msg_send![label, setTextColor: color];
        }
        let _: () = msg_send![label, setFrame: frame];
        let cell: *mut AnyObject = msg_send![label, cell];
        if !cell.is_null() {
            let _: () = msg_send![cell, setLineBreakMode: TRUNCATE_TAIL];
        }
        label
    }
}

unsafe fn set_alignment(label: *mut AnyObject, alignment: isize) {
    if !label.is_null() {
        unsafe {
            let _: () = msg_send![label, setAlignment: alignment];
        }
    }
}

/// A label whose title carries per-character color/bold runs (the
/// palette's query-match highlight). `runs` maps char indices to
/// emphasized (accent + semibold) rendering.
///
/// Attribute keys are the ABI-stable literal values of
/// `NSFontAttributeName` (`@"NSFont"`) and `NSForegroundColorAttributeName`
/// (`@"NSColor"`), avoiding a link-time dependency on the constants.
unsafe fn make_match_label(
    text: &str,
    positions: &[usize],
    size: f64,
    base_color: *mut AnyObject,
    accent_color: *mut AnyObject,
    frame: NSRect,
) -> *mut AnyObject {
    unsafe {
        let label = make_label(text, system_font(size, WEIGHT_REGULAR), base_color, frame);
        if label.is_null() || positions.is_empty() {
            return label;
        }
        let Some(attr_class) = AnyClass::get(c"NSMutableAttributedString") else {
            return label;
        };
        let string = NSString::from_str(text);
        let alloc: *mut AnyObject = msg_send![attr_class, alloc];
        let attr: *mut AnyObject = msg_send![alloc, initWithString: &*string];
        if attr.is_null() {
            return label;
        }
        let font_key = NSString::from_str("NSFont");
        let color_key = NSString::from_str("NSColor");
        let base_font = system_font(size, WEIGHT_REGULAR);
        let bold_font = system_font(size, WEIGHT_SEMIBOLD);
        let full = NSRange {
            location: 0,
            length: text.encode_utf16().count(),
        };
        if !base_font.is_null() {
            let _: () = msg_send![attr, addAttribute: &*font_key, value: base_font, range: full];
        }
        if !base_color.is_null() {
            let _: () = msg_send![attr, addAttribute: &*color_key, value: base_color, range: full];
        }
        // Char indices → UTF-16 ranges (NSString indexing).
        let mut utf16_offsets = Vec::with_capacity(text.chars().count() + 1);
        let mut acc = 0usize;
        for ch in text.chars() {
            utf16_offsets.push(acc);
            acc += ch.len_utf16();
        }
        utf16_offsets.push(acc);
        for &pos in positions {
            let (Some(&start), Some(&end)) = (utf16_offsets.get(pos), utf16_offsets.get(pos + 1))
            else {
                continue;
            };
            let range = NSRange {
                location: start,
                length: end - start,
            };
            if !bold_font.is_null() {
                let _: () =
                    msg_send![attr, addAttribute: &*font_key, value: bold_font, range: range];
            }
            if !accent_color.is_null() {
                let _: () =
                    msg_send![attr, addAttribute: &*color_key, value: accent_color, range: range];
            }
        }
        let _: () = msg_send![label, setAttributedStringValue: attr];
        release_owned(attr);
        label
    }
}

/// The full-pane modal root: scrim + identifier, added to the content
/// view. Returns `(root, pane_h_pt)`; children are laid out in the root's
/// unflipped coordinates via [`from_top`].
unsafe fn make_modal_root(
    view: *mut AnyObject,
    identifier: &str,
    pane: PaneRectPt,
    scrim: bool,
) -> *mut AnyObject {
    unsafe {
        let frame = frame_in_view(view, pane);
        let root = make_view(frame);
        if root.is_null() {
            return root;
        }
        let ident = NSString::from_str(identifier);
        let _: () = msg_send![root, setIdentifier: &*ident];
        if scrim {
            tint_layer(root, ns_color([0.0, 0.0, 0.0, 1.0], SCRIM_ALPHA), 0.0);
        }
        let _: () = msg_send![view, addSubview: root];
        release_owned(root);
        root
    }
}

// -----------------------------------------------------------------------
// command palette
// -----------------------------------------------------------------------

pub(in crate::macos_overlay) fn rebuild_palette(
    window: &Window,
    model: Option<(&CommandPaletteSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let view = content_view(window);
    if view.is_null() {
        return;
    }
    unsafe {
        remove_subview(view, ID_PALETTE);
        let Some((snap, pane)) = model else {
            return;
        };
        let root = make_modal_root(view, ID_PALETTE, pane, true);
        if root.is_null() {
            return;
        }

        // Window capacity bounded by the pane height so the list never
        // runs past the card's bottom edge on a short pane.
        let capacity = (((pane.h - 24.0 - QUERY_ROW_H - 1.0 - LIST_PAD_V * 2.0) / ENTRY_ROW_H)
            as usize)
            .clamp(3, PALETTE_CAPACITY);
        let (offset, shown) = overlay_scroll_window(snap.rows.len(), snap.selected, capacity);
        let visible = &snap.rows[offset..offset + shown];
        let empty = snap.rows.is_empty();
        let list_h: f64 = if empty {
            36.0
        } else {
            visible
                .iter()
                .map(|row| match row {
                    PaletteRow::Header { .. } => HEADER_ROW_H,
                    PaletteRow::Entry { .. } => ENTRY_ROW_H,
                })
                .sum::<f64>()
                + LIST_PAD_V * 2.0
        };
        let card_w = PALETTE_WIDTH.min(pane.w - 32.0).max(280.0);
        let card_h = (QUERY_ROW_H + 1.0 + list_h).min(pane.h - 24.0);
        let card_x = (pane.w - card_w) / 2.0;
        let card_y_top = (pane.h * 0.14).min(pane.h - card_h).max(8.0);
        let card_frame = NSRect::new(
            NSPoint::new(card_x, from_top(pane.h, card_y_top, card_h)),
            NSSize::new(card_w, card_h),
        );
        let (host, effect) = make_card(card_frame, MATERIAL_POPOVER, colors, CARD_RADIUS);
        if host.is_null() || effect.is_null() {
            return;
        }
        let _: () = msg_send![root, addSubview: host];
        release_owned(host);

        let fg = ns_color(colors.surface_fg, 1.0);
        let muted = ns_color(colors.muted, 1.0);
        let accent = ns_color(colors.accent, 1.0);

        // Query row: accent prompt, query text (or placeholder), accent
        // caret, right-aligned counter.
        let prompt = make_label(
            "\u{276f}",
            system_font(15.0, WEIGHT_SEMIBOLD),
            accent,
            NSRect::new(
                NSPoint::new(CARD_PAD_H, from_top(card_h, 13.0, 20.0)),
                NSSize::new(18.0, 20.0),
            ),
        );
        if !prompt.is_null() {
            let _: () = msg_send![effect, addSubview: prompt];
        }
        let shown_entries = visible
            .iter()
            .filter(|row| matches!(row, PaletteRow::Entry { .. }))
            .count();
        let counter = (shown_entries < snap.total_entries)
            .then(|| format!("{shown_entries}/{}", snap.total_entries));
        let counter_w = if counter.is_some() { 64.0 } else { 0.0 };
        let query_x = CARD_PAD_H + 22.0;
        let query_frame = NSRect::new(
            NSPoint::new(query_x, from_top(card_h, 13.0, 20.0)),
            NSSize::new(card_w - query_x - CARD_PAD_H - counter_w, 20.0),
        );
        let query_label = if snap.query.is_empty() {
            make_label(
                "Type a command\u{2026}",
                system_font(15.0, WEIGHT_REGULAR),
                muted,
                query_frame,
            )
        } else {
            // Trailing accent caret rides in the attributed string.
            let text = format!("{}\u{258f}", snap.query);
            let caret_pos = text.chars().count() - 1;
            make_match_label(&text, &[caret_pos], 15.0, fg, accent, query_frame)
        };
        if !query_label.is_null() {
            let _: () = msg_send![effect, addSubview: query_label];
        }
        if let Some(counter) = counter {
            let label = make_label(
                &counter,
                mono_digit_font(11.0),
                muted,
                NSRect::new(
                    NSPoint::new(
                        card_w - CARD_PAD_H - counter_w,
                        from_top(card_h, 16.0, 14.0),
                    ),
                    NSSize::new(counter_w, 14.0),
                ),
            );
            if !label.is_null() {
                set_alignment(label, ALIGN_RIGHT);
                let _: () = msg_send![effect, addSubview: label];
            }
        }
        // Hairline rule under the query row.
        let rule = make_view(NSRect::new(
            NSPoint::new(0.0, from_top(card_h, QUERY_ROW_H, 1.0)),
            NSSize::new(card_w, 1.0),
        ));
        if !rule.is_null() {
            tint_layer(rule, ns_color(colors.border, 0.5), 0.0);
            let _: () = msg_send![effect, addSubview: rule];
            release_owned(rule);
        }

        if empty {
            let label = make_label(
                "No matching commands",
                system_font(13.0, WEIGHT_REGULAR),
                muted,
                NSRect::new(
                    NSPoint::new(CARD_PAD_H, from_top(card_h, QUERY_ROW_H + 10.0, 18.0)),
                    NSSize::new(card_w - CARD_PAD_H * 2.0, 18.0),
                ),
            );
            if !label.is_null() {
                set_alignment(label, ALIGN_CENTER);
                let _: () = msg_send![effect, addSubview: label];
            }
            return;
        }

        let mut y_top = QUERY_ROW_H + 1.0 + LIST_PAD_V;
        for (i, row) in visible.iter().enumerate() {
            match row {
                PaletteRow::Header { label } => {
                    let header = make_label(
                        &label.to_uppercase(),
                        system_font(10.5, WEIGHT_SEMIBOLD),
                        muted,
                        NSRect::new(
                            NSPoint::new(
                                CARD_PAD_H,
                                from_top(card_h, y_top + HEADER_ROW_H - 16.0, 13.0),
                            ),
                            NSSize::new(card_w - CARD_PAD_H * 2.0, 13.0),
                        ),
                    );
                    if !header.is_null() {
                        let _: () = msg_send![effect, addSubview: header];
                    }
                    y_top += HEADER_ROW_H;
                }
                PaletteRow::Entry {
                    title,
                    hint,
                    match_positions,
                } => {
                    let selected = offset + i == snap.selected;
                    if selected {
                        let bg = make_view(NSRect::new(
                            NSPoint::new(8.0, from_top(card_h, y_top, ENTRY_ROW_H)),
                            NSSize::new(card_w - 16.0, ENTRY_ROW_H),
                        ));
                        if !bg.is_null() {
                            tint_layer(bg, ns_color(colors.selected_bg, 1.0), 6.0);
                            let _: () = msg_send![effect, addSubview: bg];
                            release_owned(bg);
                        }
                    }
                    let hint_w = if hint.is_some() { 110.0 } else { 0.0 };
                    let title_frame = NSRect::new(
                        NSPoint::new(CARD_PAD_H, from_top(card_h, y_top + 5.0, 17.0)),
                        NSSize::new(card_w - CARD_PAD_H * 2.0 - hint_w, 17.0),
                    );
                    let title_label =
                        make_match_label(title, match_positions, 13.0, fg, accent, title_frame);
                    if !title_label.is_null() {
                        let _: () = msg_send![effect, addSubview: title_label];
                    }
                    if let Some(hint) = hint {
                        let label = make_label(
                            hint,
                            system_font(12.0, WEIGHT_REGULAR),
                            muted,
                            NSRect::new(
                                NSPoint::new(
                                    card_w - CARD_PAD_H - hint_w,
                                    from_top(card_h, y_top + 6.0, 15.0),
                                ),
                                NSSize::new(hint_w, 15.0),
                            ),
                        );
                        if !label.is_null() {
                            set_alignment(label, ALIGN_RIGHT);
                            let _: () = msg_send![effect, addSubview: label];
                        }
                    }
                    y_top += ENTRY_ROW_H;
                }
            }
        }
    }
}

// -----------------------------------------------------------------------
// theme settings
// -----------------------------------------------------------------------

pub(in crate::macos_overlay) fn rebuild_theme_settings(
    window: &Window,
    model: Option<(ThemeSettingsViewModel, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let view = content_view(window);
    if view.is_null() {
        return;
    }
    unsafe {
        remove_subview(view, ID_THEME);
        let Some((vm, pane)) = model else {
            return;
        };
        let root = make_modal_root(view, ID_THEME, pane, true);
        if root.is_null() {
            return;
        }

        let card_w = 660.0_f64.min(pane.w - 32.0).max(320.0);
        // The card height is content-driven: title block + theme list +
        // settings section + footer. When the pane is short, first shrink
        // the theme list (min 3 rows), then window the settings rows
        // around the selection — the same degradation policy as the wgpu
        // card, so every settings row stays reachable.
        let pad = 20.0;
        let list_top = 86.0;
        let row_h = 24.0;
        let srow_h = 23.0;
        let settings_header_h = 20.0;
        let section_gap = 18.0;
        let footer_h = 34.0;
        let avail = (pane.h - 24.0).max(240.0);
        let settings_total = vm.rows.len();
        let needed = |list_rows: usize, settings_rows: usize| {
            list_top
                + list_rows as f64 * row_h
                + section_gap
                + settings_header_h
                + settings_rows as f64 * srow_h
                + footer_h
        };
        let mut list_rows = vm.themes.len().max(1);
        let mut settings_rows = settings_total;
        while needed(list_rows, settings_rows) > avail && list_rows > 3 {
            list_rows -= 1;
        }
        while needed(list_rows, settings_rows) > avail && settings_rows > 3 {
            settings_rows -= 1;
        }
        let card_h = needed(list_rows, settings_rows).min(avail);
        let card_frame = NSRect::new(
            NSPoint::new(
                (pane.w - card_w) / 2.0,
                from_top(pane.h, (pane.h - card_h) / 2.0, card_h),
            ),
            NSSize::new(card_w, card_h),
        );
        let (host, effect) = make_card(card_frame, MATERIAL_POPOVER, colors, CARD_RADIUS);
        if host.is_null() || effect.is_null() {
            return;
        }
        let _: () = msg_send![root, addSubview: host];
        release_owned(host);

        let fg = ns_color(colors.surface_fg, 1.0);
        let muted = ns_color(colors.muted, 1.0);
        let accent = ns_color(colors.accent, 1.0);
        let danger = ns_color(colors.danger, 1.0);

        // Title row + save badge.
        let title = make_label(
            "Theme & Settings",
            system_font(15.0, WEIGHT_SEMIBOLD),
            fg,
            NSRect::new(
                NSPoint::new(pad, from_top(card_h, 16.0, 20.0)),
                NSSize::new(240.0, 20.0),
            ),
        );
        if !title.is_null() {
            let _: () = msg_send![effect, addSubview: title];
        }
        if let Some(badge) = vm.badge {
            let label = make_label(
                badge,
                system_font(11.0, WEIGHT_MEDIUM),
                accent,
                NSRect::new(
                    NSPoint::new(card_w - pad - 220.0, from_top(card_h, 19.0, 14.0)),
                    NSSize::new(220.0, 14.0),
                ),
            );
            if !label.is_null() {
                set_alignment(label, ALIGN_RIGHT);
                let _: () = msg_send![effect, addSubview: label];
            }
        }

        // Left column: theme list. Right column: sample swatches.
        let col_split = card_w * 0.46;
        let section_label = |text: &str, focused: bool, x: f64, y_top: f64, w: f64| {
            let color = if focused { accent } else { muted };
            let label = make_label(
                &text.to_uppercase(),
                system_font(10.5, WEIGHT_SEMIBOLD),
                color,
                NSRect::new(
                    NSPoint::new(x, from_top(card_h, y_top, 13.0)),
                    NSSize::new(w, 13.0),
                ),
            );
            if !label.is_null() {
                let _: () = msg_send![effect, addSubview: label];
            }
        };
        section_label(
            if vm.theme_section_focused {
                "Theme \u{25cf}"
            } else {
                "Theme"
            },
            vm.theme_section_focused,
            pad,
            46.0,
            col_split - pad,
        );
        section_label("Sample", false, col_split + 12.0, 46.0, 120.0);

        // Filter line.
        let filter = make_label(
            &format!("/{}", vm.filter),
            mono_digit_font(12.0),
            muted,
            NSRect::new(
                NSPoint::new(pad, from_top(card_h, 64.0, 16.0)),
                NSSize::new(col_split - pad - 8.0, 16.0),
            ),
        );
        if !filter.is_null() {
            let _: () = msg_send![effect, addSubview: filter];
        }

        // Theme list rows, re-windowed around the highlight when the
        // short-card policy shrank the list below the VM's window.
        let theme_highlight = vm.themes.iter().position(|(_, h)| *h).unwrap_or(0);
        let (theme_off, theme_shown) =
            overlay_scroll_window(vm.themes.len(), theme_highlight, list_rows);
        for (i, (name, highlighted)) in vm.themes[theme_off..theme_off + theme_shown]
            .iter()
            .enumerate()
        {
            let y_top = list_top + i as f64 * row_h;
            if *highlighted {
                let bg = make_view(NSRect::new(
                    NSPoint::new(pad - 8.0, from_top(card_h, y_top, row_h)),
                    NSSize::new(col_split - pad, row_h),
                ));
                if !bg.is_null() {
                    tint_layer(bg, ns_color(colors.selected_bg, 1.0), 6.0);
                    let _: () = msg_send![effect, addSubview: bg];
                    release_owned(bg);
                }
            }
            let label = make_label(
                name,
                system_font(
                    13.0,
                    if *highlighted {
                        WEIGHT_MEDIUM
                    } else {
                        WEIGHT_REGULAR
                    },
                ),
                if *highlighted && vm.theme_section_focused {
                    accent
                } else {
                    fg
                },
                NSRect::new(
                    NSPoint::new(pad, from_top(card_h, y_top + 4.0, 17.0)),
                    NSSize::new(col_split - pad - 12.0, 17.0),
                ),
            );
            if !label.is_null() {
                let _: () = msg_send![effect, addSubview: label];
            }
        }

        // Sample swatches: ANSI 8x2 grid, semantic row, truecolor ramp.
        // On short cards the settings section climbs up into the sample
        // area; drop any swatch row that would cross into it (same rule
        // as the wgpu card).
        let sw = 18.0;
        let gap = 4.0;
        let sample_x = col_split + 12.0;
        let swatch_limit = list_top + list_rows as f64 * row_h;
        for (i, &(r, g, b)) in vm.ansi_swatches.iter().enumerate() {
            let row = i / 8;
            let col = i % 8;
            let y_top = list_top + row as f64 * (sw + gap);
            if y_top + sw > swatch_limit {
                continue;
            }
            let square = make_view(NSRect::new(
                NSPoint::new(
                    sample_x + col as f64 * (sw + gap),
                    from_top(card_h, y_top, sw),
                ),
                NSSize::new(sw, sw),
            ));
            if !square.is_null() {
                let color = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
                tint_layer(square, ns_color(color, 1.0), 4.0);
                let _: () = msg_send![effect, addSubview: square];
                release_owned(square);
            }
        }
        let semantic_top = list_top + 2.0 * (sw + gap) + 4.0;
        for (i, &(r, g, b)) in vm.semantic_swatches.iter().enumerate() {
            if semantic_top + sw > swatch_limit {
                break;
            }
            let w = 2.0 * sw + gap;
            let square = make_view(NSRect::new(
                NSPoint::new(
                    sample_x + i as f64 * (w + gap),
                    from_top(card_h, semantic_top, sw),
                ),
                NSSize::new(w, sw),
            ));
            if !square.is_null() {
                let color = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
                tint_layer(square, ns_color(color, 1.0), 4.0);
                let _: () = msg_send![effect, addSubview: square];
                release_owned(square);
            }
        }
        if vm.show_truecolor_ramp && semantic_top + sw + gap + 4.0 + 10.0 <= swatch_limit {
            let ramp_top = semantic_top + sw + gap + 4.0;
            let ramp_w = 8.0 * (sw + gap) - gap;
            let steps = 16;
            let step_w = ramp_w / steps as f64;
            for step in 0..steps {
                let t = step as f32 / (steps - 1) as f32;
                let span = (0xe0 - 0x20) as f32 / 255.0;
                let r = 0x20 as f32 / 255.0 + t * span;
                let b = 0xe0 as f32 / 255.0 - t * span;
                let square = make_view(NSRect::new(
                    NSPoint::new(
                        sample_x + step as f64 * step_w,
                        from_top(card_h, ramp_top, 10.0),
                    ),
                    NSSize::new(step_w + 0.5, 10.0),
                ));
                if !square.is_null() {
                    tint_layer(square, ns_color([r, 0x60 as f32 / 255.0, b, 1.0], 1.0), 0.0);
                    let _: () = msg_send![effect, addSubview: square];
                    release_owned(square);
                }
            }
        }

        // Settings section, windowed around the selection when the
        // short-card policy cut it down.
        let settings_top = list_top + list_rows as f64 * row_h + section_gap;
        section_label(
            if vm.settings_focused {
                "Settings \u{25cf}"
            } else {
                "Settings"
            },
            vm.settings_focused,
            pad,
            settings_top,
            200.0,
        );
        let rows_top = settings_top + settings_header_h;
        let settings_sel = vm.rows.iter().position(|r| r.3).unwrap_or(0);
        let (settings_off, settings_shown) =
            overlay_scroll_window(settings_total, settings_sel, settings_rows);
        for (i, (label_text, value, restart, selected)) in vm.rows
            [settings_off..settings_off + settings_shown]
            .iter()
            .enumerate()
        {
            let y_top = rows_top + i as f64 * srow_h;
            if *selected {
                let bg = make_view(NSRect::new(
                    NSPoint::new(pad - 8.0, from_top(card_h, y_top, srow_h)),
                    NSSize::new(card_w - pad * 2.0 + 16.0, srow_h),
                ));
                if !bg.is_null() {
                    tint_layer(bg, ns_color(colors.selected_bg, 1.0), 6.0);
                    let _: () = msg_send![effect, addSubview: bg];
                    release_owned(bg);
                }
            }
            let label = make_label(
                label_text,
                system_font(12.5, WEIGHT_REGULAR),
                if *selected && vm.settings_focused {
                    accent
                } else {
                    fg
                },
                NSRect::new(
                    NSPoint::new(pad, from_top(card_h, y_top + 4.0, 16.0)),
                    NSSize::new(220.0, 16.0),
                ),
            );
            if !label.is_null() {
                let _: () = msg_send![effect, addSubview: label];
            }
            let value_text = if *restart {
                format!("{value}  (restart to apply)")
            } else {
                value.clone()
            };
            let value_label = make_label(
                &value_text,
                system_font(12.5, WEIGHT_REGULAR),
                if *restart { muted } else { fg },
                NSRect::new(
                    NSPoint::new(pad + 230.0, from_top(card_h, y_top + 4.0, 16.0)),
                    NSSize::new(card_w - pad * 2.0 - 230.0, 16.0),
                ),
            );
            if !value_label.is_null() {
                let _: () = msg_send![effect, addSubview: value_label];
            }
        }

        // Footer: key hints or the commit error.
        let (footer_text, tone) = &vm.footer;
        let footer = make_label(
            footer_text,
            system_font(11.0, WEIGHT_REGULAR),
            match tone {
                Tone::Danger => danger,
                Tone::Muted => muted,
            },
            NSRect::new(
                NSPoint::new(pad, 12.0),
                NSSize::new(card_w - pad * 2.0, 15.0),
            ),
        );
        if !footer.is_null() {
            let _: () = msg_send![effect, addSubview: footer];
        }
    }
}

// -----------------------------------------------------------------------
// confirm dialog
// -----------------------------------------------------------------------

pub(in crate::macos_overlay) fn rebuild_confirm(
    window: &Window,
    model: Option<(&ConfirmDialogSnapshot, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let view = content_view(window);
    if view.is_null() {
        return;
    }
    unsafe {
        remove_subview(view, ID_CONFIRM);
        let Some((snap, pane)) = model else {
            return;
        };
        let root = make_modal_root(view, ID_CONFIRM, pane, true);
        if root.is_null() {
            return;
        }

        let card_w = 420.0_f64.min(pane.w - 32.0).max(240.0);
        let card_h = 84.0;
        let card_frame = NSRect::new(
            NSPoint::new(
                (pane.w - card_w) / 2.0,
                from_top(pane.h, (pane.h * 0.30).min(pane.h - card_h), card_h),
            ),
            NSSize::new(card_w, card_h),
        );
        let (host, effect) = make_card(card_frame, MATERIAL_POPOVER, colors, CARD_RADIUS);
        if host.is_null() || effect.is_null() {
            return;
        }
        let _: () = msg_send![root, addSubview: host];
        release_owned(host);

        let message = make_label(
            &snap.message,
            system_font(13.5, WEIGHT_SEMIBOLD),
            ns_color(colors.surface_fg, 1.0),
            NSRect::new(
                NSPoint::new(16.0, from_top(card_h, 18.0, 18.0)),
                NSSize::new(card_w - 32.0, 18.0),
            ),
        );
        if !message.is_null() {
            set_alignment(message, ALIGN_CENTER);
            let _: () = msg_send![effect, addSubview: message];
        }
        let hint = make_label(
            &snap.hint,
            system_font(11.5, WEIGHT_REGULAR),
            ns_color(colors.muted, 1.0),
            NSRect::new(
                NSPoint::new(16.0, from_top(card_h, 48.0, 15.0)),
                NSSize::new(card_w - 32.0, 15.0),
            ),
        );
        if !hint.is_null() {
            set_alignment(hint, ALIGN_CENTER);
            let _: () = msg_send![effect, addSubview: hint];
        }
    }
}

// -----------------------------------------------------------------------
// "Set Tab Title" prompt
// -----------------------------------------------------------------------

pub(in crate::macos_overlay) fn rebuild_title_prompt(
    window: &Window,
    model: Option<(&str, PaneRectPt)>,
    colors: &OverlayColors,
) {
    let view = content_view(window);
    if view.is_null() {
        return;
    }
    unsafe {
        remove_subview(view, ID_TITLE_PROMPT);
        let Some((input, pane)) = model else {
            return;
        };
        let root = make_modal_root(view, ID_TITLE_PROMPT, pane, true);
        if root.is_null() {
            return;
        }

        let card_w = 420.0_f64.min(pane.w - 32.0).max(240.0);
        let card_h = 104.0;
        let card_frame = NSRect::new(
            NSPoint::new(
                (pane.w - card_w) / 2.0,
                from_top(pane.h, (pane.h * 0.30).min(pane.h - card_h), card_h),
            ),
            NSSize::new(card_w, card_h),
        );
        let (host, effect) = make_card(card_frame, MATERIAL_POPOVER, colors, CARD_RADIUS);
        if host.is_null() || effect.is_null() {
            return;
        }
        let _: () = msg_send![root, addSubview: host];
        release_owned(host);

        let title = make_label(
            "Set Tab Title",
            system_font(13.5, WEIGHT_SEMIBOLD),
            ns_color(colors.surface_fg, 1.0),
            NSRect::new(
                NSPoint::new(CARD_PAD_H, from_top(card_h, 14.0, 18.0)),
                NSSize::new(card_w - CARD_PAD_H * 2.0, 18.0),
            ),
        );
        if !title.is_null() {
            set_alignment(title, ALIGN_CENTER);
            let _: () = msg_send![effect, addSubview: title];
        }
        // Input row: live text with a trailing accent caret, mirroring the
        // palette's query row.
        let text = format!("{input}\u{258f}");
        let caret_pos = text.chars().count() - 1;
        let input_label = make_match_label(
            &text,
            &[caret_pos],
            15.0,
            ns_color(colors.surface_fg, 1.0),
            ns_color(colors.accent, 1.0),
            NSRect::new(
                NSPoint::new(CARD_PAD_H, from_top(card_h, 40.0, 20.0)),
                NSSize::new(card_w - CARD_PAD_H * 2.0, 20.0),
            ),
        );
        if !input_label.is_null() {
            set_alignment(input_label, ALIGN_CENTER);
            let _: () = msg_send![effect, addSubview: input_label];
        }
        let hint = make_label(
            crate::macos_overlay::TITLE_PROMPT_HINT,
            system_font(11.5, WEIGHT_REGULAR),
            ns_color(colors.muted, 1.0),
            NSRect::new(
                NSPoint::new(CARD_PAD_H, from_top(card_h, 72.0, 15.0)),
                NSSize::new(card_w - CARD_PAD_H * 2.0, 15.0),
            ),
        );
        if !hint.is_null() {
            set_alignment(hint, ALIGN_CENTER);
            let _: () = msg_send![effect, addSubview: hint];
        }
    }
}

// -----------------------------------------------------------------------
// resize toast
// -----------------------------------------------------------------------

pub(in crate::macos_overlay) fn rebuild_toast(
    window: &Window,
    text: Option<&str>,
    colors: &OverlayColors,
) {
    let view = content_view(window);
    if view.is_null() {
        return;
    }
    unsafe {
        remove_subview(view, ID_TOAST);
        let Some(text) = text else {
            return;
        };
        let bounds: NSRect = msg_send![view, bounds];
        let pill_w = (text.chars().count() as f64 * 8.5 + 36.0).max(72.0);
        let pill_h = 34.0;
        let pane = PaneRectPt {
            x: (bounds.size.width - pill_w) / 2.0,
            y: (bounds.size.height - pill_h) / 2.0,
            w: pill_w,
            h: pill_h,
        };
        // No scrim: the toast is informational, not modal.
        let root = make_modal_root(view, ID_TOAST, pane, false);
        if root.is_null() {
            return;
        }
        let card_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(pill_w, pill_h));
        let (host, effect) = make_card(card_frame, MATERIAL_HUD_WINDOW, colors, pill_h / 2.0);
        if host.is_null() || effect.is_null() {
            return;
        }
        let _: () = msg_send![root, addSubview: host];
        release_owned(host);

        let label = make_label(
            text,
            mono_digit_font(13.0),
            ns_color(colors.surface_fg, 1.0),
            NSRect::new(
                NSPoint::new(8.0, from_top(pill_h, 9.0, 16.0)),
                NSSize::new(pill_w - 16.0, 16.0),
            ),
        );
        if !label.is_null() {
            set_alignment(label, ALIGN_CENTER);
            let _: () = msg_send![effect, addSubview: label];
        }
    }
}
