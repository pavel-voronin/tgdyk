# tgdyk

`tgdyk` is a small Telegram daemon built on TDLib.

It owns one Telegram user session and exposes the live TDLib update stream to
local processes. It does not store, filter, transform, or replay updates.

## Build

```sh
cargo build
cargo install --path .
```

`tgdyk` needs `libtdjson`.

Set `TDJSON_PATH` only when `libtdjson` is not next to the `tgdyk` binary and not
available to the system loader:

```sh
export TDJSON_PATH=/path/to/libtdjson.so
```

Release archives can ship `libtdjson` next to the `tgdyk` binary.

## Commands

```sh
tgdyk setup
tgdyk daemon
tgdyk stream
tgdyk doctor
```

### `tgdyk setup`

Asks for Telegram API credentials once, stores them in the local config, then
runs Telegram authentication and stores the TDLib session under the user's XDG
data directory.

### `tgdyk daemon`

Starts the local Telegram client and broadcasts live TDLib updates to connected
local clients.

The daemon requires an existing authenticated TDLib session. If the session is
missing, run `tgdyk setup`.

### `tgdyk stream`

Connects to the daemon and writes live TDLib updates to stdout:

```sh
tgdyk stream | jq .
```

Output is newline-delimited raw TDLib JSON.

### `tgdyk doctor`

Checks TDLib, Telegram API credentials, the saved session, the files directory,
the daemon socket, and daemon connectivity.

## Config

Config file:

```text
$XDG_CONFIG_HOME/tgdyk/config.toml
```

Supported keys:

```toml
api_id = 12345
api_hash = "your_api_hash"
database_dir = "/path/to/database"
files_dir = "/path/to/files"
socket_path = "/path/to/tgdyk.sock"
```

Runtime path overrides are also supported:

```text
TDJSON_PATH
TDLIB_DATABASE_DIR
TDLIB_FILES_DIR
TGDYK_SOCKET_PATH
```

## Runtime Paths

Default XDG paths:

```text
$XDG_DATA_HOME/tgdyk/tdlib/database
$XDG_DATA_HOME/tgdyk/tdlib/files
$XDG_RUNTIME_DIR/tgdyk/tgdyk.sock
```

If `XDG_RUNTIME_DIR` is missing, the socket falls back to the system temp
directory.

## Security

`tgdyk` leaves TDLib database protection to the user's OS account and filesystem permissions.

## Stream Contract

The stream is live-only.

An update is delivered only to clients connected at the time the daemon receives
it. If no client is connected, the update is discarded.

Slow clients are disconnected instead of receiving a partial stream.

The public stream format is raw TDLib NDJSON:

```text
one line = one TDLib JSON update
```

## License

MIT.
