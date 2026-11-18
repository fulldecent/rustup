# Basic usage

## Keeping Rust up to date

Rust is distributed on three different [release channels]: stable, beta, and
nightly. `rustup` uses the stable channel by default, which
represents the latest release of Rust. Stable publishes new releases every six weeks.

[release channels]: concepts/channels.md

When a new version of Rust is released, simply type `rustup update` to update:

```console
$ rustup update
info: syncing channel updates for 'stable'
info: downloading component 'rustc'
info: downloading component 'rust-std'
info: downloading component 'rust-docs'
info: downloading component 'cargo'
info: installing component 'rustc'
info: installing component 'rust-std'
info: installing component 'rust-docs'
info: installing component 'cargo'

  stable updated: rustc 1.7.0 (a5d1e7a59 2016-02-29)

```

## Help system

The `rustup` command-line has a built-in help system that provides more
information about each command. Run `rustup help` for an overview. Detailed
help for each subcommand is also available. For example, run `rustup toolchain
install --help` for specifics on installing [toolchains].

[toolchains]: concepts/toolchains.md
