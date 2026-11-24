//! `agent-usage` (unit C owns this file; this is the D0 stub).

const USAGE: &str = "usage: agent-usage [--demo] [--tab=limits|stats] [--period=today|7d|30d|all] \
                     [--agent=all|claude|codex|agy] [--prefs=FILE] \
                     [--snapshot=FILE.png [--size=WxH]]";

fn main() {
    println!("{USAGE}");
}
