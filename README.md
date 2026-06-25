<p align="center">
  <img src="docs/logo.png" width="150" alt="tgdyk logo">
</p>

# tgdyk

![Version](https://img.shields.io/badge/version-0.1.2-blue)
![Rust](https://img.shields.io/badge/rust-1.88%2B-orange)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

Tiny local Telegram daemon for developers who want raw live TDLib updates
without running a full app.

Log in once. Keep one Telegram user session open. Stream live updates to local
tools as newline-delimited JSON.

`tgdyk` does not keep its own history or replay log, and it does not decide
what updates mean. TDLib still stores the local Telegram session/cache database.

## Quick Start

Download an archive from [Releases](https://github.com/pavel-voronin/tgdyk/releases).
It includes both `tgdyk` and TDLib `libtdjson`.

Pick the archive for your machine:

- Apple Silicon Mac: `tgdyk-*-aarch64-apple-darwin.tar.gz`
- Intel Mac: `tgdyk-*-x86_64-apple-darwin.tar.gz`
- Linux x64: `tgdyk-*-x86_64-unknown-linux-gnu.tar.gz`
- Linux arm64: `tgdyk-*-aarch64-unknown-linux-gnu.tar.gz`

Unpack it:

```sh
mkdir tgdyk
tar -xzf tgdyk-*.tar.gz -C tgdyk
cd tgdyk
```

Get Telegram API credentials from
[my.telegram.org/apps](https://my.telegram.org/apps), then log in:

```sh
./tgdyk setup
```

Run the daemon:

```sh
./tgdyk daemon
```

In another terminal, read live updates:

```sh
./tgdyk stream | jq .
```

Or print only new text messages:

```sh
./tgdyk stream | jq -r 'select(.["@type"] == "updateNewMessage") | .message.content.text.text // empty'
```

If something does not work:

```sh
./tgdyk doctor
```

## Commands

| Command | What it does |
| --- | --- |
| `setup` | Saves Telegram API credentials and creates the local TDLib session. |
| `daemon` | Runs TDLib and publishes live updates over a local Unix socket. |
| `stream` | Prints live raw TDLib updates as NDJSON. |
| `doctor` | Checks TDLib, config, paths, socket, and daemon connectivity. |

`setup` asks for `api_id`, `api_hash`, phone number, Telegram login code, and
2FA password when Telegram requires it.

The daemon stays in the foreground. Run it with systemd, supervisor, tmux, a
shell wrapper, or whatever you normally use to keep a server process alive.

`stream` is live-only. Start it before the Telegram activity you want to watch.

## Build from Source

Use this when the release archive does not match your host or when you want to
provide your own TDLib build.

Requires Rust 1.88+ and Unix.

```sh
cargo build --release --locked
```

Put `libtdjson` next to the binary:

```sh
cp /path/to/libtdjson.so target/release/          # Linux
cp /path/to/libtdjson.dylib target/release/       # macOS
target/release/tgdyk doctor
```

Or point to it explicitly:

```sh
TDJSON_PATH=/path/to/libtdjson.so target/release/tgdyk doctor
```

## Config

`setup` writes Telegram API credentials to:

```text
${XDG_CONFIG_HOME:-~/.config}/tgdyk/config.toml
```

Supported config keys:

```toml
api_id = 12345
api_hash = "your_api_hash"
```

Default paths:

```text
database: ${XDG_DATA_HOME:-~/.local/share}/tgdyk/tdlib/database
files:    ${XDG_DATA_HOME:-~/.local/share}/tgdyk/tdlib/files
socket:   $XDG_RUNTIME_DIR/tgdyk/tgdyk.sock
```

If `XDG_RUNTIME_DIR` is missing, the socket uses:

```text
${XDG_CACHE_HOME:-~/.cache}/tgdyk/run/tgdyk.sock
```

Environment overrides:

```text
TDJSON_PATH
TGDYK_CONFIG
```

## License

MIT.
