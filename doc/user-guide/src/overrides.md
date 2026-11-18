# Overrides

A rustup command selects a [toolchain] in this order:

1. An in-line specification on that command, such as `rustup +nightly run cargo build` or `rustup run nightly cargo build`. See [toolchain override shorthand].
2. The nearest [`rust-toolchain.toml`] file.

`rust-toolchain.toml` is discovered by walking from the current
directory toward the filesystem root, and the nearest file wins. If neither
is present, the command fails.

`rustup show` prints the selected toolchain name. `rustup resolve` is the same command. If neither mechanism is present, the command exits with an error.

[toolchain]: concepts/toolchains.md
[toolchain override shorthand]: #toolchain-override-shorthand
[`rust-toolchain.toml`]: #the-toolchain-file

## Toolchain override shorthand

An in-line toolchain on a rustup command looks like this:

```console
rustup +beta run cargo test
rustup run beta cargo test
```

The rustup binary can also be invoked under another name, such as `cargo` or
`rustc`. A leading `+toolchain` argument is recognized there too:

```console
cargo +beta test
```

That name is a shim. A shim is not recommended. The installer does not create
shims, and these instructions do not ask you to put a shim on `PATH`.

## The toolchain file

Some projects find themselves 'pinned' to a specific release of Rust and want
this information reflected in their source repository. This is most often the
case for nightly-only software that pins to a revision from the release
archives.

In these cases the toolchain can be named in the project's directory in a file
called `rust-toolchain.toml`. The file uses the
[TOML] format and has the following layout:

[TOML]: https://toml.io/

```toml
[toolchain]
channel = "nightly-2020-07-10"
components = [ "rustfmt", "rustc-dev" ]
targets = [ "wasm32-unknown-unknown", "thumbv2-none-eabi" ]
profile = "minimal"
```

The `[toolchain]` section is mandatory, and at least one property must be
specified. `channel` and `path` are mutually exclusive.

A plain-text `rust-toolchain` file is not read. The only file pin is
`rust-toolchain.toml`.

The `rust-toolchain.toml` file is suitable to check in to
source control. If that's done, `Cargo.lock` should probably be tracked too if
the toolchain is pinned to a specific release, to avoid potential compatibility
issues with dependencies.

### Toolchain file settings

#### channel

The `channel` setting specifies which [toolchain] to use. The value is a
string in the following form:

```
(<channel>[-<date>])|<custom toolchain name>

<channel>       = stable|beta|nightly|<versioned>[-<prerelease>]
<versioned>     = <major.minor>|<major.minor.patch>
<prerelease>    = beta[.<number>]
<date>          = YYYY-MM-DD
```

[toolchain]: concepts/toolchains.md

#### path

The `path` setting allows a custom toolchain to be used. The value is an
absolute path string.

Since a `path` directive directly names a local toolchain, other options
like `components`, `targets`, and `profile` have no effect.

`channel` and `path` are mutually exclusive, since a `path` already
points to a specific toolchain.

#### profile

The `profile` setting names a group of components to be installed. The
value is a string. The valid options are: `minimal`, `default`, and
`complete`. See [profiles] for details of each.

Note that if not specified, the `default` profile is not necessarily
used, as a different default profile might have been set with `rustup
set profile`.

[profiles]: concepts/profiles.md

#### components

The `components` setting contains a list of additional components to
install. The value is a list of strings. See [components] for a list of
components. Note that different toolchains may have different components
available.

The components listed here are additive with the current profile.

[components]: concepts/components.md

#### targets

The `targets` setting contains a list of platforms to install for
[cross-compilation]. The value is a list of strings.

The host platform is automatically included; the targets listed here are
additive.

[cross-compilation]: https://rust-lang.github.io/rustup/cross-compilation.html

If neither an in-line `+toolchain` argument nor a `rust-toolchain.toml` file is
present, rustup exits with an error. Install a toolchain with `rustup toolchain
install`, then name it in `rust-toolchain.toml`, or pass `+toolchain` on the
command.
