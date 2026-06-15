mod attachments;
mod bot;
mod commands;
mod config;
mod llama;
mod memory;
mod sandbox;
mod web_search;

use std::sync::Arc;

use anyhow::{Context, Result};
use serenity::all::GatewayIntents;
use serenity::Client;

use crate::attachments::AttachmentDownloader;
use crate::bot::Handler;
use crate::config::Config;
use crate::llama::LlamaClient;
use crate::memory::Memory;
use crate::sandbox::DevSandbox;
use crate::web_search::WebSearch;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Arc::new(Config::from_env()?);
    let llama = LlamaClient::new(&config)?;
    let attachments = AttachmentDownloader::new(&config)?;
    let memory = Memory::new(&config)?;
    let web_search = WebSearch::new(&config)?;
    let sandbox = DevSandbox::new(&config);

    if let Some(task) = dev_task_argument()? {
        let summary = sandbox
            .run_task(&llama, &task)
            .await
            .context("development sandbox task failed")?;
        println!("{summary}");
        return Ok(());
    }

    let mut intents = GatewayIntents::GUILDS;

    if config.discord_chat_listener {
        intents |= GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    }

    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(Handler::new(
            Arc::clone(&config),
            llama,
            memory,
            web_search,
            attachments,
            sandbox,
        ))
        .await
        .context("failed to create Discord client")?;

    client.start().await.context("Discord client failed")?;

    Ok(())
}

fn dev_task_argument() -> Result<Option<String>> {
    let mut args = std::env::args().skip(1);
    let Some(flag) = args.next() else {
        return Ok(None);
    };
    if flag != "--dev-task" {
        anyhow::bail!("unknown argument: {flag}");
    }

    let task = args.collect::<Vec<_>>().join(" ");
    if task.trim().is_empty() {
        anyhow::bail!("--dev-task requires a task");
    }

    Ok(Some(task))
}
