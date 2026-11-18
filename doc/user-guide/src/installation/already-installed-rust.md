# Already installed Rust?

Other package managers also ship Rust. `rustup-init` copies the `rustup`
binary and leaves `PATH` and any existing `rustc` or `cargo` alone.

Invoke `rustup` by its path. Name a toolchain with `rust-toolchain.toml` or
a `+toolchain` argument. To use a packaged toolchain as a named rustup
toolchain, link it:

```console
rustup toolchain link system /usr
```

`system` is then a toolchain name, the same way `nightly` is. For example,
`cargo +system build` uses the linked install when that `cargo` is rustup,
and `cargo +nightly build` uses nightly.

To develop with the system toolchain, name it in `rust-toolchain.toml`:

```toml
[toolchain]
channel = "system"
```

A command with no `+toolchain` argument and no `rust-toolchain.toml` exits
with an error. The packaged `rustc` and `cargo` remain available at their
own paths.
