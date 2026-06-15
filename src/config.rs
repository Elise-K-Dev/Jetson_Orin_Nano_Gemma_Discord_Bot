use std::time::Duration;
use std::{env, fs};

use anyhow::{bail, Context, Result};

const DEFAULT_LLAMA_SYSTEM_PROMPT: &str =
    "You are a concise Discord assistant running locally on a Jetson.";
const DEFAULT_LLAMA_BASE_URL: &str = "http://100.79.59.49:8080";
const DEFAULT_LLAMA_MODEL: &str = "local-gemma";
const DEFAULT_LLAMA_TIMEOUT_SECS: u64 = 120;
const DEFAULT_LLAMA_MAX_TOKENS: u32 = 512;
const DEFAULT_LLAMA_PROMPT_CHARS: usize = 5000;
const DEFAULT_LLAMA_THINKING_MODE: ThinkingMode = ThinkingMode::Off;
const DEFAULT_DISCORD_CHAT_LISTENER: bool = false;
const DEFAULT_DISCORD_MARKDOWN_OUTPUT: bool = true;
const DEFAULT_DISCORD_CONTEXT_MESSAGES: u8 = 8;
const DEFAULT_DISCORD_CONTEXT_CHARS: usize = 2400;
const DEFAULT_DISCORD_TRIGGER_NAMES: &str = "";
const DEFAULT_MEMORY_ENABLED: bool = false;
const DEFAULT_MEMORY_CAPTURE_ALL_CHAT: bool = false;
const DEFAULT_MEMORY_DIR: &str = "memory";
const DEFAULT_MEMORY_SUMMARIZE_EVERY: u64 = 20;
const DEFAULT_MEMORY_SUMMARY_CHARS: usize = 1600;
const DEFAULT_MEMORY_RETAIN_RAW_LOGS: bool = false;
const DEFAULT_MEMORY_MAX_LOG_CHARS: usize = 20_000;
const DEFAULT_MEMORY_ENTRY_CHARS: usize = 500;
const DEFAULT_MEMORY_STORE_AUTHOR_NAMES: bool = false;
const DEFAULT_WEB_SEARCH_ENABLED: bool = false;
const DEFAULT_WEB_SEARCH_ALWAYS: bool = false;
const DEFAULT_WEB_SEARCH_RESULTS: usize = 5;
const DEFAULT_WEB_FETCH_LINKS: bool = true;
const DEFAULT_WEB_FETCH_MAX_URLS: usize = 3;
const DEFAULT_WEB_FETCH_CHARS: usize = 2400;
const DEFAULT_WEB_FETCH_BODY_BYTES: usize = 512 * 1024;
const DEFAULT_WEB_CRAWL_LINKS: bool = false;
const DEFAULT_WEB_CRAWL_MAX_PAGES: usize = 2;
const DEFAULT_WEB_CRAWL_CHARS: usize = 700;
const DEFAULT_ATTACHMENTS_ENABLED: bool = true;
const DEFAULT_ATTACHMENT_MAX_BYTES: usize = 8_388_608;
const DEFAULT_ATTACHMENT_MAX_IMAGES: usize = 3;
const DEFAULT_ATTACHMENT_MAX_TEXT_CHARS: usize = 12_000;
const DEFAULT_ATTACHMENT_DOWNLOAD_TIMEOUT_SECS: u64 = 20;
const DEFAULT_DEV_SANDBOX_ENABLED: bool = false;
const DEFAULT_DEV_SANDBOX_RUNTIME: &str = "podman";
const DEFAULT_DEV_SANDBOX_IMAGE: &str = "localhost/komi-dev:latest";
const DEFAULT_DEV_SANDBOX_CONTAINER: &str = "komi-dev";
const DEFAULT_DEV_SANDBOX_WORKSPACE: &str = "komi_workspace";
const DEFAULT_DEV_SANDBOX_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DEV_SANDBOX_MAX_STEPS: usize = 8;
const DEFAULT_DEV_SANDBOX_OUTPUT_CHARS: usize = 6000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThinkingMode {
    Auto,
    Off,
    On,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub discord_token: String,
    pub discord_guild_id: Option<u64>,
    pub discord_chat_listener: bool,
    pub discord_markdown_output: bool,
    pub discord_context_messages: u8,
    pub discord_context_chars: usize,
    pub discord_trigger_names: Vec<String>,
    pub memory_enabled: bool,
    pub memory_capture_all_chat: bool,
    pub memory_dir: String,
    pub memory_summarize_every: u64,
    pub memory_summary_chars: usize,
    pub memory_retain_raw_logs: bool,
    pub memory_max_log_chars: usize,
    pub memory_entry_chars: usize,
    pub memory_store_author_names: bool,
    pub web_search_enabled: bool,
    pub web_search_always: bool,
    pub web_search_results: usize,
    pub web_fetch_links: bool,
    pub web_fetch_max_urls: usize,
    pub web_fetch_chars: usize,
    pub web_fetch_body_bytes: usize,
    pub web_crawl_links: bool,
    pub web_crawl_max_pages: usize,
    pub web_crawl_chars: usize,
    pub attachments_enabled: bool,
    pub attachment_max_bytes: usize,
    pub attachment_max_images: usize,
    pub attachment_max_text_chars: usize,
    pub attachment_download_timeout: Duration,
    pub dev_sandbox_enabled: bool,
    pub dev_sandbox_allowed_user_ids: Vec<u64>,
    pub dev_sandbox_runtime: String,
    pub dev_sandbox_image: String,
    pub dev_sandbox_container: String,
    pub dev_sandbox_workspace: String,
    pub dev_sandbox_timeout: Duration,
    pub dev_sandbox_max_steps: usize,
    pub dev_sandbox_output_chars: usize,
    pub llama_base_url: String,
    pub llama_model: String,
    pub llama_system_prompt: String,
    pub llama_timeout: Duration,
    pub llama_max_tokens: u32,
    pub llama_prompt_chars: usize,
    pub llama_thinking_mode: ThinkingMode,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let discord_token = required_env("DISCORD_TOKEN")?;
        let discord_guild_id = parse_optional_env("DISCORD_GUILD_ID")?;
        let discord_chat_listener =
            parse_bool_env("DISCORD_CHAT_LISTENER", DEFAULT_DISCORD_CHAT_LISTENER)?;
        let discord_markdown_output =
            parse_bool_env("DISCORD_MARKDOWN_OUTPUT", DEFAULT_DISCORD_MARKDOWN_OUTPUT)?;
        let discord_context_messages =
            parse_env("DISCORD_CONTEXT_MESSAGES", DEFAULT_DISCORD_CONTEXT_MESSAGES)?.min(100);
        let discord_context_chars =
            parse_env("DISCORD_CONTEXT_CHARS", DEFAULT_DISCORD_CONTEXT_CHARS)?;
        let discord_trigger_names = parse_list_env("DISCORD_TRIGGER_NAMES")
            .unwrap_or_else(|| parse_list(DEFAULT_DISCORD_TRIGGER_NAMES));
        let memory_enabled = parse_bool_env("MEMORY_ENABLED", DEFAULT_MEMORY_ENABLED)?;
        let memory_capture_all_chat =
            parse_bool_env("MEMORY_CAPTURE_ALL_CHAT", DEFAULT_MEMORY_CAPTURE_ALL_CHAT)?;
        let memory_dir =
            optional_env("MEMORY_DIR").unwrap_or_else(|| DEFAULT_MEMORY_DIR.to_string());
        let memory_summarize_every =
            parse_env("MEMORY_SUMMARIZE_EVERY", DEFAULT_MEMORY_SUMMARIZE_EVERY)?;
        let memory_summary_chars = parse_env("MEMORY_SUMMARY_CHARS", DEFAULT_MEMORY_SUMMARY_CHARS)?;
        let memory_retain_raw_logs =
            parse_bool_env("MEMORY_RETAIN_RAW_LOGS", DEFAULT_MEMORY_RETAIN_RAW_LOGS)?;
        let memory_max_log_chars = parse_env("MEMORY_MAX_LOG_CHARS", DEFAULT_MEMORY_MAX_LOG_CHARS)?;
        let memory_entry_chars = parse_env("MEMORY_ENTRY_CHARS", DEFAULT_MEMORY_ENTRY_CHARS)?;
        let memory_store_author_names = parse_bool_env(
            "MEMORY_STORE_AUTHOR_NAMES",
            DEFAULT_MEMORY_STORE_AUTHOR_NAMES,
        )?;
        let web_search_enabled = parse_bool_env("WEB_SEARCH_ENABLED", DEFAULT_WEB_SEARCH_ENABLED)?;
        let web_search_always = parse_bool_env("WEB_SEARCH_ALWAYS", DEFAULT_WEB_SEARCH_ALWAYS)?;
        let web_search_results = parse_env("WEB_SEARCH_RESULTS", DEFAULT_WEB_SEARCH_RESULTS)?;
        let web_fetch_links = parse_bool_env("WEB_FETCH_LINKS", DEFAULT_WEB_FETCH_LINKS)?;
        let web_fetch_max_urls = parse_env("WEB_FETCH_MAX_URLS", DEFAULT_WEB_FETCH_MAX_URLS)?;
        let web_fetch_chars = parse_env("WEB_FETCH_CHARS", DEFAULT_WEB_FETCH_CHARS)?;
        let web_fetch_body_bytes = parse_env("WEB_FETCH_BODY_BYTES", DEFAULT_WEB_FETCH_BODY_BYTES)?;
        let web_crawl_links = parse_bool_env("WEB_CRAWL_LINKS", DEFAULT_WEB_CRAWL_LINKS)?;
        let web_crawl_max_pages = parse_env("WEB_CRAWL_MAX_PAGES", DEFAULT_WEB_CRAWL_MAX_PAGES)?;
        let web_crawl_chars = parse_env("WEB_CRAWL_CHARS", DEFAULT_WEB_CRAWL_CHARS)?;
        let attachments_enabled =
            parse_bool_env_or("ATTACHMENTS_ENABLED", DEFAULT_ATTACHMENTS_ENABLED);
        let attachment_max_bytes =
            parse_nonzero_env_or("ATTACHMENT_MAX_BYTES", DEFAULT_ATTACHMENT_MAX_BYTES);
        let attachment_max_images =
            parse_nonzero_env_or("ATTACHMENT_MAX_IMAGES", DEFAULT_ATTACHMENT_MAX_IMAGES);
        let attachment_max_text_chars = parse_nonzero_env_or(
            "ATTACHMENT_MAX_TEXT_CHARS",
            DEFAULT_ATTACHMENT_MAX_TEXT_CHARS,
        );
        let attachment_download_timeout_secs = parse_nonzero_env_or(
            "ATTACHMENT_DOWNLOAD_TIMEOUT_SECS",
            DEFAULT_ATTACHMENT_DOWNLOAD_TIMEOUT_SECS,
        );
        let dev_sandbox_enabled =
            parse_bool_env_or("DEV_SANDBOX_ENABLED", DEFAULT_DEV_SANDBOX_ENABLED);
        let dev_sandbox_allowed_user_ids =
            parse_u64_list_env("DEV_SANDBOX_ALLOWED_USER_IDS").unwrap_or_default();
        let dev_sandbox_runtime = optional_env("DEV_SANDBOX_RUNTIME")
            .unwrap_or_else(|| DEFAULT_DEV_SANDBOX_RUNTIME.to_string());
        let dev_sandbox_image = optional_env("DEV_SANDBOX_IMAGE")
            .unwrap_or_else(|| DEFAULT_DEV_SANDBOX_IMAGE.to_string());
        let dev_sandbox_container = optional_env("DEV_SANDBOX_CONTAINER")
            .unwrap_or_else(|| DEFAULT_DEV_SANDBOX_CONTAINER.to_string());
        let dev_sandbox_workspace = optional_env("DEV_SANDBOX_WORKSPACE")
            .unwrap_or_else(|| DEFAULT_DEV_SANDBOX_WORKSPACE.to_string());
        let dev_sandbox_timeout_secs =
            parse_nonzero_env_or("DEV_SANDBOX_TIMEOUT_SECS", DEFAULT_DEV_SANDBOX_TIMEOUT_SECS);
        let dev_sandbox_max_steps =
            parse_nonzero_env_or("DEV_SANDBOX_MAX_STEPS", DEFAULT_DEV_SANDBOX_MAX_STEPS);
        let dev_sandbox_output_chars =
            parse_nonzero_env_or("DEV_SANDBOX_OUTPUT_CHARS", DEFAULT_DEV_SANDBOX_OUTPUT_CHARS);

        let llama_base_url = optional_env("LLAMA_BASE_URL")
            .unwrap_or_else(|| DEFAULT_LLAMA_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        let llama_model =
            optional_env("LLAMA_MODEL").unwrap_or_else(|| DEFAULT_LLAMA_MODEL.to_string());
        let llama_system_prompt = parse_system_prompt()?;
        let llama_timeout_secs = parse_env("LLAMA_TIMEOUT_SECS", DEFAULT_LLAMA_TIMEOUT_SECS)?;
        let llama_max_tokens = parse_env("LLAMA_MAX_TOKENS", DEFAULT_LLAMA_MAX_TOKENS)?;
        let llama_prompt_chars = parse_env("LLAMA_PROMPT_CHARS", DEFAULT_LLAMA_PROMPT_CHARS)?;
        let llama_thinking_mode = parse_thinking_mode()?;

        require_nonzero("LLAMA_TIMEOUT_SECS", llama_timeout_secs, true, None)?;
        require_nonzero("LLAMA_MAX_TOKENS", llama_max_tokens, true, None)?;
        require_nonzero("LLAMA_PROMPT_CHARS", llama_prompt_chars, true, None)?;
        require_nonzero(
            "DISCORD_CONTEXT_CHARS",
            discord_context_chars,
            discord_context_messages > 0,
            Some("when context messages are enabled"),
        )?;
        require_nonzero(
            "MEMORY_SUMMARIZE_EVERY",
            memory_summarize_every,
            memory_enabled,
            Some("when memory is enabled"),
        )?;
        require_nonzero(
            "MEMORY_SUMMARY_CHARS",
            memory_summary_chars,
            memory_enabled,
            Some("when memory is enabled"),
        )?;
        require_nonzero(
            "MEMORY_MAX_LOG_CHARS",
            memory_max_log_chars,
            memory_enabled,
            Some("when memory is enabled"),
        )?;
        require_nonzero(
            "MEMORY_ENTRY_CHARS",
            memory_entry_chars,
            memory_enabled,
            Some("when memory is enabled"),
        )?;
        require_nonzero(
            "WEB_SEARCH_RESULTS",
            web_search_results,
            web_search_enabled,
            Some("when web search is enabled"),
        )?;
        require_nonzero(
            "WEB_FETCH_MAX_URLS",
            web_fetch_max_urls,
            web_search_enabled && web_fetch_links,
            Some("when link fetching is enabled"),
        )?;
        require_nonzero(
            "WEB_FETCH_CHARS",
            web_fetch_chars,
            web_search_enabled && web_fetch_links,
            Some("when link fetching is enabled"),
        )?;
        require_nonzero(
            "WEB_FETCH_BODY_BYTES",
            web_fetch_body_bytes,
            web_search_enabled && web_fetch_links,
            Some("when link fetching is enabled"),
        )?;
        require_nonzero(
            "WEB_CRAWL_MAX_PAGES",
            web_crawl_max_pages,
            web_search_enabled && web_crawl_links,
            Some("when link crawling is enabled"),
        )?;
        require_nonzero(
            "WEB_CRAWL_CHARS",
            web_crawl_chars,
            web_search_enabled && web_crawl_links,
            Some("when link crawling is enabled"),
        )?;
        require_nonzero(
            "ATTACHMENT_MAX_BYTES",
            attachment_max_bytes,
            attachments_enabled,
            Some("when attachments are enabled"),
        )?;
        require_nonzero(
            "ATTACHMENT_MAX_IMAGES",
            attachment_max_images,
            attachments_enabled,
            Some("when attachments are enabled"),
        )?;
        require_nonzero(
            "ATTACHMENT_MAX_TEXT_CHARS",
            attachment_max_text_chars,
            attachments_enabled,
            Some("when attachments are enabled"),
        )?;
        require_nonzero(
            "ATTACHMENT_DOWNLOAD_TIMEOUT_SECS",
            attachment_download_timeout_secs,
            attachments_enabled,
            Some("when attachments are enabled"),
        )?;
        if dev_sandbox_enabled && dev_sandbox_allowed_user_ids.is_empty() {
            bail!("DEV_SANDBOX_ALLOWED_USER_IDS must contain at least one Discord user ID");
        }

        Ok(Self {
            discord_token,
            discord_guild_id,
            discord_chat_listener,
            discord_markdown_output,
            discord_context_messages,
            discord_context_chars,
            discord_trigger_names,
            memory_enabled,
            memory_capture_all_chat,
            memory_dir,
            memory_summarize_every,
            memory_summary_chars,
            memory_retain_raw_logs,
            memory_max_log_chars,
            memory_entry_chars,
            memory_store_author_names,
            web_search_enabled,
            web_search_always,
            web_search_results,
            web_fetch_links,
            web_fetch_max_urls,
            web_fetch_chars,
            web_fetch_body_bytes,
            web_crawl_links,
            web_crawl_max_pages,
            web_crawl_chars,
            attachments_enabled,
            attachment_max_bytes,
            attachment_max_images,
            attachment_max_text_chars,
            attachment_download_timeout: Duration::from_secs(attachment_download_timeout_secs),
            dev_sandbox_enabled,
            dev_sandbox_allowed_user_ids,
            dev_sandbox_runtime,
            dev_sandbox_image,
            dev_sandbox_container,
            dev_sandbox_workspace,
            dev_sandbox_timeout: Duration::from_secs(dev_sandbox_timeout_secs),
            dev_sandbox_max_steps,
            dev_sandbox_output_chars,
            llama_base_url,
            llama_model,
            llama_system_prompt,
            llama_timeout: Duration::from_secs(llama_timeout_secs),
            llama_max_tokens,
            llama_prompt_chars,
            llama_thinking_mode,
        })
    }
}

impl std::str::FromStr for ThinkingMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "0" | "false" | "no" | "off" => Ok(Self::Off),
            "1" | "true" | "yes" | "on" => Ok(Self::On),
            _ => bail!("LLAMA_THINKING_MODE must be one of: auto, on, off, 1, 0"),
        }
    }
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_env(name: &str) -> Result<String> {
    optional_env(name).with_context(|| format!("missing required environment variable: {name}"))
}

fn parse_optional_env<T>(name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    optional_env(name)
        .map(|value| {
            value
                .parse::<T>()
                .with_context(|| format!("{name} is invalid"))
        })
        .transpose()
}

fn parse_env<T>(name: &str, fallback: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    optional_env(name)
        .map(|value| {
            value
                .parse::<T>()
                .with_context(|| format!("{name} is invalid"))
        })
        .unwrap_or(Ok(fallback))
}

fn parse_thinking_mode() -> Result<ThinkingMode> {
    if let Some(value) = optional_env("LLAMA_THINKING_MODE") {
        return value.parse();
    }

    if let Some(value) = optional_env("LLAMA_ENABLE_THINKING") {
        return match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(ThinkingMode::On),
            "0" | "false" | "no" | "off" => Ok(ThinkingMode::Off),
            _ => bail!("LLAMA_ENABLE_THINKING must be one of: 1, 0, true, false, yes, no, on, off"),
        };
    }

    Ok(DEFAULT_LLAMA_THINKING_MODE)
}

fn parse_system_prompt() -> Result<String> {
    if let Some(path) = optional_env("LLAMA_SYSTEM_PROMPT_FILE") {
        return fs::read_to_string(&path)
            .with_context(|| format!("failed to read LLAMA_SYSTEM_PROMPT_FILE: {path}"))
            .map(|prompt| prompt.trim().to_string());
    }

    Ok(optional_env("LLAMA_SYSTEM_PROMPT")
        .map(|prompt| prompt.replace("\\n", "\n"))
        .unwrap_or_else(|| DEFAULT_LLAMA_SYSTEM_PROMPT.to_string()))
}

fn parse_bool_env(name: &str, fallback: bool) -> Result<bool> {
    let Some(value) = optional_env(name) else {
        return Ok(fallback);
    };

    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be one of: 1, 0, true, false, yes, no, on, off"),
    }
}

fn parse_env_or<T>(name: &str, fallback: T) -> T
where
    T: std::str::FromStr + Copy,
{
    let Some(value) = optional_env(name) else {
        return fallback;
    };

    match value.parse::<T>() {
        Ok(value) => value,
        Err(_) => {
            tracing::warn!(env = name, "invalid environment value; using default");
            fallback
        }
    }
}

fn parse_nonzero_env_or<T>(name: &str, fallback: T) -> T
where
    T: std::str::FromStr + Copy + Default + Eq,
{
    let value = parse_env_or(name, fallback);
    if value == T::default() {
        tracing::warn!(
            env = name,
            "environment value must be nonzero; using default"
        );
        fallback
    } else {
        value
    }
}

fn parse_bool_env_or(name: &str, fallback: bool) -> bool {
    let Some(value) = optional_env(name) else {
        return fallback;
    };

    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => {
            tracing::warn!(env = name, "invalid environment value; using default");
            fallback
        }
    }
}

fn require_nonzero<T>(name: &str, value: T, enabled: bool, reason: Option<&str>) -> Result<()>
where
    T: Default + Eq,
{
    if enabled && value == T::default() {
        if let Some(reason) = reason {
            bail!("{name} must be greater than 0 {reason}");
        }

        bail!("{name} must be greater than 0");
    }

    Ok(())
}

fn parse_list_env(name: &str) -> Option<Vec<String>> {
    optional_env(name).map(|value| parse_list(&value))
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_u64_list_env(name: &str) -> Option<Vec<u64>> {
    optional_env(name).map(|value| {
        value
            .split(',')
            .filter_map(|item| match item.trim().parse::<u64>() {
                Ok(id) => Some(id),
                Err(_) => {
                    tracing::warn!(env = name, value = item.trim(), "ignoring invalid user ID");
                    None
                }
            })
            .collect()
    })
}
