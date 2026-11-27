//! A bounded subprocess for GitHub CLI. No command string is constructed.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn output(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, String> {
    output_with_limit(command, timeout, 1024 * 1024)
}

pub fn output_with_limit(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>, String> {
    use std::os::unix::process::CommandExt;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and touches no parent memory.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("コマンドを開始できません: {e}"))?;
    let stdout = child.stdout.take().ok_or("標準出力を取得できません")?;
    let stderr = child.stderr.take().ok_or("標準エラーを取得できません")?;
    fn read(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
        let mut result = Vec::new();
        let mut exceeded = false;
        let mut chunk = [0u8; 8192];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if result.len().saturating_add(n) > limit {
                exceeded = true;
            }
            if result.len() < limit {
                result.extend_from_slice(&chunk[..n.min(limit - result.len())]);
            }
        }
        (result, exceeded)
    }
    let out = std::thread::spawn(move || read(stdout, limit));
    let err = std::thread::spawn(move || read(stderr, 1024 * 1024));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Err(_) => {
                // SAFETY: the child owns this dedicated process group.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                break None;
            }
            Ok(None) if start.elapsed() >= timeout => {
                // SAFETY: the child was placed in its own process group.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // A CLI or login rc file may leave a background child holding a pipe.
    // It belongs to the dedicated session created above, never the GUI's.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let (stdout, exceeded) = out.join().map_err(|_| "標準出力の読込が終了しました")?;
    let _stderr = err.join().map_err(|_| "標準エラーの読込が終了しました")?;
    match status {
        Some(status) if status.success() && !exceeded => Ok(stdout),
        Some(status) if status.success() => Err("コマンドの出力が読込上限を超えました".into()),
        Some(status) => Err(format!(
            "コマンドが終了コード {} で失敗しました。認証・接続・対象リポジトリを確認してください",
            status.code().unwrap_or(-1)
        )),
        None => {
            Err("コマンドの応答が時間内に確認できません。外部操作の結果を照合してください".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_child_does_not_keep_the_result_pipe_open() {
        let started = Instant::now();
        let result = output(
            Command::new("/bin/sh").args(["-c", "sleep 30"]),
            Duration::from_millis(80),
        );
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
