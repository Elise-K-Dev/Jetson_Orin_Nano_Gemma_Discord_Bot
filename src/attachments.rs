use anyhow::{Context, Result};
use base64::Engine as _;
use serenity::all::Attachment;
use tracing::{info, warn};

use crate::config::Config;

const IMAGE_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/jpg", "image/webp"];
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "rs", "py", "js", "ts", "json", "toml", "yaml", "yml", "log", "csv",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentType {
    Image,
    Document,
    Unsupported,
}

pub fn attachment_type(attachment: &Attachment) -> AttachmentType {
    match attachment_kind(attachment) {
        Some(AttachmentKind::Image(_)) => AttachmentType::Image,
        Some(AttachmentKind::Text) => AttachmentType::Document,
        None => AttachmentType::Unsupported,
    }
}

#[derive(Clone, Debug)]
pub struct ImageInput {
    pub filename: String,
    pub mime: String,
    pub data_base64: String,
}

#[derive(Debug, Default)]
pub struct AttachmentContext {
    pub text_blocks: Vec<String>,
    pub images: Vec<ImageInput>,
    pub skipped: Vec<String>,
}

impl AttachmentContext {
    pub fn append_text_to(&self, prompt: &mut String) {
        for block in &self.text_blocks {
            prompt.push_str("\n\n");
            prompt.push_str(block);
        }
    }

    pub fn accepted_count(&self) -> usize {
        self.text_blocks.len() + self.images.len()
    }
}

#[derive(Clone)]
pub struct AttachmentDownloader {
    enabled: bool,
    max_bytes: usize,
    max_images: usize,
    max_text_chars: usize,
    http: reqwest::Client,
}

impl AttachmentDownloader {
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(config.attachment_download_timeout)
            .build()
            .context("failed to build attachment HTTP client")?;

        Ok(Self {
            enabled: config.attachments_enabled,
            max_bytes: config.attachment_max_bytes,
            max_images: config.attachment_max_images,
            max_text_chars: config.attachment_max_text_chars,
            http,
        })
    }

    pub async fn collect(&self, attachments: &[Attachment]) -> AttachmentContext {
        let mut context = AttachmentContext::default();
        let mut remaining_text_chars = self.max_text_chars;

        if !self.enabled || attachments.is_empty() {
            return context;
        }

        info!(total = attachments.len(), "processing Discord attachments");

        for attachment in attachments {
            let kind = attachment_kind(attachment);
            let Some(kind) = kind else {
                self.skip(&mut context, attachment, "unsupported type");
                continue;
            };

            if attachment.size as usize > self.max_bytes {
                self.skip(&mut context, attachment, "too large");
                continue;
            }

            if matches!(kind, AttachmentKind::Image(_)) && context.images.len() >= self.max_images {
                self.skip(&mut context, attachment, "image limit reached");
                continue;
            }

            let bytes = match self.download(attachment).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    let reason = if err.to_string().contains("exceeds attachment limit") {
                        "too large"
                    } else {
                        "download failed"
                    };
                    warn!(
                        filename = %attachment.filename,
                        reason,
                        error = %err,
                        "skipping Discord attachment"
                    );
                    context
                        .skipped
                        .push(format!("{}: {reason}", attachment.filename));
                    continue;
                }
            };

            match kind {
                AttachmentKind::Image(mime) => {
                    context.images.push(ImageInput {
                        filename: attachment.filename.clone(),
                        mime,
                        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    });
                }
                AttachmentKind::Text => {
                    if remaining_text_chars == 0 {
                        self.skip(&mut context, attachment, "text limit reached");
                        continue;
                    }

                    let content = String::from_utf8_lossy(&bytes);
                    let original_chars = content.chars().count();
                    let accepted_chars = original_chars.min(remaining_text_chars);
                    let mut content = content.chars().take(accepted_chars).collect::<String>();
                    remaining_text_chars -= accepted_chars;
                    content = content.replace("```", "`` `");
                    if accepted_chars < original_chars {
                        content.push_str("\n[Attachment text truncated]");
                    }
                    let filename = sanitize_filename(&attachment.filename);
                    context.text_blocks.push(format!(
                        "[Attached file: {}]\n\n```text\n{}\n```",
                        filename, content
                    ));
                }
            }
        }

        info!(
            accepted_images = context.images.len(),
            accepted_text = context.text_blocks.len(),
            skipped = context.skipped.len(),
            "finished processing Discord attachments"
        );
        context
    }

    async fn download(&self, attachment: &Attachment) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(&attachment.url)
            .send()
            .await
            .with_context(|| format!("failed to download {}", attachment.filename))?
            .error_for_status()
            .with_context(|| format!("attachment download failed for {}", attachment.filename))?;

        if response
            .content_length()
            .is_some_and(|length| length > self.max_bytes as u64)
        {
            anyhow::bail!("response content length exceeds attachment limit");
        }

        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("failed to read {}", attachment.filename))?;
        if bytes.len() > self.max_bytes {
            anyhow::bail!("downloaded attachment exceeds attachment limit");
        }

        Ok(bytes.to_vec())
    }

    fn skip(&self, context: &mut AttachmentContext, attachment: &Attachment, reason: &'static str) {
        warn!(
            filename = %attachment.filename,
            reason,
            "skipping Discord attachment"
        );
        context
            .skipped
            .push(format!("{}: {reason}", attachment.filename));
    }
}

enum AttachmentKind {
    Image(String),
    Text,
}

fn attachment_kind(attachment: &Attachment) -> Option<AttachmentKind> {
    let mime = attachment
        .content_type
        .as_deref()
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);

    if let Some(mime) = mime.as_deref() {
        if IMAGE_MIME_TYPES.contains(&mime) {
            return Some(AttachmentKind::Image(mime.to_string()));
        }
        if mime.starts_with("text/") {
            return Some(AttachmentKind::Text);
        }
    }

    let extension = attachment
        .filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    if let Some(mime) = extension.as_deref().and_then(image_mime_for_extension) {
        return Some(AttachmentKind::Image(mime.to_string()));
    }
    if extension
        .as_deref()
        .is_some_and(|extension| TEXT_EXTENSIONS.contains(&extension))
    {
        return Some(AttachmentKind::Text);
    }

    None
}

fn image_mime_for_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn sanitize_filename(filename: &str) -> String {
    let filename = filename.replace(['\n', '\r', '\t'], " ");
    let filename = filename.split_whitespace().collect::<Vec<_>>().join(" ");
    filename.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::{image_mime_for_extension, sanitize_filename};

    #[test]
    fn sanitize_filename_removes_control_whitespace() {
        assert_eq!(sanitize_filename("hello\nworld.rs"), "hello world.rs");
    }

    #[test]
    fn image_extension_detection_is_case_normalized_by_caller() {
        assert_eq!(image_mime_for_extension("png"), Some("image/png"));
        assert_eq!(image_mime_for_extension("jpeg"), Some("image/jpeg"));
        assert_eq!(image_mime_for_extension("txt"), None);
    }
}
