# Installation

Follow the instructions at <https://www.rust-lang.org/tools/install>. If that
doesn't work for you there are [other installation methods].

`rustup-init` copies the `rustup` binary to `$HOME/.cargo/bin/rustup` on
Unix and to `%USERPROFILE%\.cargo\bin\rustup.exe` on Windows. The installer
leaves `PATH` unchanged and does not download a toolchain.

Invoke that executable, install a toolchain, and name it:

```console
rustup toolchain install stable
```

```toml
[toolchain]
channel = "stable"
```

A command also accepts an in-line toolchain, such as
`rustup run stable cargo build`. Remove the install with
`rustup self uninstall`.

[other installation methods]: other.md

## Choosing where to install

`rustup` allows you to customise your installation by setting the environment
variables `CARGO_HOME` and `RUSTUP_HOME` before running the `rustup-init`
executable. As mentioned in the [Environment Variables] section, `RUSTUP_HOME`
sets the root `rustup` folder, which is used for storing installed toolchains
and configuration options. `CARGO_HOME` contains cache files used by [cargo].

Keep these environment variables set for later `rustup` commands. The
installer does not add `CARGO_HOME/bin` to `PATH`.

[Environment Variables]: ../environment-variables.md
[cargo]: https://doc.rust-lang.org/cargo/

## Installing nightly

`rustup-init` does not download a toolchain. Install the channel you want
afterwards. To install `nightly` along with `clippy` or `miri`, and allow
rustup to downgrade until those [components] exist:

```console
rustup toolchain install nightly --allow-downgrade --profile minimal --component clippy
```

This can be used in CI to get a toolchain which meets your criteria.

[components]: ../concepts/components.md

## Enable tab completion for Bash, Fish, Zsh, or PowerShell

`rustup` now supports generating completion scripts for Bash, Fish, Zsh, and
PowerShell. See `rustup help completions` for full details, but the gist is as
simple as using one of the following:

```console
# Bash
$ rustup completions bash > ~/.local/share/bash-completion/completions/rustup

# Bash (macOS/Homebrew)
$ rustup completions bash > $(brew --prefix)/etc/bash_completion.d/rustup.bash-completion

# Fish
$ mkdir -p ~/.config/fish/completions
$ rustup completions fish > ~/.config/fish/completions/rustup.fish

# Zsh
$ rustup completions zsh > ~/.zfunc/_rustup

# PowerShell v5.0+
$ rustup completions powershell >> $PROFILE.CurrentUserCurrentHost
# or
$ rustup completions powershell | Out-String | Invoke-Expression
```

**Note**: you may need to restart your shell in order for the changes to take
effect.

For `zsh`, you must then add the following line in your `~/.zshrc` before
`compinit`:

```zsh
fpath+=~/.zfunc
```

In Xonsh you can reuse Fish completion by installing [xontrib-fish-completer](https://github.com/xonsh/xontrib-fish-completer).
