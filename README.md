# tgdyk

Tiny local Telegram daemon for developers who need raw live TDLib updates.

It keeps one Telegram user session open and streams updates to local tools. It
does not store history, replay missed updates, or decide what the updates mean.

`tgdyk` is distributed as GitHub release archives, not through crates.io.
Release archives include both the `tgdyk` binary and TDLib `libtdjson`.

## Install from Release

Download the archive for your server from:

https://github.com/pavel-voronin/tgdyk/releases

Linux archives contain `libtdjson.so`; macOS archives contain
`libtdjson.dylib`. Keep that library next to the `tgdyk` binary.

```sh
mkdir tgdyk
tar -xzf tgdyk-*.tar.gz -C tgdyk
cd tgdyk
```

Get Telegram API credentials from https://my.telegram.org/apps, then run:

```sh
./tgdyk setup
```

`setup` asks for `api_id`, `api_hash`, phone number, Telegram login code, and
2FA password when Telegram requires it.

Run the daemon:

```sh
./tgdyk daemon
```

The daemon stays in the foreground. Run it however you normally keep a server
process alive: systemd, supervisor, tmux, shell wrapper, or a foreground session.

In another terminal, read live updates:

```sh
./tgdyk stream
```

`stream` prints newline-delimited raw TDLib JSON. It is live-only: missed updates
are not replayed. Pipe it to `jq .` if you want pretty output.

Use `doctor` when setup or streaming does not work:

```sh
./tgdyk doctor
```

## Build from Source

Use this when the release archive does not match your host or when you want to
provide your own TDLib build. Requires Rust 1.88+ and Unix.

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
$XDG_CONFIG_HOME/tgdyk/config.toml
```

Supported config keys:

```toml
api_id = 12345
api_hash = "your_api_hash"
database_dir = "/path/to/database"
files_dir = "/path/to/files"
socket_path = "/path/to/tgdyk.sock"
```

Default runtime paths:

```text
$XDG_DATA_HOME/tgdyk/tdlib/database
$XDG_DATA_HOME/tgdyk/tdlib/files
$XDG_RUNTIME_DIR/tgdyk/tgdyk.sock
```

If `XDG_RUNTIME_DIR` is missing, the socket uses
`$XDG_CACHE_HOME/tgdyk/run/tgdyk.sock`.

Environment overrides:

```text
TDJSON_PATH
TDLIB_DATABASE_DIR
TDLIB_FILES_DIR
TGDYK_SOCKET_PATH
TGDYK_CONFIG
```

## Release Maintainers

Build TDLib first with the `Build TDLib` workflow. It publishes assets and
matching `.sha256` files like:

```text
tdlib-x86_64-unknown-linux-gnu.tar.gz
tdlib-aarch64-unknown-linux-gnu.tar.gz
tdlib-x86_64-apple-darwin.tar.gz
tdlib-aarch64-apple-darwin.tar.gz
```

Then tag a `tgdyk` release that matches `Cargo.toml`:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow downloads the TDLib assets and publishes `tgdyk` archives
with `libtdjson` included.

## License

MIT.
