//! Control-path cost over a real loopback server with a mock backend: what
//! one `+ctl` round trip (`listPanels`, `getText`) costs, how big the
//! messages are, and how much CPU idle connections burn.
//!
//! ```text
//! cargo run --release -p noa-ipc --example bench_rpc -- rtt [panels] [iters]
//! cargo run --release -p noa-ipc --example bench_rpc -- idle [conns] [secs]
//! cargo run --release -p noa-ipc --example bench_rpc -- notify [panels] [iters]
//! ```
//!
//! `rtt` prints the median/p99 round trip of each call and the response
//! size; `idle` opens `conns` authenticated connections (each subscribed to
//! `stateChanged`, as `+ctl wait` is), leaves them idle for `secs` and prints
//! the process's CPU time and context switches over that span; `notify`
//! times one `stateChanged` of `panels` panels from broadcast to its arrival
//! at a subscribed client. Binding the
//! loopback port needs the sandbox disabled.

use std::sync::Arc;
use std::time::{Duration, Instant};

use noa_ipc::backend::{GridResult, IpcBackend, PaneRef, TextResult, WindowRef};
use noa_ipc::error::IpcError;
use noa_ipc::protocol::{
    EventKind, ListPanelsResult, Panel, PanelAgent, Row, Span, SpanColor, SplitDirection,
    TextSource, WireId,
};
use noa_ipc::{Broadcaster, Client, ScopeSet, Server, ServerConfig};

const TOKEN: &str = "bench-token";
const SCREEN_COLS: usize = 120;
const SCREEN_ROWS: usize = 40;

struct Mock {
    panels: Vec<Panel>,
    screen: String,
}

impl IpcBackend for Mock {
    fn list_panels(&self) -> Vec<Panel> {
        self.panels.clone()
    }

    fn get_text(&self, pane: PaneRef, _: TextSource, _: usize) -> Result<TextResult, IpcError> {
        if self.panels.iter().any(|panel| panel.pane_id.0 == pane) {
            Ok(TextResult {
                text: self.screen.clone(),
                truncated: false,
            })
        } else {
            Err(IpcError::UnknownPane)
        }
    }

    fn get_grid(&self, _: PaneRef, _: u64, _: u64) -> Result<GridResult, IpcError> {
        Err(IpcError::UnknownPane)
    }

    fn send_text(&self, _: PaneRef, _: &str, _: bool) -> Result<(), IpcError> {
        Ok(())
    }

    fn focus_pane(&self, _: PaneRef) -> Result<(), IpcError> {
        Ok(())
    }

    fn new_tab(&self, _: Option<WindowRef>) -> Result<PaneRef, IpcError> {
        Err(IpcError::Unsupported("new_tab"))
    }

    fn split(&self, _: PaneRef, _: SplitDirection) -> Result<PaneRef, IpcError> {
        Err(IpcError::Unsupported("split"))
    }

    fn close_pane(&self, _: PaneRef) -> Result<(), IpcError> {
        Ok(())
    }
}

/// A worker as the app lists it: an agent with a report, a label, a parent
/// and a four-line colored preview.
fn worker(pane: u64) -> Panel {
    let preview = (0..4)
        .map(|row| Row {
            row,
            spans: vec![
                Span {
                    text: "● ".to_string(),
                    fg: Some(SpanColor::rgb(95, 196, 160)),
                    bg: None,
                    attrs: None,
                },
                Span {
                    text: format!("Running the test suite for crate number {row}"),
                    fg: None,
                    bg: None,
                    attrs: None,
                },
                Span {
                    text: " (12s)".to_string(),
                    fg: Some(SpanColor::Palette(8)),
                    bg: None,
                    attrs: None,
                },
            ],
        })
        .collect();
    Panel {
        window_group_id: WireId(1),
        window_id: WireId(1),
        pane_id: WireId(pane),
        name: format!("claude · worker-{pane}"),
        cwd: format!("/Users/someone/repos/project-wt/worker-{pane}"),
        branch: Some(format!("orch/worker-{pane}")),
        process: Some("claude".to_string()),
        busy: true,
        attention: false,
        attachable: true,
        preview,
        surface_id: Some(format!("0x{pane:016x}")),
        agent: Some(PanelAgent {
            kind: Some("claude".to_string()),
            state: Some("working".to_string()),
            detail: Some("running cargo test --workspace".to_string()),
            seq: 3,
        }),
        exit: None,
        parent_pane_id: Some(WireId(1)),
        label: Some(format!("worker-{pane}")),
    }
}

fn screen() -> String {
    let line: String = "x".repeat(SCREEN_COLS - 20);
    (0..SCREEN_ROWS)
        .map(|row| format!("{row:>4} │ {line}\n"))
        .collect()
}

fn start(panels: usize) -> noa_ipc::ServerHandle {
    let mut list = vec![worker(1)];
    list[0].parent_pane_id = None;
    list.extend((2..=panels as u64).map(worker));
    let backend = Arc::new(Mock {
        panels: list,
        screen: screen(),
    });
    Server::start(
        ServerConfig {
            port: 0,
            bind_addr: ServerConfig::DEFAULT_BIND_ADDR,
            token: TOKEN.to_string(),
            allowed_scopes: ScopeSet::from_strings(["read", "input", "control"]),
            hello_deadline: ServerConfig::DEFAULT_HELLO_DEADLINE,
            handshake_timeout: ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
        },
        backend,
        Broadcaster::new(),
    )
    .expect("bind a loopback port (run outside the sandbox)")
}

fn connect(handle: &noa_ipc::ServerHandle) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}/", handle.port()),
        TOKEN,
        ScopeSet::from_strings(["read", "input", "control"]),
    )
    .expect("connect")
}

fn stats(name: &str, mut samples: Vec<Duration>) {
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    println!(
        "{name:<12} median {:>8.1}µs  p99 {:>8.1}µs  min {:>8.1}µs",
        at(0.5).as_secs_f64() * 1e6,
        at(0.99).as_secs_f64() * 1e6,
        samples[0].as_secs_f64() * 1e6,
    );
}

fn rtt(panels: usize, iters: usize) {
    let handle = start(panels);
    let mut client = connect(&handle);
    let listed = client.list_panels().unwrap();
    let bytes = serde_json::to_string(&ListPanelsResult {
        panels: listed.clone(),
    })
    .unwrap()
    .len();
    println!(
        "panels {panels}: listPanels result {bytes} B, screen text {} B",
        screen().len()
    );
    for _ in 0..iters / 10 {
        client.list_panels().unwrap();
    }
    let mut list = Vec::with_capacity(iters);
    let mut text = Vec::with_capacity(iters);
    for _ in 0..iters {
        let at = Instant::now();
        std::hint::black_box(client.list_panels().unwrap());
        list.push(at.elapsed());
        let at = Instant::now();
        std::hint::black_box(
            client
                .get_text(2.min(panels as u64), TextSource::Screen, None)
                .unwrap(),
        );
        text.push(at.elapsed());
    }
    stats("listPanels", list);
    stats("getText", text);
}

fn usage() -> (Duration, i64) {
    // SAFETY: getrusage fills a zeroed rusage.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    (
        tv(usage.ru_utime) + tv(usage.ru_stime),
        usage.ru_nvcsw + usage.ru_nivcsw,
    )
}

fn idle(conns: usize, secs: u64) {
    let handle = start(2);
    let clients: Vec<Client> = (0..conns)
        .map(|_| {
            let mut client = connect(&handle);
            client
                .subscribe(&[EventKind::StateChanged], None)
                .expect("subscribe");
            client
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    let (cpu0, switches0) = usage();
    let at = Instant::now();
    std::thread::sleep(Duration::from_secs(secs));
    let wall = at.elapsed();
    let (cpu1, switches1) = usage();
    let cpu = cpu1 - cpu0;
    println!(
        "idle {conns} conns {:.1}s: cpu {:.2}ms ({:.3}% of a core), {} context switches ({:.0}/s)",
        wall.as_secs_f64(),
        cpu.as_secs_f64() * 1e3,
        cpu.as_secs_f64() / wall.as_secs_f64() * 100.0,
        switches1 - switches0,
        (switches1 - switches0) as f64 / wall.as_secs_f64(),
    );
    drop(clients);
}

fn notify(panels: usize, iters: usize) {
    let handle = start(1);
    let broadcaster = handle.broadcaster();
    let mut client = connect(&handle);
    client
        .subscribe(&[EventKind::StateChanged], None)
        .expect("subscribe");
    let changed: Vec<Panel> = (1..=panels as u64).map(worker).collect();
    let mut samples = Vec::with_capacity(iters);
    for i in 0..iters + iters / 10 {
        let at = Instant::now();
        broadcaster.broadcast_state_changed(changed.clone());
        let notification = client
            .next_notification(Duration::from_secs(5))
            .unwrap()
            .expect("a notification");
        if i >= iters / 10 {
            samples.push(at.elapsed());
        }
        std::hint::black_box(notification);
    }
    stats("stateChanged", samples);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, default: usize| {
        args.get(i)
            .and_then(|arg| arg.parse().ok())
            .unwrap_or(default)
    };
    match args.first().map(String::as_str) {
        Some("idle") => idle(num(1, 20), num(2, 5) as u64),
        Some("notify") => notify(num(1, 20), num(2, 2000)),
        _ => rtt(num(1, 20), num(2, 2000)),
    }
}
