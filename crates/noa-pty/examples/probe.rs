//! Diagnostic: spawn the default shell PTY and print every event for a few
//! seconds. `cargo run -p noa-pty --example probe`
use noa_pty::{Pty, PtyConfig, PtyEvent};
use std::time::{Duration, Instant};

fn main() {
    println!("SHELL={:?}  TERM(parent)={:?}", std::env::var("SHELL"), std::env::var("TERM"));
    let pty = Pty::spawn(PtyConfig::default()).expect("spawn pty");
    let start = Instant::now();
    loop {
        match pty.event_rx().recv_timeout(Duration::from_millis(400)) {
            Ok(PtyEvent::Data(d)) => println!(
                "[{:>6.0?}] DATA {}B: {:?}",
                start.elapsed(),
                d.len(),
                String::from_utf8_lossy(&d)
            ),
            Ok(PtyEvent::Exit(c)) => {
                println!("[{:>6.0?}] EXIT code={c}", start.elapsed());
                break;
            }
            Ok(PtyEvent::Error(e)) => {
                println!("[{:>6.0?}] ERROR {e}", start.elapsed());
                break;
            }
            Err(_) => {
                if start.elapsed() > Duration::from_secs(3) {
                    println!("[{:>6.0?}] still alive after 3s — shell is waiting (good)", start.elapsed());
                    break;
                }
            }
        }
    }
}
