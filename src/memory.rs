use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::llama::LlamaClient;

const QUESTIONS_LOG: &str = "questions.log";
const CHAT_LOG: &str = "chat.log";
const SUMMARY_FILE: &str = "summary.md";
const STATE_FILE: &str = "state.txt";

#[derive(Clone)]
pub struct Memory {
    enabled: bool,
    capture_all_chat: bool,
    dir: PathBuf,
    summarize_every: u64,
    summary_chars: usize,
    retain_raw_logs: bool,
    max_log_chars: usize,
    entry_chars: usize,
    store_author_names: bool,
}

impl Memory {
    pub fn new(config: &Config) -> Result<Self> {
        let dir = PathBuf::from(&config.memory_dir);

        if config.memory_enabled {
            fs::create_dir_all(&dir).context("failed to create memory directory")?;
        }

        Ok(Self {
            enabled: config.memory_enabled,
            capture_all_chat: config.memory_capture_all_chat,
            dir,
            summarize_every: config.memory_summarize_every,
            summary_chars: config.memory_summary_chars,
            retain_raw_logs: config.memory_retain_raw_logs,
            max_log_chars: config.memory_max_log_chars,
            entry_chars: config.memory_entry_chars,
            store_author_names: config.memory_store_author_names,
        })
    }

    pub fn summary(&self) -> Result<Option<String>> {
        if !self.enabled {
            return Ok(None);
        }

        let path = self.summary_path();
        if !path.exists() {
            return Ok(None);
        }

        let summary = fs::read_to_string(&path)
            .with_context(|| format!("failed to read memory summary: {}", path.display()))?;
        Ok(non_empty(summary))
    }

    pub async fn remember_question(
        &self,
        llama: &LlamaClient,
        author: &str,
        prompt: &str,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.append_question(author, prompt)?;
        if self.capture_all_chat {
            return Ok(());
        }

        let count = self.increment_count()?;

        if count % self.summarize_every == 0 {
            self.summarize(llama).await?;
        }

        Ok(())
    }

    pub async fn remember_chat(
        &self,
        llama: &LlamaClient,
        author: &str,
        content: &str,
    ) -> Result<()> {
        if !self.enabled || !self.capture_all_chat || content.trim().is_empty() {
            return Ok(());
        }

        self.append_chat(author, content)?;
        let count = self.increment_count()?;

        if count % self.summarize_every == 0 {
            self.summarize(llama).await?;
        }

        Ok(())
    }

    fn append_question(&self, author: &str, prompt: &str) -> Result<()> {
        self.append_line(QUESTIONS_LOG, author, prompt)
    }

    fn append_chat(&self, author: &str, content: &str) -> Result<()> {
        self.append_line(CHAT_LOG, author, content)
    }

    fn append_line(&self, file_name: &str, author: &str, content: &str) -> Result<()> {
        let Some(content) = sanitize_memory_content(content, self.entry_chars) else {
            return Ok(());
        };
        let author = self.memory_author(author);
        let path = self.dir.join(file_name);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open memory log: {file_name}"))?;

        writeln!(file, "- {author}: {content}").context("failed to append memory log")?;
        self.compact_log(&path)
    }

    fn compact_log(&self, path: &PathBuf) -> Result<()> {
        if self.retain_raw_logs {
            return Ok(());
        }

        let Ok(metadata) = fs::metadata(path) else {
            return Ok(());
        };

        if metadata.len() <= self.max_log_chars as u64 {
            return Ok(());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to compact memory log: {}", path.display()))?;
        let trimmed = trim_tail_chars(&content, self.max_log_chars);
        fs::write(path, trimmed)
            .with_context(|| format!("failed to write compacted memory log: {}", path.display()))
    }

    fn memory_author<'a>(&self, author: &'a str) -> &'a str {
        if self.store_author_names {
            author
        } else {
            "user"
        }
    }

    fn increment_count(&self) -> Result<u64> {
        let path = self.state_path();
        let count = fs::read_to_string(&path)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0)
            + 1;

        fs::write(&path, count.to_string()).context("failed to write memory state")?;
        Ok(count)
    }

    async fn summarize(&self, llama: &LlamaClient) -> Result<()> {
        let existing_summary = self.summary()?.unwrap_or_else(|| "(none)".to_string());
        let questions = fs::read_to_string(self.questions_path()).unwrap_or_default();
        let chat = fs::read_to_string(self.chat_path()).unwrap_or_default();
        let questions = trim_tail_chars(&questions, self.summary_chars);
        let chat = trim_tail_chars(&chat, self.summary_chars);

        let prompt = format!(
            "Update the bot's long-term memory summary from recent Discord chat and user questions.\n\
Keep only stable preferences, recurring topics, useful facts, and unresolved questions.\n\
Do not include secrets, tokens, or private credentials.\n\
Write concise Korean bullet points.\n\
Keep the summary under {limit} characters.\n\n\
Existing summary:\n{existing_summary}\n\n\
Recent direct questions:\n{questions}\n\n\
Recent general chat:\n{chat}",
            limit = self.summary_chars
        );

        let summary = llama
            .ask(&prompt)
            .await
            .context("failed to summarize memory")?;
        fs::write(
            self.summary_path(),
            sanitize_summary(&summary, self.summary_chars),
        )
        .context("failed to write memory summary")?;

        if !self.retain_raw_logs {
            self.clear_raw_logs()?;
        }

        Ok(())
    }

    fn clear_raw_logs(&self) -> Result<()> {
        for path in [self.questions_path(), self.chat_path()] {
            if path.exists() {
                fs::write(&path, "")
                    .with_context(|| format!("failed to clear memory log: {}", path.display()))?;
            }
        }

        Ok(())
    }

    fn questions_path(&self) -> PathBuf {
        self.dir.join(QUESTIONS_LOG)
    }

    fn chat_path(&self) -> PathBuf {
        self.dir.join(CHAT_LOG)
    }

    fn summary_path(&self) -> PathBuf {
        self.dir.join(SUMMARY_FILE)
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn trim_tail_chars(value: &str, max_chars: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect()
}

fn trim_head_chars(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn sanitize_memory_content(content: &str, max_chars: usize) -> Option<String> {
    let content = content.replace(['\n', '\r', '\t'], " ");
    let content = redact_sensitive_text(&content);
    let content = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let content = trim_head_chars(&content, max_chars);

    non_empty(content)
}

fn sanitize_summary(summary: &str, max_chars: usize) -> String {
    sanitize_memory_content(summary, max_chars).unwrap_or_default()
}

fn redact_sensitive_text(content: &str) -> String {
    content
        .split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    let sensitive_keys = [
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "api_key",
        "authorization",
        "bearer",
        "discord_token",
    ];

    if sensitive_keys.iter().any(|key| lower.contains(key)) {
        return "[REDACTED]".to_string();
    }

    if looks_like_discord_token(token)
        || looks_like_long_secret(token)
        || looks_like_email(token)
        || looks_like_url_with_query(token)
    {
        return "[REDACTED]".to_string();
    }

    token.to_string()
}

fn looks_like_discord_token(token: &str) -> bool {
    let parts = token.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts[0].len() >= 20
        && parts[1].len() >= 6
        && parts[2].len() >= 20
        && parts.iter().all(|part| {
            part.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        })
}

fn looks_like_long_secret(token: &str) -> bool {
    token.len() >= 32
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '='))
}

fn looks_like_email(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| matches!(ch, '<' | '>' | '(' | ')' | ',' | '.'));
    let Some((local, domain)) = token.split_once('@') else {
        return false;
    };

    !local.is_empty() && domain.contains('.') && !domain.ends_with('.')
}

fn looks_like_url_with_query(token: &str) -> bool {
    (token.starts_with("http://") || token.starts_with("https://")) && token.contains('?')
}

#[cfg(test)]
mod tests {
    use super::{sanitize_memory_content, trim_head_chars, trim_tail_chars};

    #[test]
    fn trim_tail_chars_keeps_end() {
        assert_eq!(trim_tail_chars("abcdef", 3), "def");
    }

    #[test]
    fn trim_head_chars_keeps_start() {
        assert_eq!(trim_head_chars("abcdef", 3), "abc");
    }

    #[test]
    fn sanitize_memory_content_redacts_sensitive_values() {
        let sanitized =
            sanitize_memory_content("DISCORD_TOKEN=abc.def.ghi password=secret", 200).unwrap();

        assert_eq!(sanitized, "[REDACTED] [REDACTED]");
    }

    #[test]
    fn sanitize_memory_content_trims_entries() {
        assert_eq!(
            sanitize_memory_content("hello\nworld", 8).as_deref(),
            Some("hello wo")
        );
    }
}
