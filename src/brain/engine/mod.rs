//! The compiler engine abstraction.
//!
//! The brain's compiler needs a cheap model to turn raw session logs into
//! knowledge notes. Which model that is, is the user's business — a Claude
//! subscription via the `claude` CLI, or a Gemini API key. Everything above
//! this trait is engine-agnostic: swapping engines never touches the data
//! layer.
//!
//! `complete` is async, unlike the original design sketch. Both engines do I/O
//! (a subprocess, an HTTPS call) and the rest of the binary already runs on
//! tokio; a blocking trait would have meant either blocking the runtime or
//! smuggling a second one in behind it.

use async_trait::async_trait;

pub mod claude_cli;
pub mod gemini;

/// One compilation unit: an instruction, the raw material, and a ceiling.
pub struct CompileRequest<'a> {
    /// The compiler instruction (an embedded template).
    pub system: &'a str,
    /// Raw log / transcript slice to digest.
    pub input: &'a str,
    pub max_output_tokens: usize,
}

#[derive(Debug)]
pub enum EngineError {
    /// The engine cannot run at all: no CLI on PATH, no API key.
    Unavailable(String),
    /// The engine ran but failed — non-zero exit, HTTP error, bad response.
    Failed(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) | Self::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for EngineError {}

#[async_trait]
pub trait CompilerEngine: Send + Sync {
    fn name(&self) -> &'static str;

    /// Whether this engine could run right now. Used by `brain doctor` and by
    /// `brain init`, so the user finds out at setup time rather than at 18:00
    /// when the scheduled compile silently does nothing.
    fn available(&self) -> Result<(), EngineError>;

    async fn complete(&self, req: CompileRequest<'_>) -> Result<String, EngineError>;
}

/// Build the engine named by the config, or by a one-shot `--engine` override.
pub fn create(
    config: &super::config::BrainConfig,
    override_name: Option<&str>,
) -> Result<Box<dyn CompilerEngine>, EngineError> {
    let name = override_name.unwrap_or(&config.compiler.engine);
    match name {
        "claude-cli" => Ok(Box::new(claude_cli::ClaudeCliEngine::new(&config.compiler.model))),
        "gemini-api" => Ok(Box::new(gemini::GeminiApiEngine::new(&config.compiler.gemini.model))),
        other => Err(EngineError::Unavailable(format!(
            "Unknown compiler engine '{other}'. Known engines: claude-cli, gemini-api."
        ))),
    }
}

/// Every engine name `create` accepts — for help text and validation.
pub const ENGINE_NAMES: &[&str] = &["claude-cli", "gemini-api"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::config::BrainConfig;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(label: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-engine-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for the `claude` CLI: echoes back the system prompt it was
    /// given and whatever arrived on stdin.
    #[cfg(unix)]
    fn claude_stub(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("claude-stub");
        std::fs::write(&path, "#!/bin/sh\ninput=$(cat)\necho \"system=$7 input=$input\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A one-shot HTTP server answering with a canned Gemini response that
    /// quotes the request body back, so the test can prove the input travelled.
    fn gemini_stub() -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());

        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the body is complete (Content-Length is always sent).
            loop {
                let n = socket.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: ").or_else(|| l.strip_prefix("Content-Length: ")))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&raw).to_string();
            let body = serde_json::json!({
                "candidates": [{ "content": { "parts": [{ "text": "system=be terse input=raw log" }] } }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).unwrap();
            socket.flush().unwrap();
            request
        });

        (base, handle)
    }

    /// The Phase A acceptance criterion: one `CompileRequest`, two engines,
    /// both carrying the system prompt and the raw input through to a result.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_same_request_works_on_both_engines() {
        let dir = scratch("both");
        let stub = claude_stub(&dir);

        let request = || CompileRequest { system: "be terse", input: "raw log", max_output_tokens: 256 };

        // ── claude-cli ──
        let claude = claude_cli::ClaudeCliEngine::new("haiku")
            .with_command(stub.to_string_lossy().to_string());
        assert_eq!(claude.name(), "claude-cli");
        claude.available().expect("stub is executable");

        let out = claude.complete(request()).await.unwrap();
        assert!(out.contains("system=be terse"), "system prompt not passed: {out}");
        assert!(out.contains("input=raw log"), "stdin not passed: {out}");

        // ── gemini-api ──
        let (base, server) = gemini_stub();
        std::env::set_var(gemini::API_KEY_ENV, "test-key");
        let gemini = gemini::GeminiApiEngine::new("gemini-2.5-flash").with_base_url(base);
        assert_eq!(gemini.name(), "gemini-api");
        gemini.available().expect("key is set");

        let out = gemini.complete(request()).await.unwrap();
        assert!(out.contains("system=be terse"), "{out}");
        assert!(out.contains("input=raw log"), "{out}");

        let seen = server.join().unwrap();
        assert!(seen.contains("be terse"), "system prompt never reached the API");
        assert!(seen.contains("raw log"), "input never reached the API");
        // The key travels in a header, and only there.
        assert!(seen.contains("x-goog-api-key"), "key header missing");

        // ── and the key really is required ──
        std::env::remove_var(gemini::API_KEY_ENV);
        let err = gemini::GeminiApiEngine::new("gemini-2.5-flash").available().unwrap_err();
        assert!(matches!(err, EngineError::Unavailable(_)));
        assert!(err.to_string().contains(gemini::API_KEY_ENV));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_cli_is_reported_as_unavailable_not_as_a_crash() {
        let engine = claude_cli::ClaudeCliEngine::new("haiku")
            .with_command("definitely-not-a-real-binary-xyz");
        let err = engine.available().unwrap_err();
        assert!(matches!(err, EngineError::Unavailable(_)));
        assert!(err.to_string().contains("not on PATH"), "{err}");
    }

    #[test]
    fn create_honours_the_config_and_the_override() {
        let mut cfg = BrainConfig::default();
        assert_eq!(create(&cfg, None).unwrap().name(), "claude-cli");

        // A one-shot `--engine` wins over the config…
        assert_eq!(create(&cfg, Some("gemini-api")).unwrap().name(), "gemini-api");
        // …and does not change it.
        assert_eq!(cfg.compiler.engine, "claude-cli");

        cfg.compiler.engine = "gemini-api".into();
        assert_eq!(create(&cfg, None).unwrap().name(), "gemini-api");

        let err = match create(&cfg, Some("telepathy")) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("an unknown engine name must not resolve"),
        };
        assert!(err.contains("Unknown compiler engine"), "{err}");
        for name in ENGINE_NAMES {
            assert!(err.contains(name), "help text should list {name}");
        }
    }
}
