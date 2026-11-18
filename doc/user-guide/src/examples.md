# Examples

| Command                                                    | Description                                                          |
| ---------------------------------------------------------- | -------------------------------------------------------------------- |
| `channel = "nightly"` in `rust-toolchain.toml`             | Use the latest nightly for this directory                            |
| `rustup set profile minimal`                               | Set the default [profile]                                            |
| `rustup target list`                                       | List all available [targets] for the active toolchain                |
| `rustup target add arm-linux-androideabi`                  | Install the Android target                                           |
| `rustup target remove arm-linux-androideabi`               | Remove the Android target                                            |
| `rustup run nightly rustc foo.rs`                          | Run the nightly regardless of the active toolchain                   |
| `rustc +nightly foo.rs`                                    | [Shorthand] way to run a nightly compiler                            |
| `rustup run nightly bash`                                  | Run a shell configured for the nightly compiler                      |
| `rustup toolchain install stable-msvc`                    | On Windows, install the MSVC toolchain instead of GNU                |
| `channel = "nightly-2015-04-01"` in `rust-toolchain.toml`  | For the current directory, use a nightly from a specific date        |
| `rustup toolchain link my-toolchain "C:\RustInstallation"` | Install a custom toolchain by symlinking an existing installation    |
| `rustup show`                                              | Show which toolchain will be used in the current directory           |
| `rustup toolchain uninstall nightly`                       | Uninstall a given toolchain                                          |
| `rustup toolchain help`                                    | Show the `help` page for a subcommand (like `toolchain`)             |
| `rustup man cargo`                                         | \(_Unix only_\) View the man page for a given command (like `cargo`) |

[profile]: concepts/profiles.md
[shorthand]: overrides.md#toolchain-override-shorthand
[targets]: cross-compilation.md
