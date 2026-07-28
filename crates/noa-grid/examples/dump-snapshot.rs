//! Decode a persisted scrollback snapshot and print what it would restore.
//!
//! `scrollback-persist` writes an opaque binary blob, so when a restored pane
//! looks wrong there is otherwise no way to tell whether the capture, the
//! file, or the restore is at fault. This reads a `.nsb` and reports the
//! header plus each row's text and pen, which answers that in one step.
//!
//! ```sh
//! cargo run -p noa-grid --example dump-snapshot -- \
//!     ~/Library/Application\ Support/noa/scrollback/<key>.nsb
//! ```

use noa_core::{CellAttrs, Color};

fn describe(color: Color) -> String {
    match color {
        Color::Default => "default".to_string(),
        Color::Palette(index) => format!("palette({index})"),
        Color::Rgb(rgb) => format!("rgb({},{},{})", rgb.r, rgb.g, rgb.b),
    }
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dump-snapshot <path to .nsb>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("{path}: {err}");
            std::process::exit(1);
        }
    };
    let Some(snapshot) = noa_grid::snapshot::decode(&bytes) else {
        eprintln!("{path}: not a readable snapshot ({} bytes)", bytes.len());
        std::process::exit(1);
    };

    println!(
        "file      {} bytes\ncols      {}\nsaved_at  {}\nrows      {}\nlinks     {}",
        bytes.len(),
        snapshot.cols,
        snapshot.saved_at,
        snapshot.rows.len(),
        snapshot.hyperlinks.len()
    );
    for link in &snapshot.hyperlinks {
        println!("          {}", link.uri);
    }
    println!("--- rows ---");
    for (index, row) in snapshot.rows.iter().enumerate() {
        let text: String = row.cells.iter().map(|cell| cell.ch).collect();
        let wrap = if row.wrapped { " ↩" } else { "" };
        println!("{index:>4} |{}|{wrap}", text.trim_end());

        // Summarize the pens actually used on the row, so a colour or
        // attribute lost in the round-trip is visible without a terminal.
        let mut pens: Vec<String> = Vec::new();
        for cell in row.cells.iter().filter(|cell| cell.ch != ' ') {
            let mut pen = describe(cell.fg);
            if cell.bg != Color::Default {
                pen.push_str(&format!(" on {}", describe(cell.bg)));
            }
            for (flag, name) in [
                (CellAttrs::BOLD, "bold"),
                (CellAttrs::FAINT, "faint"),
                (CellAttrs::ITALIC, "italic"),
                (CellAttrs::UNDERLINE, "underline"),
                (CellAttrs::WIDE, "wide"),
            ] {
                if cell.attrs.contains(flag) {
                    pen.push('+');
                    pen.push_str(name);
                }
            }
            if cell.hyperlink.is_some() {
                pen.push_str("+link");
            }
            if !pens.contains(&pen) {
                pens.push(pen);
            }
        }
        if !pens.is_empty() {
            println!("     └─ {}", pens.join(", "));
        }
    }
}
