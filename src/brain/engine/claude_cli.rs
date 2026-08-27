//! Compile through the `claude` CLI — uses the user's existing subscription,
//! so no API key is involved and nothing new needs paying for.

use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

use super::{CompileRequest, CompilerEngine, EngineError};

pub struct ClaudeCliEngine {
    model: String,
    /// The binary to run. Overridable so tests can drive a stub instead of the
    /// real CLI.
    command: String,
}

impl ClaudeCliEngine {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_string(), command: "claude".to_string() }
    }

    /// Run a different binary — tests only.
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    fn resolve_binary(&self) -> Option<std::path::PathBuf> {
        let candidate = std::path::Path::new(&self.command);
        if candidate.is_absolute() || self.command.contains(std::path::MAIN_SEPARATOR) {
            return candidate.is_file().then(|| candidate.to_path_buf());
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(&self.command))
            .find(|p| p.is_file())
    }
}

#[async_trait]
impl CompilerEngine for ClaudeCliEngine {
    fn name(&self) -> &'static str { "claude-cli" }

    fn available(&self) -> Result<(), EngineError> {
        if self.resolve_binary().is_some() {
            return Ok(());
        }
        Err(EngineError::Unavailable(format!(
            "'{}' is not on PATH. Install Claude Code (https://claude.ai/code), or switch \
             the brain to the gemini-api engine.",
            self.command
        )))
    }

    async fn complete(&self, req: CompileRequest<'_>) -> Result<String, EngineError> {
        self.available()?;

        let mut child = tokio::process::Command::new(&self.command)
            .arg("-p")
            .arg("--model")
            .arg(&self.model)
            .arg("--output-format")
            .arg("text")
            .arg("--append-system-prompt")
            .arg(req.system)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| EngineError::Failed(format!("Cannot start '{}': {e}", self.command)))?;

        // The raw material goes in on stdin — it can be far larger than an
        // argument list allows.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| EngineError::Failed("Cannot open stdin for the claude CLI".into()))?;
        let input = req.input.to_string();
        let writer = tokio::spawn(async move {
            stdin.write_all(input.as_bytes()).await?;
            stdin.shutdown().await
        });

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| EngineError::Failed(format!("'{}' failed: {e}", self.command)))?;
        writer
            .await
            .map_err(|e| EngineError::Failed(format!("stdin writer panicked: {e}")))?
            .map_err(|e| EngineError::Failed(format!("Cannot write to '{}': {e}", self.command)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EngineError::Failed(format!(
                "'{}' exited with {}: {}",
                self.command,
                output.status,
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}
