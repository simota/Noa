//! Replay a captured pty byte stream into a headless `Terminal` and dump the
//! final visible screen, for diagnosing rendering divergence against real
//! terminals (`cargo run -p noa-grid --example replay -- <file> <cols> <rows>`).

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_vt::Stream;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: replay <capture-file> <cols> <rows> [byte-limit]");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1]).expect("read capture file");
    let cols: u16 = args[2].parse().expect("cols");
    let rows: u16 = args[3].parse().expect("rows");
    let limit = args
        .get(4)
        .map(|s| s.parse::<usize>().expect("byte-limit"))
        .unwrap_or(bytes.len())
        .min(bytes.len());

    let mut terminal = Terminal::new(GridSize::new(cols, rows));
    let mut stream = Stream::new();
    stream.feed(&bytes[..limit], &mut terminal);
    let _ = terminal.take_pending_writes();

    let screen = terminal.active();
    println!(
        "== screen {}x{} cursor=({},{}) alt={} fed {}/{} bytes ==",
        cols,
        rows,
        terminal.active().cursor.x,
        terminal.active().cursor.y,
        terminal.active_is_alt,
        limit,
        bytes.len()
    );
    for y in 0..rows {
        let Some(row) = screen.visible_row(y) else {
            continue;
        };
        let mut line = String::new();
        for cell in &row.cells {
            if cell
                .attrs
                .contains(noa_core::CellAttrs::WIDE_SPACER)
            {
                continue;
            }
            line.push(cell.ch);
            line.extend(cell.combining.chars());
        }
        println!("{y:>3}|{}|", line.trim_end());
    }
}
