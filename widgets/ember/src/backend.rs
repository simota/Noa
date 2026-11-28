//! Tool-free Responses API transport. Credentials and prompts are sent through
//! curl's stdin configuration, never through argv or a saved request file.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const ENDPOINT: &str = "https://api.openai.com/v1/responses";
const MAX_REQUEST: usize = 512 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(185);

#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub model: String,
    pub instructions: String,
    pub input: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    Canceled,
    Failed(String),
    Uncertain(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Canceled => f.write_str("応答を停止しました"),
            Self::Failed(message) | Self::Uncertain(message) => f.write_str(message),
        }
    }
}

pub trait Backend: Send + Sync {
    fn complete(
        &self,
        request: &CompletionRequest,
        canceled: &AtomicBool,
    ) -> Result<Completion, BackendError>;
}

#[derive(Default)]
pub struct OpenAiBackend;

impl OpenAiBackend {
    pub fn new() -> Self {
        Self
    }

    /// Called when a client connects or refreshes settings, not for each frame.
    pub fn available() -> bool {
        credential().is_ok()
    }
}

impl Backend for OpenAiBackend {
    fn complete(
        &self,
        request: &CompletionRequest,
        canceled: &AtomicBool,
    ) -> Result<Completion, BackendError> {
        if canceled.load(Ordering::Acquire) {
            return Err(BackendError::Canceled);
        }
        let body = request_body(request)?;
        let key = credential()?;
        let config = curl_config(&body, &key);
        let bytes = transport(config.as_bytes(), canceled)?;
        parse_response(&bytes)
    }
}

fn request_body(request: &CompletionRequest) -> Result<String, BackendError> {
    if request.model.is_empty()
        || request.model.len() > 128
        || !request
            .model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:".contains(&b))
    {
        return Err(BackendError::Failed("モデル名を確認してください".into()));
    }
    if request.input.trim().is_empty() {
        return Err(BackendError::Failed("入力が空です".into()));
    }
    let body = json!({
        "model": request.model,
        "instructions": request.instructions,
        "input": request.input,
        "tools": [],
        "tool_choice": "none",
        "store": false,
        "max_output_tokens": 8192
    })
    .to_string();
    if body.len() > MAX_REQUEST {
        return Err(BackendError::Failed(
            "共有する情報が大きすぎます。資料を分けてください".into(),
        ));
    }
    Ok(body)
}

fn credential() -> Result<String, BackendError> {
    for name in ["NOA_EMBER_OPENAI_API_KEY", "OPENAI_API_KEY"] {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
            return validate_key(value.trim().to_owned());
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(value) = keychain_credential() {
        return validate_key(value);
    }
    Err(BackendError::Failed(
        "OpenAI API キーが未設定です。設定画面の接続案内を確認してください".into(),
    ))
}

fn validate_key(value: String) -> Result<String, BackendError> {
    if value.is_empty() || value.len() > 8192 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(BackendError::Failed(
            "API キーの形式を確認してください".into(),
        ));
    }
    Ok(value)
}

#[cfg(target_os = "macos")]
fn keychain_credential() -> Option<String> {
    let mut child = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "noa.ember.openai",
            "-a",
            "api-key",
            "-w",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = child.stdout.take()?;
    let reader = std::thread::spawn(move || read_bounded(output, 8192));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader.join().ok()?.ok()?;
                return status
                    .success()
                    .then(|| String::from_utf8(bytes).ok())
                    .flatten()
                    .map(|s| s.trim().to_owned());
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

fn config_quote(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn curl_config(body: &str, key: &str) -> String {
    format!(
        "header = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"{}\"\n",
        config_quote(key),
        config_quote(body),
    )
}

fn curl_command() -> Command {
    let mut command = Command::new("/usr/bin/curl");
    command
        // --disable must be first: a personal curlrc must not add a redirect,
        // proxy, trace file or another request containing this credential.
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "0",
            "--connect-timeout",
            "10",
            "--max-time",
            "180",
            "--max-filesize",
            "2097152",
            "--request",
            "POST",
            "--url",
            ENDPOINT,
            "--write-out",
            "\n%{http_code}",
            "--config",
            "-",
        ])
        .env_remove("NOA_EMBER_OPENAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|_| "応答を読み取れませんでした".to_string())?;
    if output.len() > limit {
        return Err("応答が上限を超えました".into());
    }
    Ok(output)
}

fn transport(config: &[u8], canceled: &AtomicBool) -> Result<Vec<u8>, BackendError> {
    // Credential lookup may have waited for Keychain permission. A stop that
    // arrived during that wait must be observed before starting a connection.
    if canceled.load(Ordering::Acquire) {
        return Err(BackendError::Canceled);
    }
    let mut child = curl_command().spawn().map_err(|_| {
        BackendError::Failed("API 接続を起動できませんでした（curl が必要です）".into())
    })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || read_bounded(stdout, MAX_RESPONSE));
    let input = child
        .stdin
        .take()
        .ok_or_else(|| BackendError::Failed("API 接続を開始できませんでした".into()))?;
    let mut input = input;
    if canceled.load(Ordering::Acquire) {
        drop(input);
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err(BackendError::Canceled);
    }
    if input.write_all(config).is_err() {
        drop(input);
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err(BackendError::Uncertain(
            "API 接続へ入力を渡し終えたことを確認できません。自動再送していません".into(),
        ));
    }
    drop(input);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if canceled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(BackendError::Canceled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let result = reader.join().map_err(|_| {
                    BackendError::Uncertain("API 応答の読み取りが中断しました".into())
                })?;
                if !status.success() {
                    return Err(BackendError::Uncertain(
                        "API 応答を確認できません。受け付け・処理・利用料金は未確定です。通信状態を確認してください。自動再送は行っていません".into(),
                    ));
                }
                return result.map_err(BackendError::Uncertain);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(BackendError::Uncertain(
                    "API 応答を確認できませんでした。処理・利用料金は未確定です。自動再送は行っていません".into(),
                ));
            }
        }
    }
}

fn parse_response(bytes: &[u8]) -> Result<Completion, BackendError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| BackendError::Uncertain("API 応答が UTF-8 ではありません".into()))?;
    let (body, status) = text
        .rsplit_once('\n')
        .ok_or_else(|| BackendError::Uncertain("API 応答の状態を確認できません".into()))?;
    match status {
        "200" => {}
        "401" | "403" => {
            return Err(BackendError::Failed(
                "API キーまたはモデルの利用権限を確認してください".into(),
            ));
        }
        "429" => {
            return Err(BackendError::Failed(
                "API の利用上限に達しました。利用状況を確認してから再実行してください".into(),
            ));
        }
        "400" | "404" => {
            return Err(BackendError::Failed(
                "API が依頼を受け付けませんでした。モデル名と入力の大きさを確認してください".into(),
            ));
        }
        _ => {
            return Err(BackendError::Uncertain(
                "API から正常な応答を取得できませんでした。処理の完了・利用料金は未確定です。自動再送は行っていません".into(),
            ));
        }
    }
    let value: Value = serde_json::from_str(body)
        .map_err(|_| BackendError::Uncertain("API 応答の形式が不正です".into()))?;
    match value.get("status").and_then(Value::as_str) {
        Some("completed") => {}
        Some("failed" | "incomplete" | "cancelled") => {
            return Err(BackendError::Failed(
                "APIから完了した回答が返されませんでした。依頼の範囲と利用状況を確認してください"
                    .into(),
            ));
        }
        _ => {
            return Err(BackendError::Uncertain(
                "APIの処理が完了したか確認できません。自動再送していません".into(),
            ));
        }
    }
    let mut paragraphs = Vec::new();
    for item in value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if item.get("type").and_then(Value::as_str) != Some("message")
            || item.get("role").and_then(Value::as_str) != Some("assistant")
        {
            continue;
        }
        if item.get("status").and_then(Value::as_str) != Some("completed") {
            return Err(BackendError::Failed("応答本文が完了していません".into()));
        }
        for part in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if part.get("type").and_then(Value::as_str) == Some("output_text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                paragraphs.push(text.to_owned());
            }
        }
    }
    let text = paragraphs.join("\n\n");
    if text.trim().is_empty() {
        return Err(BackendError::Failed(
            "表示できる応答が返されませんでした".into(),
        ));
    }
    Ok(Completion {
        text,
        input_tokens: value.pointer("/usage/input_tokens").and_then(Value::as_u64),
        output_tokens: value
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: &str, content: &str) -> Vec<u8> {
        format!(
            "{}\n200",
            json!({"status":status,"output":[
                {"type":"reasoning","summary":[{"text":"not an answer"}]},
                {"type":"message","role":"assistant","status":"completed",
                "content":[{"type":"output_text","text":content}]}
            ],"usage":{"input_tokens":42,"output_tokens":12}})
        )
        .into_bytes()
    }

    #[test]
    fn only_completed_assistant_output_is_accepted() {
        let complete = parse_response(&response("completed", "# Report\n日本語")).unwrap();
        assert_eq!(complete.text, "# Report\n日本語");
        assert_eq!(complete.input_tokens, Some(42));
        assert_eq!(complete.output_tokens, Some(12));
        assert!(parse_response(&response("incomplete", "partial")).is_err());
        assert!(parse_response(&response("completed", "")).is_err());
    }

    #[test]
    fn errors_do_not_echo_provider_bodies_or_credentials() {
        for status in ["401", "403", "429", "500", "302"] {
            let error = parse_response(format!("secret-and-prompt\n{status}").as_bytes())
                .unwrap_err()
                .to_string();
            assert!(!error.contains("secret"));
            assert!(!error.contains("prompt"));
        }
        assert!(parse_response(b"{}\n200").is_err());
        assert!(parse_response(b"invalid-json\n200").is_err());
    }

    #[test]
    fn explicit_rejections_and_ambiguous_responses_have_different_outcomes() {
        assert!(matches!(
            parse_response(b"{}\n401"),
            Err(BackendError::Failed(_))
        ));
        assert!(matches!(
            parse_response(b"{}\n500"),
            Err(BackendError::Uncertain(_))
        ));
        assert!(matches!(
            parse_response(b"truncated\n200"),
            Err(BackendError::Uncertain(_))
        ));
        assert!(matches!(
            parse_response(&response("in_progress", "")),
            Err(BackendError::Uncertain(_))
        ));
        assert!(matches!(
            parse_response(&response("incomplete", "partial")),
            Err(BackendError::Failed(_))
        ));
    }

    #[test]
    fn request_has_no_tools_or_server_side_conversation_storage() {
        let body = request_body(&CompletionRequest {
            model: "gpt-5-mini".into(),
            instructions: "Only use the supplied documents.".into(),
            input: "Summarize the attached text.".into(),
        })
        .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["tools"], json!([]));
        assert_eq!(body["tool_choice"], "none");
        assert_eq!(body["store"], false);
        assert!(body.get("previous_response_id").is_none());
    }

    #[test]
    fn secrets_and_prompt_stay_off_argv_and_curlrc_is_disabled() {
        let command = curl_command();
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args[0], "--disable");
        assert!(!args.iter().any(|a| a.contains("Bearer")));
        assert!(!args.contains(&"--location"));
        let config = curl_config("{\"input\":\"quoted \\\"text\\\"\\nnext\"}", "test-key");
        assert_eq!(config.lines().count(), 3);
        assert!(config.contains("Authorization: Bearer test-key"));
        assert!(validate_key("key\nheader = bad".into()).is_err());
        assert!(validate_key("key with spaces".into()).is_err());
    }

    #[test]
    fn oversized_input_and_response_fail_closed() {
        assert!(
            request_body(&CompletionRequest {
                model: "gpt-5-mini".into(),
                instructions: String::new(),
                input: "x".repeat(MAX_REQUEST),
            })
            .is_err()
        );
        assert!(read_bounded(&b"12345"[..], 4).is_err());
        assert_eq!(read_bounded(&b"1234"[..], 4).unwrap(), b"1234");
    }

    #[test]
    fn cancellation_before_dispatch_needs_no_credentials_or_network() {
        let canceled = AtomicBool::new(true);
        let result = OpenAiBackend::new().complete(
            &CompletionRequest {
                model: "gpt-5-mini".into(),
                instructions: String::new(),
                input: "hello".into(),
            },
            &canceled,
        );
        assert_eq!(result, Err(BackendError::Canceled));
    }
}
