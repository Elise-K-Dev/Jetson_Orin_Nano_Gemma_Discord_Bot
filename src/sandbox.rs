use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::config::Config;
use crate::llama::LlamaClient;

const CONTAINER_WORKSPACE: &str = "/workspace";
const CONTAINER_USER: &str = "1000:1000";
const CONTAINER_MEMORY: &str = "2g";
const CONTAINER_CPUS: &str = "2";
const CONTAINER_PIDS: &str = "256";
const CONTAINER_TMPFS: &str = "/tmp:rw,noexec,nosuid,size=256m";
const DEFAULT_COMPLETION: &str = "작업을 완료했어.";

#[derive(Clone)]
pub struct DevSandbox {
    enabled: bool,
    allowed_user_ids: Vec<u64>,
    runtime: String,
    image: String,
    container: String,
    workspace: String,
    command_timeout: std::time::Duration,
    max_steps: usize,
    output_chars: usize,
    lock: Arc<Mutex<()>>,
}

impl DevSandbox {
    pub fn new(config: &Config) -> Self {
        Self {
            enabled: config.dev_sandbox_enabled,
            allowed_user_ids: config.dev_sandbox_allowed_user_ids.clone(),
            runtime: config.dev_sandbox_runtime.clone(),
            image: config.dev_sandbox_image.clone(),
            container: config.dev_sandbox_container.clone(),
            workspace: config.dev_sandbox_workspace.clone(),
            command_timeout: config.dev_sandbox_timeout,
            max_steps: config.dev_sandbox_max_steps,
            output_chars: config.dev_sandbox_output_chars,
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn authorized(&self, user_id: u64) -> bool {
        self.enabled && self.allowed_user_ids.contains(&user_id)
    }

    pub async fn run_task(&self, llama: &LlamaClient, task: &str) -> Result<String> {
        if !self.enabled {
            return Err(anyhow!("development sandbox is disabled"));
        }

        let _guard = self.lock.lock().await;
        self.ensure_container().await?;

        let mut transcript = String::new();
        for step in 1..=self.max_steps {
            let action = llama
                .dev_action(task, &transcript, step, self.max_steps)
                .await
                .context("failed to plan sandbox action")?;

            if action.done {
                return Ok(action
                    .summary
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_COMPLETION.to_string()));
            }

            let command = required_command(action)?;
            info!(step, command = %trim_chars(&command, 300), "executing sandbox command");
            let result = self.exec(&command).await?;
            append_transcript(&mut transcript, step, &command, &result);
            transcript = trim_tail_chars(&transcript, self.output_chars * 2);
        }

        Err(anyhow!(
            "development task reached the configured step limit ({})",
            self.max_steps
        ))
    }

    async fn ensure_container(&self) -> Result<()> {
        self.require_runtime().await?;

        let inspect = self
            .runtime_command()
            .args(["container", "inspect", &self.container])
            .output()
            .await
            .context("failed to inspect development container")?;
        if inspect.status.success() {
            let start = self.run_runtime(["start", &self.container]).await?;
            if start.status.success()
                || String::from_utf8_lossy(&start.stderr).contains("already running")
            {
                return Ok(());
            }
            return Err(command_error(
                "failed to start development container",
                &start,
            ));
        }

        let volume = format!("{}:/workspace:rw", self.workspace);
        let create = self
            .run_runtime([
                "run",
                "--detach",
                "--init",
                "--stop-signal",
                "SIGKILL",
                "--name",
                &self.container,
                "--hostname",
                "komi-dev",
                "--network",
                "none",
                "--cap-drop",
                "all",
                "--security-opt",
                "no-new-privileges",
                "--pids-limit",
                CONTAINER_PIDS,
                "--memory",
                CONTAINER_MEMORY,
                "--cpus",
                CONTAINER_CPUS,
                "--read-only",
                "--tmpfs",
                CONTAINER_TMPFS,
                "--volume",
                &volume,
                "--workdir",
                CONTAINER_WORKSPACE,
                "--user",
                CONTAINER_USER,
                &self.image,
                "sleep",
                "infinity",
            ])
            .await?;
        if !create.status.success() {
            return Err(command_error(
                "failed to create development container",
                &create,
            ));
        }

        Ok(())
    }

    async fn require_runtime(&self) -> Result<()> {
        let output = self.run_runtime(["--version"]).await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_error("container runtime is unavailable", &output))
        }
    }

    async fn exec(&self, shell_command: &str) -> Result<ExecResult> {
        let timeout_seconds = self.command_timeout.as_secs().max(1).to_string();
        let mut command = self.runtime_command();
        command
            .args([
                "exec",
                "--workdir",
                CONTAINER_WORKSPACE,
                &self.container,
                "timeout",
                "--signal=KILL",
                &timeout_seconds,
                "bash",
                "-lc",
                shell_command,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().context("failed to start sandbox command")?;

        let supervisor_timeout = self.command_timeout + std::time::Duration::from_secs(5);
        let output = timeout(supervisor_timeout, child.wait_with_output())
            .await
            .context("sandbox command timed out")?
            .context("failed to wait for sandbox command")?;
        let exit_code = output.status.code().unwrap_or(-1);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let output = trim_tail_chars(&combined, self.output_chars);
        if exit_code != 0 {
            warn!(exit_code, "sandbox command returned a nonzero status");
        }

        Ok(ExecResult { exit_code, output })
    }

    fn runtime_command(&self) -> Command {
        Command::new(&self.runtime)
    }

    async fn run_runtime<I, S>(&self, args: I) -> Result<std::process::Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.runtime_command()
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to execute container runtime {}", self.runtime))
    }
}

struct ExecResult {
    exit_code: i32,
    output: String,
}

#[derive(Debug, Deserialize)]
pub struct DevAction {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub summary: Option<String>,
}

fn required_command(action: DevAction) -> Result<String> {
    action
        .command
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("model returned neither a command nor a completion"))
}

fn append_transcript(transcript: &mut String, step: usize, command: &str, result: &ExecResult) {
    transcript.push_str(&format!(
        "\nStep {step}\nCommand:\n{command}\nExit code: {}\nOutput:\n{}\n",
        result.exit_code, result.output
    ));
}

fn command_error(message: &str, output: &std::process::Output) -> anyhow::Error {
    let stderr = trim_chars(&String::from_utf8_lossy(&output.stderr), 1000);
    anyhow!("{message}: {stderr}")
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn trim_tail_chars(value: &str, max_chars: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    chars[chars.len().saturating_sub(max_chars)..]
        .iter()
        .collect()
}
