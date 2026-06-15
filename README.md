# Jetson Discord Bot

Rust Discord bot scaffold using `serenity` 0.12 and a local `llama.cpp` server.

## Commands

- `/ping`: bot latency smoke test
- `/llm-status`: checks the local model server
- `/ask prompt:<text>`: sends a prompt to `LLAMA_BASE_URL/v1/chat/completions`

## Run

```bash
cp .env.example .env
cargo run --release
```

All runtime settings are read from `.env`. Change `.env` instead of passing flags
to the run scripts.

For fast command updates during development, set `DISCORD_GUILD_ID`. Without it, commands are registered globally and can take a while to appear.

Set `DISCORD_CHAT_LISTENER=1` to let the bot read normal channel messages and
reply when mentioned by name. This requires Message Content Intent in the
Discord Developer Portal.

## Attachments

Attachment input is enabled by default for mentioned channel messages. Configure
the limits in `.env`:

```dotenv
ATTACHMENTS_ENABLED=1
ATTACHMENT_MAX_BYTES=8388608
ATTACHMENT_MAX_IMAGES=3
ATTACHMENT_MAX_TEXT_CHARS=12000
ATTACHMENT_DOWNLOAD_TIMEOUT_SECS=20
```

Supported images are PNG, JPEG, and WebP. Supported text attachments include
`.txt`, `.md`, `.rs`, `.py`, `.js`, `.ts`, `.json`, `.toml`, `.yaml`, `.yml`,
`.log`, `.csv`, and files with a `text/*` MIME type.

`ATTACHMENT_MAX_TEXT_CHARS` is the combined text budget for one Discord
message. Longer files are marked as truncated, and additional text files are
skipped after the budget is exhausted.

Manual Discord checks:

- Attach an image and mention the bot with `코미야 이 사진 설명해봐`.
- Attach a `.txt`, `.md`, or `.rs` file and ask the bot to summarize or review it.
- On mobile, upload the image or file first, then send `코미야 방금 올린 파일
  봐줘` in the same channel. The bot searches the recent
  `DISCORD_CONTEXT_MESSAGES` messages and uses only the latest attachment from
  the same Discord user ID.
- Replying to your own attachment message with `코미야 이거 봐줘` selects that
  replied-to attachment first. An attachment included with the current request
  always has the highest priority.
- You can explicitly select an attachment message from any author with
  `코미야 메시지 123456789012345678 읽어줘` or by including its Discord message
  link. Explicit selection works outside the recent-message window when the bot
  has permission to access that channel. Automatic recent-message selection
  still uses only the requester's own attachments.
- Explicitly selected messages are routed by content type. Plain text is
  summarized directly, web links are fetched, images use vision input, and
  supported documents use extracted text. Mixed messages combine all available
  inputs; when no specific instruction is given, the bot produces a concise
  summary and key points automatically.

Web search runs only for clear search intent such as `검색`, `인터넷`, `최신`,
`뉴스`, or a supplied URL. A plain attachment request such as
`이 문서 요약해줘` does not trigger web search. Search and linked-page failures
are logged and the bot continues with its normal model response.

Direct llama-server multimodal check:

```bash
IMG=/path/to/test.png
B64=$(base64 -w0 "$IMG")

curl http://192.168.100.14:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d "{
    \"model\":\"gemma-4-26b-a4b-it-q8\",
    \"messages\":[{
      \"role\":\"user\",
      \"content\":[
        {\"type\":\"text\",\"text\":\"이 이미지에 뭐가 보이는지 설명해.\"},
        {\"type\":\"image_url\",\"image_url\":{\"url\":\"data:image/png;base64,$B64\"}}
      ]
    }],
    \"max_tokens\":256,
    \"temperature\":0.2
  }"
```

## Development sandbox

Approved Discord users can use `/dev task:<work>` or mention the bot naturally,
for example `코미야 개발 컨테이너 써서 hello.py 만들고 실행해`. Commands run
only inside a persistent Podman container with no host project, credentials,
container socket, capabilities, or network access mounted into it.

Install Podman and build the image:

```bash
sudo apt-get update
sudo apt-get install -y podman
./scripts/build-dev-sandbox.sh
```

Then configure `.env`:

```dotenv
DEV_SANDBOX_ENABLED=1
DEV_SANDBOX_ALLOWED_USER_IDS=1163691482415382669
DEV_SANDBOX_RUNTIME=podman
DEV_SANDBOX_IMAGE=localhost/komi-dev:latest
DEV_SANDBOX_CONTAINER=komi-dev
DEV_SANDBOX_WORKSPACE=komi_workspace
DEV_SANDBOX_TIMEOUT_SECS=60
DEV_SANDBOX_MAX_STEPS=8
DEV_SANDBOX_OUTPUT_CHARS=6000
```

The default image includes Rust, C/C++, Python, Node.js, Git, curl, jq,
ripgrep, CMake, and ShellCheck. The container filesystem is read-only except
for its `/workspace` volume and temporary directory.

Run an end-to-end task locally without Discord:

```bash
./target/release/jetson-discord-bot --dev-task \
  "Create a small program, run its tests, and summarize the result."
```

On Windows without Visual C++ Build Tools, use the GNU toolchain and native TLS for local checks:

```powershell
cargo +stable-x86_64-pc-windows-gnu check --no-default-features --features tls-native
```

On Jetson/Linux, keep the default `tls-rustls` feature:

```bash
cargo run --release
```

## Jetson llama-server

```bash
./scripts/run-llama.sh
```

## Run the bot on Jetson

```bash
./scripts/run-bot.sh
```
