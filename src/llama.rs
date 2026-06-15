use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::attachments::ImageInput;
use crate::config::{Config, ThinkingMode};
use crate::sandbox::DevAction;

const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
const HEALTH_PATH: &str = "/health";
const DISCORD_MARKDOWN_PROMPT: &str = "\
Format replies with Discord-compatible Markdown when it improves readability:
- Use short bullet lists for steps or options.
- Use fenced code blocks for commands, logs, code, and fixed-width tables.
- Use compact Markdown pipe tables only when comparing small structured data.
- Never use LaTeX syntax. Do not write $...$, \\(...\\), \\epsilon, \\Delta, \\times, \\text{}, or \\frac.
- For math and physics, write formulas in plain text or fenced code blocks.
- Prefer ASCII formulas, for example: P = epsilon0 * (epsilon_r - 1) * E.
- For calculation questions, show the given values, substitute them, and provide the numeric result with units.
- For calculation answers, use this compact shape: Given / Formula / Calculation / Answer.
- If a Korean electrical-engineering prompt says 전위경도, treat it as electric field strength E unless the user states otherwise.
- Avoid long caveats when the user's formula and values are enough to calculate.
Keep tables narrow enough for mobile Discord, and avoid tables for prose answers.";
const DEFAULT_TEMPERATURE: f32 = 0.7;
const DEV_TEMPERATURE: f32 = 0.2;

#[derive(Clone)]
pub struct LlamaClient {
    base_url: String,
    model: String,
    system_prompt: String,
    max_tokens: u32,
    prompt_chars: usize,
    thinking_mode: ThinkingMode,
    markdown_output: bool,
    http: reqwest::Client,
}

impl LlamaClient {
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(config.llama_timeout)
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            base_url: config.llama_base_url.clone(),
            model: config.llama_model.clone(),
            system_prompt: config.llama_system_prompt.clone(),
            max_tokens: config.llama_max_tokens,
            prompt_chars: config.llama_prompt_chars,
            thinking_mode: config.llama_thinking_mode,
            markdown_output: config.discord_markdown_output,
            http,
        })
    }

    pub async fn health(&self) -> Result<String> {
        let url = self.url(HEALTH_PATH);
        let response = self
            .http
            .get(url)
            .send()
            .await
            .context("failed to call llama health endpoint")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status.is_success() {
            return Ok(if body.trim().is_empty() {
                "healthy".to_string()
            } else {
                body
            });
        }

        Err(anyhow!("llama server returned {status}: {body}"))
    }

    pub async fn ask(&self, prompt: &str) -> Result<String> {
        self.ask_with_images(prompt, &[]).await
    }

    pub async fn ask_with_images(&self, prompt: &str, images: &[ImageInput]) -> Result<String> {
        self.complete(
            prompt,
            images,
            CompletionOptions {
                markdown_instructions: self.markdown_output,
                clean_output: true,
                temperature: DEFAULT_TEMPERATURE,
            },
        )
        .await
    }

    async fn complete(
        &self,
        prompt: &str,
        images: &[ImageInput],
        options: CompletionOptions,
    ) -> Result<String> {
        let url = self.url(CHAT_COMPLETIONS_PATH);
        let prompt = trim_prompt(prompt, self.prompt_chars);
        let mut messages = vec![ChatMessage {
            role: "system",
            content: MessageContent::Text(self.system_prompt.clone()),
        }];

        if options.markdown_instructions {
            messages.push(ChatMessage {
                role: "system",
                content: MessageContent::Text(DISCORD_MARKDOWN_PROMPT.to_string()),
            });
        }

        let content = if images.is_empty() {
            MessageContent::Text(prompt.clone())
        } else {
            let mut parts = vec![ContentPart::Text {
                text: prompt.clone(),
            }];
            parts.extend(images.iter().map(|image| ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: format!(
                        "data:{};base64,{}",
                        image.mime.as_str(),
                        image.data_base64.as_str()
                    ),
                },
            }));
            MessageContent::Parts(parts)
        };
        messages.push(ChatMessage {
            role: "user",
            content,
        });

        info!(
            mode = if images.is_empty() {
                "text-only"
            } else {
                "multimodal"
            },
            image_count = images.len(),
            image_filenames = ?images.iter().map(|image| image.filename.as_str()).collect::<Vec<_>>(),
            "sending llama chat request"
        );

        let request = ChatCompletionRequest {
            model: self.model.as_str(),
            messages,
            max_tokens: self.max_tokens,
            temperature: options.temperature,
            stream: false,
            chat_template_kwargs: ChatTemplateKwargs {
                enable_thinking: self.enable_thinking_for(&prompt),
            },
        };

        let response = self
            .http
            .post(url)
            .json(&request)
            .send()
            .await
            .context("failed to call llama chat endpoint")?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("llama server returned {status}: {body}"));
        }

        let completion = response
            .json::<ChatCompletionResponse>()
            .await
            .context("invalid llama response JSON")?;
        let content = completion
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content())
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| anyhow!("llama server returned no visible answer"))?;

        Ok(if options.clean_output {
            clean_discord_output(content)
        } else {
            content.trim().to_string()
        })
    }

    pub async fn dev_action(
        &self,
        task: &str,
        transcript: &str,
        step: usize,
        max_steps: usize,
    ) -> Result<DevAction> {
        let prompt = format!(
            "You are controlling a disposable, network-disabled Linux development container.\n\
The only persistent path is /workspace. Host files, credentials, Docker/Podman sockets, and the \
Discord bot source are not mounted.\n\
Complete the user's development task by issuing one shell command at a time.\n\
User task:\n{task}\n\n\
Previous command transcript:\n{transcript}\n\n\
Execution rules:\n\
- You are on step {step} of at most {max_steps}.\n\
- Inspect existing files before editing.\n\
- Prefer non-interactive commands and never wait for user input.\n\
- Return exactly one JSON object and no Markdown.\n\
- To run a command: {{\"command\":\"shell command\",\"done\":false,\"summary\":null}}\n\
- When complete: {{\"command\":null,\"done\":true,\"summary\":\"concise Korean result summary\"}}"
        );
        let response = self
            .complete(
                &prompt,
                &[],
                CompletionOptions {
                    markdown_instructions: false,
                    clean_output: false,
                    temperature: DEV_TEMPERATURE,
                },
            )
            .await?;
        parse_dev_action(&response)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn enable_thinking_for(&self, prompt: &str) -> bool {
        match self.thinking_mode {
            ThinkingMode::On => true,
            ThinkingMode::Off => false,
            ThinkingMode::Auto => should_enable_thinking(prompt),
        }
    }
}

#[derive(Clone, Copy)]
struct CompletionOptions {
    markdown_instructions: bool,
    clean_output: bool,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
    chat_template_kwargs: ChatTemplateKwargs,
}

#[derive(Serialize)]
struct ChatTemplateKwargs {
    enable_thinking: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: MessageContent,
}

#[derive(Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
}

impl ChatChoiceMessage {
    fn content(self) -> Option<String> {
        non_empty(self.content)
    }
}

fn non_empty(content: Option<String>) -> Option<String> {
    content.filter(|content| !content.trim().is_empty())
}

fn clean_discord_output(content: String) -> String {
    let replacements = [
        ("\\epsilon_0", "epsilon0"),
        ("\\epsilon_r", "epsilon_r"),
        ("\\epsilon_s", "epsilon_s"),
        ("\\epsilon", "epsilon"),
        ("\\Delta", "Delta"),
        ("\\times", "*"),
        ("\\text{V/m}", "V/m"),
        ("\\text{C/m}^2", "C/m^2"),
        ("\\text{F/m}", "F/m"),
        ("\\,", " "),
    ];
    let mut cleaned = content.replace('$', "");

    for (from, to) in replacements {
        cleaned = cleaned.replace(from, to);
    }

    cleaned
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn trim_prompt(prompt: &str, max_chars: usize) -> String {
    let chars = prompt.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return prompt.to_string();
    }

    let start = chars.len().saturating_sub(max_chars);
    let tail = chars[start..].iter().collect::<String>();
    format!("Earlier context was truncated to fit the model context window.\n\n{tail}")
}

fn should_enable_thinking(prompt: &str) -> bool {
    let prompt = prompt.trim();
    let lower = prompt.to_ascii_lowercase();
    let reasoning_markers = [
        "why",
        "how",
        "explain",
        "analyze",
        "compare",
        "debug",
        "error",
        "code",
        "plan",
        "calculate",
        "design",
        "reason",
        "왜",
        "어떻게",
        "설명",
        "분석",
        "비교",
        "디버그",
        "에러",
        "오류",
        "코드",
        "계획",
        "계산",
        "설계",
        "원인",
    ];

    prompt.chars().count() > 160
        || reasoning_markers
            .iter()
            .any(|marker| lower.contains(marker))
}

fn parse_dev_action(response: &str) -> Result<DevAction> {
    let response = response.trim();
    if let Ok(action) = serde_json::from_str(response) {
        return Ok(action);
    }

    let start = response
        .find('{')
        .ok_or_else(|| anyhow!("model returned no JSON development action"))?;
    let end = response
        .rfind('}')
        .ok_or_else(|| anyhow!("model returned incomplete JSON development action"))?;
    serde_json::from_str(&response[start..=end]).context("invalid development action JSON")
}

#[cfg(test)]
mod tests {
    use super::{
        clean_discord_output, parse_dev_action, should_enable_thinking, trim_prompt,
        ChatChoiceMessage,
    };

    #[test]
    fn message_content_returns_visible_answer() {
        let message = ChatChoiceMessage {
            content: Some("answer".to_string()),
        };

        assert_eq!(message.content().as_deref(), Some("answer"));
    }

    #[test]
    fn message_content_rejects_empty_content() {
        let message = ChatChoiceMessage {
            content: Some(String::new()),
        };

        assert_eq!(message.content(), None);
    }

    #[test]
    fn message_content_rejects_missing_content() {
        let message = ChatChoiceMessage { content: None };

        assert_eq!(message.content(), None);
    }

    #[test]
    fn thinking_auto_stays_off_for_simple_prompts() {
        assert!(!should_enable_thinking("Say only: ok"));
    }

    #[test]
    fn thinking_auto_turns_on_for_reasoning_prompts() {
        assert!(should_enable_thinking("이 에러 원인 분석해줘"));
    }

    #[test]
    fn clean_discord_output_removes_latex_markers() {
        let output = clean_discord_output(
            "$P = \\epsilon_0 (\\epsilon_r - 1) E$ = $8.855 \\times 10^{-12}$ F/m".to_string(),
        );

        assert_eq!(
            output,
            "P = epsilon0 (epsilon_r - 1) E = 8.855 * 10^{-12} F/m"
        );
    }

    #[test]
    fn trim_prompt_keeps_tail_when_over_budget() {
        let prompt = trim_prompt("abcdef", 3);

        assert!(prompt.ends_with("def"));
        assert!(prompt.contains("truncated"));
    }

    #[test]
    fn parse_dev_action_accepts_fenced_or_prefixed_json() {
        let action =
            parse_dev_action("result:\n```json\n{\"command\":\"ls\",\"done\":false}\n```").unwrap();

        assert_eq!(action.command.as_deref(), Some("ls"));
        assert!(!action.done);
    }

    #[test]
    fn parse_dev_action_preserves_shell_variables() {
        let action =
            parse_dev_action(r#"{"command":"printf '%s\n' \"$HOME\"","done":false}"#).unwrap();

        assert!(action
            .command
            .as_deref()
            .is_some_and(|command| command.contains("$HOME")));
    }
}
