//! Testing install and uninstall of the rustup binary.

use std::{env::consts::EXE_SUFFIX, fs, path::Path, process::Command};

#[cfg(windows)]
use retry::{
    delay::{Fibonacci, jitter},
    retry,
};
use rustup::{
    DUP_TOOLS, TOOLS,
    test::{CROSS_ARCH1, CliTestContext, Scenario},
};

/// Empty dist server, rustup installed with no toolchain
async fn setup_empty_installed() -> CliTestContext {
    let cx = CliTestContext::new(Scenario::Empty).await;
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--no-modify-path",
            "--default-toolchain",
            "none",
        ])
        .await
        .is_ok();
    cx
}

/// SimpleV3 dist server, rustup installed with default toolchain
async fn setup_installed() -> CliTestContext {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;
    cx.config
        .expect(["rustup-init", "-y", "--no-modify-path"])
        .await
        .is_ok();
    cx
}

/// The installer copies the rustup binary and records profile and host.
/// It does not download a toolchain or create proxy names.
#[tokio::test]
async fn install_bins_to_cargo_home() {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;

    cx.config
        .expect(["rustup-init", "-y"])
        .await
        .with_stdout(snapbox::str![[r#"
...
rustup is installed
...
"#]])
        .with_stderr(snapbox::str![[r#"
...
info: profile set to default
info: default host tuple is [HOST_TUPLE]

"#]])
        .is_ok();
    #[cfg(windows)]
    fn check(path: &Path) {
        assert!(path.exists());
    }
    #[cfg(not(windows))]
    fn check(path: &Path) {
        fn is_exe(path: &Path) -> bool {
            use std::os::unix::fs::MetadataExt;
            let mode = path.metadata().unwrap().mode();
            mode & 0o777 == 0o755
        }
        assert!(is_exe(path));
    }

    let rustup = cx.config.cargodir.join(format!("bin/rustup{EXE_SUFFIX}"));
    check(&rustup);
    for tool in TOOLS.iter().chain(DUP_TOOLS.iter()) {
        let path = cx.config.cargodir.join(format!("bin/{tool}{EXE_SUFFIX}"));
        assert!(!path.exists(), "{}", path.display());
    }
}

#[tokio::test]
async fn install_twice() {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;

    cx.config.expect(["rustup-init", "-y"]).await.is_ok();
    cx.config.expect(["rustup-init", "-y"]).await.is_ok();
    let rustup = cx.config.cargodir.join(format!("bin/rustup{EXE_SUFFIX}"));
    assert!(rustup.exists());
}

/// Smoke test for the entire install process when dirs need to be made :
/// depending just on unit tests here could miss subtle dependencies being added
/// earlier in the code, so a black-box test is needed.
#[tokio::test]
async fn install_creates_cargo_home() {
    let cx = CliTestContext::new(Scenario::Empty).await;
    fs::remove_dir_all(&cx.config.cargodir).unwrap();
    cx.config.rustupdir.remove().unwrap();
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--no-modify-path",
            "--default-toolchain",
            "none",
        ])
        .await
        .is_ok();
    assert!(cx.config.cargodir.exists());
}

/// Functional test needed here - we need to do the full dance where we start
/// with rustup.exe and end up deleting that exe itself.
#[tokio::test]
async fn uninstall_deletes_bins() {
    let cx = setup_empty_installed().await;
    // no-modify-path isn't needed here, as the test-dir-path isn't present
    // in the registry, so the no-change code path will be triggered.
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .is_ok();
    let rustup = cx.config.cargodir.join(format!("bin/rustup{EXE_SUFFIX}"));
    let rustc = cx.config.cargodir.join(format!("bin/rustc{EXE_SUFFIX}"));
    let rustdoc = cx.config.cargodir.join(format!("bin/rustdoc{EXE_SUFFIX}"));
    let cargo = cx.config.cargodir.join(format!("bin/cargo{EXE_SUFFIX}"));
    let rust_lldb = cx
        .config
        .cargodir
        .join(format!("bin/rust-lldb{EXE_SUFFIX}"));
    let rust_gdb = cx.config.cargodir.join(format!("bin/rust-gdb{EXE_SUFFIX}"));
    let rust_gdbgui = cx
        .config
        .cargodir
        .join(format!("bin/rust-gdbgui{EXE_SUFFIX}"));
    assert!(!rustup.exists());
    assert!(!rustc.exists());
    assert!(!rustdoc.exists());
    assert!(!cargo.exists());
    assert!(!rust_lldb.exists());
    assert!(!rust_gdb.exists());
    assert!(!rust_gdbgui.exists());
}

#[tokio::test]
async fn uninstall_deletes_installed_toolchains() {
    let cx = setup_empty_installed().await;
    let path = cx.config.customdir.join("custom-1");
    let path = path.to_string_lossy();
    cx.config
        .expect(["rustup", "toolchain", "link", "custom", &path])
        .await
        .is_ok();
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .with_stderr(snapbox::str![[r#"
...
info: uninstalling toolchain custom
info: toolchain custom uninstalled
...
"#]])
        .is_ok();
    assert!(!cx.config.rustupdir.join("toolchains").exists());
}

#[tokio::test]
async fn uninstall_works_if_some_bins_dont_exist() {
    let cx = setup_empty_installed().await;
    let rustup = cx.config.cargodir.join(format!("bin/rustup{EXE_SUFFIX}"));
    let rustc = cx.config.cargodir.join(format!("bin/rustc{EXE_SUFFIX}"));
    let rustdoc = cx.config.cargodir.join(format!("bin/rustdoc{EXE_SUFFIX}"));
    let cargo = cx.config.cargodir.join(format!("bin/cargo{EXE_SUFFIX}"));
    let rust_lldb = cx
        .config
        .cargodir
        .join(format!("bin/rust-lldb{EXE_SUFFIX}"));
    let rust_gdb = cx.config.cargodir.join(format!("bin/rust-gdb{EXE_SUFFIX}"));
    let rust_gdbgui = cx
        .config
        .cargodir
        .join(format!("bin/rust-gdbgui{EXE_SUFFIX}"));

    // A name that is not the rustup binary stays. Proxy names are absent.
    fs::write(&rustc, "not rustup").unwrap();

    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .is_ok();

    assert!(!rustup.exists());
    assert!(rustc.exists());
    assert!(!rustdoc.exists());
    assert!(!cargo.exists());
    assert!(!rust_lldb.exists());
    assert!(!rust_gdb.exists());
    assert!(!rust_gdbgui.exists());
}

#[tokio::test]
async fn uninstall_deletes_rustup_home() {
    let cx = setup_empty_installed().await;
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .is_ok();
    assert!(!cx.config.rustupdir.has("."));
}

#[tokio::test]
async fn uninstall_works_if_rustup_home_doesnt_exist() {
    let cx = setup_empty_installed().await;
    cx.config.rustupdir.remove().unwrap();
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .is_ok();
}

#[tokio::test]
async fn uninstall_deletes_cargo_home() {
    let cx = setup_empty_installed().await;
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .is_ok();
    assert!(!cx.config.cargodir.exists());
}

#[tokio::test]
async fn complete_uninstall_removes_empty_cargo_bin() {
    let cx = setup_empty_installed().await;
    let cargo_bin = cx.config.cargodir.join("bin");
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .is_ok();

    #[cfg(unix)]
    assert!(!cargo_bin.exists());

    #[cfg(windows)]
    retry(Fibonacci::from_millis(1).map(jitter).take(23), || {
        if cargo_bin.exists() {
            return Err(format!("cargo bin still exists: {}", cargo_bin.display()));
        }
        Ok(())
    })
    .unwrap()
}

#[tokio::test]
async fn complete_uninstall_keeps_non_empty_cargo_bin() {
    let cx = setup_empty_installed().await;
    let cargo_bin = cx.config.cargodir.join("bin");

    let mock_file = cargo_bin.join("custom-tool");
    fs::write(&mock_file, "").unwrap();

    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .with_stderr(snapbox::str![[r#"
...
warn: keeping non-empty cargo bin directory `[..]`
...
"#]])
        .is_ok();
    assert!(cargo_bin.exists());
    assert!(mock_file.exists());
}

#[tokio::test]
async fn uninstall_fails_if_not_installed() {
    let cx = setup_empty_installed().await;
    let rustup = cx.config.cargodir.join(format!("bin/rustup{EXE_SUFFIX}"));
    fs::remove_file(rustup).unwrap();
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .with_stderr(snapbox::str![[r#"
error: rustup is not installed at '[..]'

"#]])
        .is_err();
}

// The other tests here just run rustup from a temp directory. This
// does the uninstall by actually invoking the installed binary in
// order to test that it can successfully delete itself.
#[tokio::test]
#[cfg_attr(target_os = "macos", ignore)] // FIXME #1515
async fn uninstall_self_delete_works() {
    let cx = setup_empty_installed().await;
    let rustup = cx.config.cargodir.join(format!("bin/rustup{EXE_SUFFIX}"));
    let mut cmd = Command::new(rustup.clone());
    cmd.args(["self", "uninstall", "-y"]);
    cx.config.env(&mut cmd);
    let out = cmd.output().unwrap();
    println!("out: {}", String::from_utf8(out.stdout).unwrap());
    println!("err: {}", String::from_utf8(out.stderr).unwrap());

    assert!(out.status.success());
    assert!(!rustup.exists());
    assert!(!cx.config.cargodir.exists());

    let rustc = cx.config.cargodir.join(format!("bin/rustc{EXE_SUFFIX}"));
    let rustdoc = cx.config.cargodir.join(format!("bin/rustdoc{EXE_SUFFIX}"));
    let cargo = cx.config.cargodir.join(format!("bin/cargo{EXE_SUFFIX}"));
    let rust_lldb = cx
        .config
        .cargodir
        .join(format!("bin/rust-lldb{EXE_SUFFIX}"));
    let rust_gdb = cx.config.cargodir.join(format!("bin/rust-gdb{EXE_SUFFIX}"));
    let rust_gdbgui = cx
        .config
        .cargodir
        .join(format!("bin/rust-gdbgui{EXE_SUFFIX}"));
    assert!(!rustc.exists());
    assert!(!rustdoc.exists());
    assert!(!cargo.exists());
    assert!(!rust_lldb.exists());
    assert!(!rust_gdb.exists());
    assert!(!rust_gdbgui.exists());
}

// On windows rustup self uninstall temporarily puts a rustup-gc-$randomnumber.exe
// file in CONFIG.CARGODIR/.. ; check that it doesn't exist.
#[tokio::test]
#[cfg(windows)]
async fn uninstall_doesnt_leave_gc_file() {
    let cx = setup_empty_installed().await;
    cx.config
        .expect(["rustup", "self", "uninstall", "-y"])
        .await
        .is_ok();
    let parent = cx.config.cargodir.parent().unwrap();

    // The gc removal happens after rustup terminates. Typically under
    // 100ms, but during the contention of test suites can be substantially
    // longer while still succeeding.

    let check = || {
        let garbage = fs::read_dir(parent)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name()?.to_str()?;
                // On Windows, this binary is cleaned up on exit
                if !(name.starts_with("rustup-gc-") && name.ends_with(EXE_SUFFIX)) {
                    return None;
                }
                Some(path.to_string_lossy().to_string())
            })
            .collect::<Vec<_>>();
        if garbage.is_empty() {
            Ok(())
        } else {
            Err(format!("garbage remaining: {garbage:?}"))
        }
    };
    match retry(Fibonacci::from_millis(1).map(jitter).take(23), check) {
        Ok(_) => (),
        Err(e) => panic!("{e}"),
    }
}

// The installer used to be called rustup-setup. For compatibility it
// still needs to work in that mode.
#[tokio::test]
async fn as_rustup_setup() {
    let cx = CliTestContext::new(Scenario::Empty).await;
    cx.config
        .expect_with_env(
            [
                "rustup-init",
                "-y",
                "--no-modify-path",
                "--default-toolchain",
                "none",
            ],
            [("RUSTUP_FORCE_ARG0", &*format!("rustup-setup{EXE_SUFFIX}"))],
        )
        .await
        .is_ok();
}

#[tokio::test]
async fn reinstall_exact() {
    let cx = setup_empty_installed().await;
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--no-update-default-toolchain",
            "--no-modify-path",
        ])
        .await
        .is_ok();
}

#[tokio::test]
async fn reinstall_specifying_toolchain() {
    let cx = setup_installed().await;
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--default-toolchain=stable",
            "--no-modify-path",
        ])
        .await
        .is_ok();
    cx.config
        .expect(["rustup", "toolchain", "list"])
        .await
        .with_stdout(snapbox::str![""])
        .is_ok();
}

#[tokio::test]
async fn reinstall_specifying_component() {
    let cx = setup_installed().await;
    cx.config
        .expect(["rustup", "toolchain", "install", "stable"])
        .await
        .is_ok();
    cx.config.pin("stable");
    cx.config
        .expect(["rustup", "component", "add", "rls"])
        .await
        .is_ok();
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--default-toolchain=stable",
            "--no-modify-path",
        ])
        .await
        .is_ok();
    cx.config
        .expect(["rustup", "component", "list", "--installed"])
        .await
        .with_stdout(snapbox::str![[r#"
...
rls-[HOST_TUPLE]
...
"#]])
        .is_ok();
}

#[tokio::test]
async fn reinstall_specifying_different_toolchain() {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--default-toolchain=nightly",
            "--no-modify-path",
        ])
        .await
        .is_ok();
    cx.config
        .expect(["rustup", "toolchain", "list"])
        .await
        .with_stdout(snapbox::str![""])
        .is_ok();
}

#[tokio::test]
async fn install_sets_up_stable_unless_a_different_default_is_requested() {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--default-toolchain",
            "nightly",
            "--no-modify-path",
        ])
        .await
        .is_ok();
    cx.config
        .expect(["rustup", "run", "nightly", "rustc", "--version"])
        .await
        .with_stderr(snapbox::str![[r#"
error: toolchain 'nightly-[HOST_TUPLE]' is not installed
help: run `rustup toolchain install nightly-[HOST_TUPLE]` to install it

"#]])
        .is_err();
}

#[tokio::test]
async fn install_sets_up_stable_unless_there_is_already_a_default() {
    let cx = setup_installed().await;
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--default-toolchain",
            "nightly",
            "--no-modify-path",
        ])
        .await
        .is_ok();
    cx.config.pin("nightly");
    cx.config
        .expect(["rustup", "toolchain", "remove", "stable"])
        .await
        .is_ok();
    cx.config
        .expect(["rustup-init", "-y", "--no-modify-path"])
        .await
        .is_ok();
    cx.config
        .expect(["rustc", "--version"])
        .await
        .with_stdout(snapbox::str![[r#"
1.3.0 (hash-nightly-2)

"#]])
        .is_ok();
    cx.config
        .expect(["rustup", "run", "stable", "rustc", "--version"])
        .await
        .with_stderr(snapbox::str![[r#"
error: toolchain 'stable-[HOST_TUPLE]' is not installed
help: run `rustup toolchain install stable-[HOST_TUPLE]` to install it

"#]])
        .is_err();
}

#[tokio::test]
async fn readline_no_stdin() {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;
    cx.config
        .expect(["rustup-init", "--no-modify-path"])
        .await
        .with_stderr(snapbox::str![[r#"
...
error: unable to read from stdin for confirmation[..]
...
"#]])
        .is_err();
}

#[tokio::test]
async fn rustup_init_works_with_weird_names() {
    // Browsers often rename bins to e.g. rustup-init(2).exe.
    let cx = CliTestContext::new(Scenario::SimpleV2).await;
    let old = cx.config.exedir.join(format!("rustup-init{EXE_SUFFIX}"));
    let new = cx.config.exedir.join(format!("rustup-init(2){EXE_SUFFIX}"));
    fs::rename(old, new).unwrap();
    cx.config
        .expect(&["rustup-init(2)", "-y", "--no-modify-path"])
        .await
        .is_ok();
    let rustup = cx.config.cargodir.join(format!("bin/rustup{EXE_SUFFIX}"));
    assert!(rustup.exists());
}

#[tokio::test]
async fn rls_proxy_set_up_after_install() {
    let mut cx = CliTestContext::new(Scenario::None).await;

    {
        let cx = cx.with_dist_dir(Scenario::SimpleV2);
        cx.config
            .expect(["rustup-init", "-y", "--no-modify-path"])
            .await
            .is_ok();
        cx.config
            .expect(["rustup", "toolchain", "install", "stable"])
            .await
            .is_ok();
        cx.config.pin("stable");
    }

    cx.config
        .expect(["rls", "--version"])
        .await
        .with_stderr(snapbox::str![[r#"
error: 'rls[EXE]' is not installed for the toolchain 'stable-[HOST_TUPLE]'.
help: run `rustup component add --toolchain stable-[HOST_TUPLE] rls` to install it

"#]])
        .is_err();
    cx.config
        .expect(["rustup", "component", "add", "rls"])
        .await
        .is_ok();
    cx.config.expect(["rls", "--version"]).await.is_ok();
}

#[tokio::test]
async fn install_with_components_and_targets() {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;
    cx.config
        .expect([
            "rustup-init",
            "--default-toolchain",
            "nightly",
            "-y",
            "-c",
            "rls",
            "-t",
            CROSS_ARCH1,
            "--no-modify-path",
        ])
        .await
        .is_ok();
    cx.config
        .expect(["rustup", "toolchain", "list"])
        .await
        .with_stdout(snapbox::str![""])
        .with_stderr(snapbox::str![[r#"
info: no installed toolchains
warn: no toolchain installed
help: run 'rustup toolchain install stable' to download the latest stable release of Rust, then name it in rust-toolchain.toml

"#]])
        .is_ok();
}

#[tokio::test]
async fn install_minimal_profile() {
    let cx = CliTestContext::new(Scenario::SimpleV2).await;
    cx.config
        .expect([
            "rustup-init",
            "-y",
            "--profile",
            "minimal",
            "--no-modify-path",
        ])
        .await
        .is_ok();
    cx.config.pin("stable");

    cx.config.expect_component_executable("rustup").await;
    cx.config.expect_component_executable("rustc").await;
    cx.config.expect_component_not_executable("cargo").await;
}

