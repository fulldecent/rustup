//! The big installation messages. These are macros because the first
//! argument of format! needs to be a literal.

macro_rules! pre_install_msg {
    () => {
        r"
# Welcome to Rust!

This copies the rustup binary into place and records install metadata.

Rustup metadata will be installed into the rustup home directory,
located at:

    {rustup_home}

This can be modified with the `RUSTUP_HOME` environment variable.

The cargo home directory is located at:

    {cargo_home}

This can be modified with the `CARGO_HOME` environment variable.

The rustup binary will be copied to:

    {cargo_bin_dir}

Invoke rustup by that path. Install a toolchain afterwards with
`rustup toolchain install`, then name it in `rust-toolchain.toml`
or pass a `+toolchain` argument.

You can uninstall at any time with `rustup self uninstall`.
"
    };
}

macro_rules! post_install_msg {
    () => {
        r"# rustup is installed

The rustup binary is at `{cargo_bin_dir}`.

Install a toolchain with `rustup toolchain install`, then name it
in `rust-toolchain.toml` or pass a `+toolchain` argument.
"
    };
}

macro_rules! pre_uninstall_msg {
    () => {
        r"# Thanks for hacking in Rust!

This will uninstall all Rust toolchains and data, and remove
the rustup binary.

"
    };
}
