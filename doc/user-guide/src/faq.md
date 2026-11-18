# FAQ

### Is this an official Rust project?

Yes. rustup is an official Rust project. It is the recommended way to install
Rust at https://www.rust-lang.org.

### How is this related to multirust?

rustup is the successor to [multirust]. rustup began as multirust-rs, a
rewrite of multirust from shell script to Rust, by [Diggory Blake], and is now
maintained by The Rust Project.

[multirust]: https://github.com/brson/multirust
[Diggory Blake]: https://github.com/Diggsey

### Can rustup download the Rust source code?

The source for Rust's standard library can be obtained by running `rustup
component add rust-src`. It will be downloaded to the `<toolchain
root>/lib/rustlib/src/rust` directory of the current toolchain.

The source for the compiler and tools must be obtained from the [Rust
repository] or the standalone [source tarballs].

[rust repository]: https://github.com/rust-lang/rust/
[source tarballs]: https://forge.rust-lang.org/infra/other-installation-methods.html#source-code

### rustup fails with Windows error 32

If `rustup` fails with Windows error 32, it may be due to antivirus scanning
in the background. Disable antivirus scanner and try again.

### I get "error: could not remove 'rustup-bin' file: 'C:\Users\USER\\.cargo\bin\rustup.exe'"

If `rustup self uninstall` fails to remove `rustup.exe` this way, another
program has that file open. Close that program and try again.

### rustup exited successfully but I can't run `rustc --version`

`rustup` does not put toolchain binaries on `PATH`. Name a toolchain in
`rust-toolchain.toml`, or run `rustup +nightly run rustc --version` or
`rustup run nightly rustc --version`. The toolchain binary can also be
invoked by its path.
