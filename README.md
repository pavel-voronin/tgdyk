# tgdyk

Tiny local Telegram daemon for developers who need raw live TDLib updates.

It keeps one Telegram user session open and streams updates to local tools. It
does not store history, replay missed updates, or decide what the updates mean.

Requires Rust 1.88+ and TDLib `libtdjson`.

```sh
cargo install --path .
tgdyk setup
tgdyk daemon
tgdyk stream
```

`tgdyk setup` asks for Telegram API credentials once and saves them locally. Use
`tgdyk doctor` when the local setup does not work.

Full notes: [DOCUMENTATION.md](DOCUMENTATION.md)

License: MIT.
