#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    # Non-login SSH shells often miss Cargo's PATH entry.
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

cargo build --release
sudo setcap 'cap_net_bind_service=+ep' target/release/makepad-web-server

if [ "$#" -eq 0 ]; then
    exec cargo run --release -- --port 80
fi

exec cargo run --release -- "$@"
