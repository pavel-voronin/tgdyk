# Maintainers

## Release

Build TDLib first with the `Build TDLib` workflow. It publishes assets and
matching `.sha256` files:

```text
tdlib-x86_64-unknown-linux-gnu.tar.gz
tdlib-aarch64-unknown-linux-gnu.tar.gz
tdlib-x86_64-apple-darwin.tar.gz
tdlib-aarch64-apple-darwin.tar.gz
```

Then tag a `tgdyk` release that matches `Cargo.toml`:

```sh
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
git tag "v$version"
git push origin "v$version"
```

The release workflow downloads the TDLib assets and publishes `tgdyk` archives
with `libtdjson` included.
