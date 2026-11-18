# Concepts

## How rustup works

`rustup` installs and manages many Rust toolchains. A command selects one
toolchain in this order:

1. A `+toolchain` argument, as in `rustup run nightly cargo build`.
2. The nearest `rust-toolchain.toml`, walking from the current directory
   toward the filesystem root.

If neither is present, the command exits with an error.

Run a tool with `rustup run`, for example `rustup run cargo build` or
`rustup run rustc --version`. `rustup run` installs a missing channel from
that selection and puts the toolchain's binaries and libraries on the child
process path.

The installer copies only the `rustup` binary. If that binary is invoked
under another name, such as [`rustc`] or [`cargo`], the invocation is a
[proxy][proxies]: it uses the same two rules, then runs the matching binary from
the selected toolchain. Those names exist only when you create them.

This is similar to Ruby's [rbenv], Python's [pyenv], or Node's [nvm].

[rbenv]: https://github.com/rbenv/rbenv
[pyenv]: https://github.com/yyuu/pyenv
[nvm]: https://github.com/creationix/nvm
[`rustc`]: https://doc.rust-lang.org/rustc/
[`cargo`]: https://doc.rust-lang.org/cargo/
[proxies]: proxies.md

## Terminology

- **channel** --- Rust is released to three different "channels": stable, beta,
  and nightly. See the [Channels] chapter for more details.

- **toolchain** --- A "toolchain" is a complete installation of the Rust
  compiler (`rustc`) and related tools (like `cargo`). A [toolchain
  specification] includes the release channel or version, and the host
  platform that the toolchain runs on.

- **target** --- `rustc` is capable of generating code for many platforms. The
  "target" specifies the platform that the code will be generated for. By
  default, `cargo` and `rustc` use the host toolchain's platform as the
  target. To build for a different target, usually the target's standard
  library needs to be installed first via the `rustup target` command. See the
  [Cross-compilation] chapter for more details.

- **component** --- Each release of Rust includes several "components", some of
  which are required (like `rustc`) and some that are optional (like
  [`clippy`]). See the [Components] chapter for more detail.

- **profile** --- In order to make it easier to work with components, a
  "profile" defines a grouping of components. See the [Profiles] chapter for
  more details.

- **proxy** --- The rustup binary invoked under the name of a Rust tool, such as
  `rustc` or `cargo`. It forwards that command to the selected toolchain. See
  the [Proxies] chapter for the names rustup recognizes.

[`clippy`]: https://github.com/rust-lang/rust-clippy
[components]: components.md
[cross-compilation]: ../cross-compilation.md
[profiles]: profiles.md
[toolchain specification]: toolchains.md
[channels]: channels.md
[proxies]: proxies.md
