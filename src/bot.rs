use std::sync::Arc;

use anyhow::{Context as AnyhowContext, Result};
use serenity::all::{
    async_trait, ChannelId, Command, CommandInteraction, Context, CreateInteractionResponse,
    CreateInteractionResponseMessage, EditInteractionResponse, EventHandler, GuildId, Interaction,
    Message, MessageId, Ready, UserId,
};
use serenity::builder::GetMessages;
use tracing::{error, info};

use crate::attachments::{attachment_type, AttachmentDownloader, AttachmentType};
use crate::commands;
use crate::config::Config;
use crate::llama::LlamaClient;
use crate::memory::Memory;
use crate::sandbox::DevSandbox;
use crate::web_search::WebSearch;

const DISCORD_REPLY_LIMIT: usize = 1900;
const DISCORD_STATUS_LIMIT: usize = 1800;

pub struct Handler {
    config: Arc<Config>,
    llama: LlamaClient,
    memory: Memory,
    web_search: WebSearch,
    attachments: AttachmentDownloader,
    sandbox: DevSandbox,
}

impl Handler {
    pub fn new(
        config: Arc<Config>,
        llama: LlamaClient,
        memory: Memory,
        web_search: WebSearch,
        attachments: AttachmentDownloader,
        sandbox: DevSandbox,
    ) -> Self {
        Self {
            config,
            llama,
            memory,
            web_search,
            attachments,
            sandbox,
        }
    }

    async fn register_commands(&self, ctx: &Context) {
        let commands = commands::all_commands(self.sandbox.enabled());
        let result = match self.config.discord_guild_id {
            Some(guild_id) => {
                info!(guild_id, "registering guild slash commands");
                GuildId::new(guild_id)
                    .set_commands(&ctx.http, commands)
                    .await
            }
            None => {
                info!("registering global slash commands");
                Command::set_global_commands(&ctx.http, commands).await
            }
        };

        match result {
            Ok(commands) => info!(count = commands.len(), "registered slash commands"),
            Err(err) => error!("failed to register slash commands: {err:?}"),
        }
    }

    async fn dispatch_command(&self, ctx: &Context, command: &CommandInteraction) -> Result<()> {
        match command.data.name.as_str() {
            commands::PING => self.reply_ephemeral(ctx, command, "pong").await,
            commands::LLM_STATUS => self.llm_status(ctx, command).await,
            commands::ASK => self.ask(ctx, command).await,
            commands::DEV => self.dev(ctx, command).await,
            name => {
                self.reply_ephemeral(ctx, command, &format!("unknown command: {name}"))
                    .await
            }
        }
    }

    async fn reply_ephemeral(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        content: &str,
    ) -> Result<()> {
        command
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(trim_discord(content, DISCORD_REPLY_LIMIT))
                        .ephemeral(true),
                ),
            )
            .await
            .context("failed to reply to interaction")
    }

    async fn llm_status(&self, ctx: &Context, command: &CommandInteraction) -> Result<()> {
        command
            .defer_ephemeral(&ctx.http)
            .await
            .context("failed to defer interaction")?;

        let content = match self.llama.health().await {
            Ok(body) => format!(
                "llama-server online: {}",
                trim_discord(&body, DISCORD_STATUS_LIMIT)
            ),
            Err(err) => format!("llama-server check failed: {err}"),
        };

        self.edit_response(ctx, command, &content).await
    }

    async fn ask(&self, ctx: &Context, command: &CommandInteraction) -> Result<()> {
        let prompt = commands::string_option(&command.data.options, commands::ASK_PROMPT_OPTION)
            .context("missing prompt")?;

        command
            .defer(&ctx.http)
            .await
            .context("failed to defer interaction")?;

        let prompt = self.prompt_with_web_context(prompt, prompt).await?;
        let content = match self.llama.ask(&prompt).await {
            Ok(answer) => trim_discord(&answer, DISCORD_REPLY_LIMIT),
            Err(err) => format!("llama-server request failed: {err}"),
        };

        self.edit_response(ctx, command, &content).await
    }

    async fn dev(&self, ctx: &Context, command: &CommandInteraction) -> Result<()> {
        if !self.sandbox.authorized(command.user.id.get()) {
            return self
                .reply_ephemeral(ctx, command, "이 개발 컨테이너를 사용할 권한이 없어.")
                .await;
        }

        let task = commands::string_option(&command.data.options, commands::DEV_TASK_OPTION)
            .context("missing development task")?;
        command
            .defer_ephemeral(&ctx.http)
            .await
            .context("failed to defer development interaction")?;

        let content = match self.sandbox.run_task(&self.llama, task).await {
            Ok(summary) => format!("개발 작업 완료:\n{summary}"),
            Err(err) => format!("개발 작업 실패: {err}"),
        };
        self.edit_response(ctx, command, &content).await
    }

    async fn edit_response(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        content: &str,
    ) -> Result<()> {
        command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content(trim_discord(content, DISCORD_REPLY_LIMIT)),
            )
            .await
            .context("failed to edit interaction response")?;

        Ok(())
    }

    async fn maybe_reply_to_mention(&self, ctx: &Context, message: &Message) -> Result<()> {
        if !self.config.discord_chat_listener || message.author.bot {
            return Ok(());
        }

        let current_user = ctx
            .http
            .get_current_user()
            .await
            .context("failed to fetch current bot user")?;
        let Some(user_prompt) = mentioned_prompt(
            &message.content,
            current_user.id.get(),
            current_user.name.as_str(),
            &self.config.discord_trigger_names,
        ) else {
            return Ok(());
        };

        if let Some(task) = natural_dev_task(&user_prompt) {
            return self.reply_to_natural_dev_request(ctx, message, task).await;
        }

        let recent_messages = self.recent_channel_messages(ctx, message).await?;
        let explicit_reference = explicit_message_reference(&user_prompt, message.channel_id.get());
        let explicit_message = match explicit_reference {
            Some(reference) => {
                let fetched = ChannelId::new(reference.channel_id)
                    .message(&ctx.http, MessageId::new(reference.message_id))
                    .await;
                match fetched {
                    Ok(target)
                        if target.content.trim().is_empty() && target.attachments.is_empty() =>
                    {
                        self.send_channel_reply(
                            ctx,
                            message,
                            "지정한 메시지에는 읽거나 요약할 내용이 없어.",
                        )
                        .await?;
                        return Ok(());
                    }
                    Ok(target) => {
                        info!(
                            requester_id = message.author.id.get(),
                            attachment_author_id = target.author.id.get(),
                            channel_id = target.channel_id.get(),
                            message_id = target.id.get(),
                            attachment_count = target.attachments.len(),
                            "using explicitly referenced message attachments"
                        );
                        Some(target)
                    }
                    Err(err) => {
                        error!(
                            channel_id = reference.channel_id,
                            message_id = reference.message_id,
                            "failed to fetch explicitly referenced message: {err:?}"
                        );
                        self.send_channel_reply(
                            ctx,
                            message,
                            "지정한 메시지를 찾거나 읽을 수 없어. 메시지 ID나 링크와 채널 권한을 확인해줘.",
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }
            None => None,
        };
        let attachment_intent = attachment_requested(&user_prompt) || explicit_reference.is_some();
        let resolved_attachments = resolve_attachments(
            message,
            explicit_message.as_ref(),
            &recent_messages,
            attachment_intent,
        );
        if message.attachments.is_empty() && !resolved_attachments.is_empty() {
            info!(
                user_id = message.author.id.get(),
                attachment_count = resolved_attachments.len(),
                "using recent same-author attachments"
            );
        }
        let attachment_context = self.attachments.collect(&resolved_attachments).await;
        let all_attachments_failed = !resolved_attachments.is_empty()
            && attachment_context.accepted_count() == 0
            && attachment_context.skipped.len() == resolved_attachments.len();
        let prompt = if let Some(target) = explicit_message.as_ref() {
            explicit_message_prompt(target, &user_prompt)
        } else if self.web_search.has_links(&user_prompt) {
            user_prompt.clone()
        } else {
            self.prompt_with_channel_context(&recent_messages, &user_prompt)?
        };
        let mut prompt = if let Some(target) = explicit_message.as_ref() {
            self.prompt_with_web_context(&target.content, &prompt)
                .await?
        } else {
            self.prompt_with_web_context(&user_prompt, &prompt).await?
        };
        attachment_context.append_text_to(&mut prompt);

        let explicit_has_text = explicit_message
            .as_ref()
            .is_some_and(|target| !target.content.trim().is_empty());
        let response = if all_attachments_failed && attachment_intent && !explicit_has_text {
            "첨부 파일을 읽지 못했어. 파일 크기와 형식을 확인해서 다시 올려줘.".to_string()
        } else {
            match self
                .llama
                .ask_with_images(&prompt, &attachment_context.images)
                .await
            {
                Ok(answer) => trim_discord(&answer, DISCORD_REPLY_LIMIT),
                Err(err) => format!("llama-server request failed: {err}"),
            }
        };
        self.send_channel_reply(ctx, message, &response).await?;

        let memory_prompt = memory_attachment_note(&user_prompt, &resolved_attachments);
        self.memory
            .remember_question(&self.llama, &message.author.name, &memory_prompt)
            .await
            .context("failed to update memory")?;

        Ok(())
    }

    async fn send_channel_reply(
        &self,
        ctx: &Context,
        message: &Message,
        content: &str,
    ) -> Result<()> {
        let response = format!(
            "<@{}> {}",
            message.author.id.get(),
            trim_discord(content, DISCORD_REPLY_LIMIT - 32)
        );
        message
            .channel_id
            .say(&ctx.http, response)
            .await
            .context("failed to send channel reply")?;
        Ok(())
    }

    async fn reply_to_natural_dev_request(
        &self,
        ctx: &Context,
        message: &Message,
        task: String,
    ) -> Result<()> {
        let authorized = self.sandbox.authorized(message.author.id.get());
        let content = match authorized {
            false => "이 개발 컨테이너를 사용할 권한이 없어.".to_string(),
            true => match self.sandbox.run_task(&self.llama, &task).await {
                Ok(summary) => format!("개발 작업 완료:\n{summary}"),
                Err(err) => {
                    error!(
                        user_id = message.author.id.get(),
                        channel_id = message.channel_id.get(),
                        "natural-language development task failed: {err:?}"
                    );
                    format!("개발 작업 실패: {err}")
                }
            },
        };
        let response = format!(
            "<@{}> {}",
            message.author.id.get(),
            trim_discord(&content, DISCORD_REPLY_LIMIT - 32)
        );

        message
            .channel_id
            .say(&ctx.http, response)
            .await
            .context("failed to send development task reply")?;

        if authorized {
            self.memory
                .remember_question(&self.llama, &message.author.name, &task)
                .await
                .context("failed to update development task memory")?;
        }

        Ok(())
    }

    async fn prompt_with_web_context(&self, search_prompt: &str, prompt: &str) -> Result<String> {
        let Some(web_context) = self.web_search.context_for(search_prompt).await? else {
            return Ok(prompt.to_string());
        };

        Ok(format!("{web_context}\n\nUser request:\n{prompt}"))
    }

    async fn recent_channel_messages(
        &self,
        ctx: &Context,
        message: &Message,
    ) -> Result<Vec<Message>> {
        let limit = self.config.discord_context_messages;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut messages = message
            .channel_id
            .messages(
                &ctx.http,
                GetMessages::new().before(message.id).limit(limit),
            )
            .await
            .context("failed to fetch channel context")?;
        messages.reverse();
        Ok(messages)
    }

    fn prompt_with_channel_context(&self, messages: &[Message], prompt: &str) -> Result<String> {
        let context = format_channel_context(messages, self.config.discord_context_chars);
        let memory = self.memory.summary()?;

        if context.is_empty() && memory.is_none() {
            return Ok(prompt.to_string());
        }

        Ok(format_prompt_with_context(
            memory.as_deref(),
            &context,
            prompt,
        ))
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "Discord bot connected");
        self.register_commands(&ctx).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };

        if let Err(err) = self.dispatch_command(&ctx, &command).await {
            error!(command = %command.data.name, "command failed: {err:?}");

            let message = format!("Command failed: {err}");
            if let Err(reply_err) = self.reply_ephemeral(&ctx, &command, &message).await {
                error!("failed to send error response: {reply_err:?}");
            }
        }
    }

    async fn message(&self, ctx: Context, message: Message) {
        if !message.author.bot {
            if let Err(err) = self
                .memory
                .remember_chat(&self.llama, &message.author.name, &message.content)
                .await
            {
                error!(
                    channel_id = message.channel_id.get(),
                    message_id = message.id.get(),
                    "chat memory update failed: {err:?}"
                );
            }
        }

        if let Err(err) = self.maybe_reply_to_mention(&ctx, &message).await {
            error!(
                channel_id = message.channel_id.get(),
                message_id = message.id.get(),
                "message reply failed: {err:?}"
            );
        }
    }
}

fn mentioned_prompt(
    content: &str,
    bot_id: u64,
    bot_name: &str,
    trigger_names: &[String],
) -> Option<String> {
    let content = content.trim();
    if content.is_empty() {
        return None;
    }

    let mention = format!("<@{bot_id}>");
    let nickname_mention = format!("<@!{bot_id}>");
    let lower_content = content.to_ascii_lowercase();
    let lower_name = bot_name.to_ascii_lowercase();

    if content.contains(&mention) || content.contains(&nickname_mention) {
        let prompt = content
            .replace(&mention, "")
            .replace(&nickname_mention, "")
            .trim()
            .to_string();
        return Some(default_prompt_if_empty(prompt));
    }

    if lower_content.contains(&lower_name)
        || trigger_names
            .iter()
            .any(|name| lower_content.contains(&name.to_ascii_lowercase()))
    {
        return Some(content.to_string());
    }

    None
}

fn default_prompt_if_empty(prompt: String) -> String {
    if prompt.is_empty() {
        "무슨 일이야?".to_string()
    } else {
        prompt
    }
}

fn natural_dev_task(prompt: &str) -> Option<String> {
    let lower = prompt.to_ascii_lowercase();
    let trigger_phrases = [
        "개발 컨테이너 써서",
        "개발 컨테이너를 써서",
        "개발 컨테이너 사용해서",
        "개발 컨테이너를 사용해서",
        "개발 컨테이너에서",
        "개발 샌드박스에서",
        "샌드박스에서 개발",
        "dev container로",
        "dev container에서",
        "development container로",
        "development container에서",
    ];
    let trigger = trigger_phrases
        .iter()
        .find(|trigger| lower.contains(**trigger))?;
    let task = remove_first_case_insensitive(prompt, trigger);
    let task = task.split_whitespace().collect::<Vec<_>>().join(" ");

    Some(if task.is_empty() {
        prompt.trim().to_string()
    } else {
        task
    })
}

fn remove_first_case_insensitive(value: &str, needle: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let Some(start) = lower.find(needle) else {
        return value.to_string();
    };
    let end = start + needle.len();
    format!("{}{}", &value[..start], &value[end..])
}

fn attachment_requested(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    [
        "attachment",
        "attached",
        "image",
        "photo",
        "picture",
        "file",
        "첨부",
        "사진",
        "이미지",
        "파일",
        "문서",
        "이거",
        "이것",
        "저거",
        "방금",
        "위에",
        "올린",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn resolve_attachments(
    message: &Message,
    explicit_message: Option<&Message>,
    recent_messages: &[Message],
    attachment_requested: bool,
) -> Vec<serenity::all::Attachment> {
    if !message.attachments.is_empty() {
        return message.attachments.clone();
    }
    if !attachment_requested {
        return Vec::new();
    }

    if let Some(explicit_message) = explicit_message {
        return explicit_message.attachments.clone();
    }

    if let Some(referenced) = message.referenced_message.as_deref() {
        if referenced.author.id == message.author.id && !referenced.attachments.is_empty() {
            return referenced.attachments.clone();
        }
    }

    latest_author_attachments(recent_messages, message.author.id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExplicitMessageReference {
    channel_id: u64,
    message_id: u64,
}

fn explicit_message_reference(
    prompt: &str,
    current_channel_id: u64,
) -> Option<ExplicitMessageReference> {
    for token in prompt.split_whitespace() {
        let token = token.trim_matches(|character: char| {
            matches!(
                character,
                '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | '!' | '?' | ':'
            )
        });

        if let Some(reference) = discord_message_link_reference(token) {
            return Some(reference);
        }

        if is_discord_snowflake(token) {
            return Some(ExplicitMessageReference {
                channel_id: current_channel_id,
                message_id: token.parse().ok()?,
            });
        }
    }

    None
}

fn discord_message_link_reference(token: &str) -> Option<ExplicitMessageReference> {
    let path = token
        .strip_prefix("https://discord.com/channels/")
        .or_else(|| token.strip_prefix("http://discord.com/channels/"))
        .or_else(|| token.strip_prefix("https://discordapp.com/channels/"))
        .or_else(|| token.strip_prefix("http://discordapp.com/channels/"))?;
    let mut parts = path.split('/');
    let _guild_id = parts.next()?;
    let channel_id = parts.next()?.parse().ok()?;
    let message_id = parts.next()?.parse().ok()?;

    Some(ExplicitMessageReference {
        channel_id,
        message_id,
    })
}

fn is_discord_snowflake(value: &str) -> bool {
    (17..=20).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn explicit_message_prompt(message: &Message, user_prompt: &str) -> String {
    let content = message.content.trim();
    let has_links = content_has_http_link(content);
    let mut image_count = 0;
    let mut document_count = 0;
    let mut unsupported_count = 0;

    for attachment in &message.attachments {
        match attachment_type(attachment) {
            AttachmentType::Image => image_count += 1,
            AttachmentType::Document => document_count += 1,
            AttachmentType::Unsupported => unsupported_count += 1,
        }
    }

    let mut types = Vec::new();
    if !content.is_empty() {
        types.push("text");
    }
    if has_links {
        types.push("link");
    }
    if image_count > 0 {
        types.push("image");
    }
    if document_count > 0 {
        types.push("document");
    }
    if unsupported_count > 0 {
        types.push("unsupported attachment");
    }

    let filenames = message
        .attachments
        .iter()
        .map(|attachment| attachment.filename.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let content = if content.is_empty() {
        "(none)"
    } else {
        content
    };
    let filenames = if filenames.is_empty() {
        "(none)"
    } else {
        filenames.as_str()
    };

    format!(
        "You are analyzing an explicitly selected Discord message.\n\
         Detected content types: {}\n\
         Message author: {}\n\
         Message text:\n{}\n\
         Attachment filenames: {}\n\n\
         Use the available inputs according to their type: linked-page excerpts for links, \
         vision input for images, extracted text for documents, and the message text itself. \
         Combine mixed inputs into one coherent answer. If the user did not provide a specific \
         transformation, summarize the selected message and its contents, highlighting key points.\n\n\
         User request:\n{}",
        types.join(", "),
        message.author.name,
        content,
        filenames,
        user_prompt
    )
}

fn content_has_http_link(content: &str) -> bool {
    content
        .split_whitespace()
        .any(|token| token.starts_with("https://") || token.starts_with("http://"))
}

fn latest_author_attachments(
    messages: &[Message],
    author_id: UserId,
) -> Vec<serenity::all::Attachment> {
    let candidates = messages
        .iter()
        .map(|message| (message.author.id.get(), !message.attachments.is_empty()))
        .collect::<Vec<_>>();

    latest_attachment_index(&candidates, author_id.get())
        .map(|index| messages[index].attachments.clone())
        .unwrap_or_default()
}

fn latest_attachment_index(candidates: &[(u64, bool)], author_id: u64) -> Option<usize> {
    candidates
        .iter()
        .rposition(|&(candidate_author_id, has_attachments)| {
            candidate_author_id == author_id && has_attachments
        })
}

fn memory_attachment_note(prompt: &str, attachments: &[serenity::all::Attachment]) -> String {
    if attachments.is_empty() {
        return prompt.to_string();
    }

    let filenames = attachments
        .iter()
        .map(|attachment| attachment.filename.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{prompt} [User attached file(s): {filenames}]")
}

fn format_channel_context(messages: &[Message], max_chars: usize) -> String {
    let mut context = String::new();
    let mut used_chars = 0;

    for message in messages {
        if message.content.trim().is_empty() {
            continue;
        }

        let line = format!(
            "{}: {}\n",
            message.author.name,
            message.content.replace('\n', " ")
        );
        let line_chars = line.chars().count();

        if used_chars + line_chars > max_chars {
            break;
        }

        used_chars += line_chars;
        context.push_str(&line);
    }

    context.trim().to_string()
}

fn format_prompt_with_context(memory: Option<&str>, channel_context: &str, prompt: &str) -> String {
    let mut full_prompt = String::new();

    if let Some(memory) = memory.filter(|value| !value.trim().is_empty()) {
        full_prompt.push_str("Long-term memory summary:\n");
        full_prompt.push_str(memory.trim());
        full_prompt.push_str("\n\n");
    }

    if !channel_context.trim().is_empty() {
        full_prompt.push_str("Recent channel context before the user's message:\n");
        full_prompt.push_str(channel_context.trim());
        full_prompt.push_str("\n\n");
    }

    full_prompt.push_str("User request:\n");
    full_prompt.push_str(prompt);
    full_prompt
}

#[cfg(test)]
fn trim_context_lines(lines: &[&str], max_chars: usize) -> String {
    let mut context = String::new();
    let mut used_chars = 0;

    for line in lines {
        let line = format!("{line}\n");
        let line_chars = line.chars().count();

        if used_chars + line_chars > max_chars {
            break;
        }

        used_chars += line_chars;
        context.push_str(&line);
    }

    context.trim().to_string()
}

fn trim_discord(content: &str, max_chars: usize) -> String {
    let content = content.trim();
    let mut chars = content.chars();
    let mut trimmed = chars.by_ref().take(max_chars).collect::<String>();

    if chars.next().is_some() {
        trimmed.push_str("\n...");
    }

    trimmed
}

#[cfg(test)]
mod tests {
    use super::{
        attachment_requested, content_has_http_link, explicit_message_reference,
        format_prompt_with_context, latest_attachment_index, mentioned_prompt, natural_dev_task,
        trim_context_lines, trim_discord, ExplicitMessageReference,
    };

    #[test]
    fn trim_discord_trims_whitespace() {
        assert_eq!(trim_discord("  pong\n", 10), "pong");
    }

    #[test]
    fn trim_discord_adds_ellipsis_when_needed() {
        assert_eq!(trim_discord("abcdef", 3), "abc\n...");
    }

    #[test]
    fn mentioned_prompt_extracts_discord_mention() {
        assert_eq!(
            mentioned_prompt("<@42> 안녕", 42, "Elise_Bot", &[]).as_deref(),
            Some("안녕")
        );
    }

    #[test]
    fn mentioned_prompt_uses_default_for_empty_mentions() {
        assert_eq!(
            mentioned_prompt("<@!42>", 42, "Elise_Bot", &[]).as_deref(),
            Some("무슨 일이야?")
        );
    }

    #[test]
    fn mentioned_prompt_detects_bot_name() {
        assert_eq!(
            mentioned_prompt("elise_bot 지금 돼?", 42, "Elise_Bot", &[]).as_deref(),
            Some("elise_bot 지금 돼?")
        );
    }

    #[test]
    fn mentioned_prompt_ignores_unrelated_messages() {
        assert_eq!(mentioned_prompt("안녕", 42, "Elise_Bot", &[]), None);
    }

    #[test]
    fn mentioned_prompt_detects_extra_trigger_names() {
        let triggers = vec!["코미".to_string()];
        assert_eq!(
            mentioned_prompt("코미 이거 봐줘", 42, "Elise_Bot", &triggers).as_deref(),
            Some("코미 이거 봐줘")
        );
    }

    #[test]
    fn trim_context_lines_respects_character_budget() {
        assert_eq!(trim_context_lines(&["a: 123", "b: 456"], 7), "a: 123");
    }

    #[test]
    fn format_prompt_with_context_includes_memory_and_channel_context() {
        let prompt = format_prompt_with_context(Some("- likes math"), "A: hi", "help");

        assert!(prompt.contains("Long-term memory summary"));
        assert!(prompt.contains("Recent channel context"));
        assert!(prompt.contains("User request:\nhelp"));
    }

    #[test]
    fn attachment_requested_detects_korean_image_reference() {
        assert!(attachment_requested("이 사진 설명해봐"));
    }

    #[test]
    fn attachment_requested_detects_recent_upload_reference() {
        assert!(attachment_requested("방금 올린 문서 요약해줘"));
    }

    #[test]
    fn latest_attachment_index_uses_latest_matching_author() {
        let candidates = [(10, true), (20, true), (10, false), (10, true)];

        assert_eq!(latest_attachment_index(&candidates, 10), Some(3));
        assert_eq!(latest_attachment_index(&candidates, 20), Some(1));
        assert_eq!(latest_attachment_index(&candidates, 30), None);
    }

    #[test]
    fn explicit_message_reference_accepts_bare_message_id() {
        assert_eq!(
            explicit_message_reference("코미야 메시지 123456789012345678 읽어줘", 42),
            Some(ExplicitMessageReference {
                channel_id: 42,
                message_id: 123456789012345678,
            })
        );
    }

    #[test]
    fn explicit_message_reference_accepts_discord_link() {
        assert_eq!(
            explicit_message_reference(
                "https://discord.com/channels/111/222/333333333333333333",
                42
            ),
            Some(ExplicitMessageReference {
                channel_id: 222,
                message_id: 333333333333333333,
            })
        );
    }

    #[test]
    fn explicit_message_reference_ignores_short_numbers() {
        assert_eq!(explicit_message_reference("코미야 123번 읽어줘", 42), None);
    }

    #[test]
    fn content_has_http_link_detects_message_links() {
        assert!(content_has_http_link("자료: https://example.com/page"));
        assert!(!content_has_http_link("링크 없는 일반 메시지"));
    }

    #[test]
    fn natural_dev_task_detects_explicit_korean_request() {
        assert_eq!(
            natural_dev_task("코미야 개발 컨테이너 써서 hello.py 만들어").as_deref(),
            Some("코미야 hello.py 만들어")
        );
    }

    #[test]
    fn natural_dev_task_ignores_general_container_discussion() {
        assert_eq!(
            natural_dev_task("개발 컨테이너 보안 설정이 어떻게 돼?"),
            None
        );
    }

    #[test]
    fn natural_dev_task_detects_english_request_case_insensitively() {
        assert_eq!(
            natural_dev_task("DEV CONTAINER에서 cargo test 돌려").as_deref(),
            Some("cargo test 돌려")
        );
    }
}
