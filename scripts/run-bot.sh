#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

if [ -f ".env" ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"

    case "$line" in
      ""|\#*) continue ;;
    esac

    if [[ "$line" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]]; then
      export "$line"
    else
      echo "[bot] warning: ignoring invalid .env line: $line"
    fi
  done < ".env"
else
  echo "[bot] warning: .env not found in $(pwd)"
fi

echo "[bot] LLAMA_BASE_URL=${LLAMA_BASE_URL:-<unset>}"
echo "[bot] LLAMA_MODEL=${LLAMA_MODEL:-<unset>}"

exec ./target/release/jetson-discord-bot
