use std::{
    env::consts::EXE_SUFFIX,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::Context;

use crate::{process::Process, utils};

/// Exclusive right to replace the installed rustup binary.
pub(super) struct InstallLock {
    _file: File,
}

impl InstallLock {
    pub(super) fn lock(process: &Process) -> anyhow::Result<Self> {
        let lock = Self::open(process)?;
        utils::raw::lock(&lock._file).context("failed to lock rustup binary replacement")?;
        Ok(lock)
    }

    fn open(process: &Process) -> anyhow::Result<Self> {
        let directory = stage_root(process)?;
        utils::ensure_dir_exists("rustup install", &directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // The file exists only to be locked; never touch its contents.
            .truncate(false)
            .open(directory.join(INSTALL_LOCK_FILE))
            .context("failed to open rustup install lock")?;

        Ok(Self { _file: file })
    }

    /// Installs the running executable as `rustup` in `bin_path`.
    ///
    /// Proxy names such as `cargo` and `rustc` are not created.
    pub(super) fn install_bins(
        &self,
        bin_path: &Path,
        force_hard_links: bool,
    ) -> anyhow::Result<()> {
        self.install_bins_from(&utils::current_exe()?, bin_path, force_hard_links)
    }

    fn install_bins_from(
        &self,
        this_exe_path: &Path,
        bin_path: &Path,
        force_hard_links: bool,
    ) -> anyhow::Result<()> {
        let rustup_path = bin_path.join(format!("rustup{EXE_SUFFIX}"));
        utils::ensure_dir_exists("bin", bin_path)?;

        // Stage the new binary as a sibling of the installed one, so that
        // publishing it below is a rename within a single directory.
        let pending = tempfile::Builder::new()
            .prefix(PENDING_BINARY_PREFIX)
            .tempfile_in(bin_path)
            .context("failed to reserve a pending rustup binary")?
            .into_temp_path();

        // TempPath reserves a unique name, but preserving a source symlink requires
        // an absent destination rather than an existing empty file.
        fs::remove_file(&pending).context("failed to prepare the pending rustup path")?;
        utils::copy_file_symlink_to_source(this_exe_path, &pending)?;
        utils::make_executable(&pending)?;
        if !fs::symlink_metadata(&pending)?.file_type().is_symlink() {
            OpenOptions::new()
                .write(true)
                .open(&pending)
                .and_then(|file| file.sync_all())
                .context("failed to sync the pending rustup binary")?;
        }

        let _ = force_hard_links;
        replace_rustup_binary(&pending, &rustup_path)?;
        Ok(())
    }
}

fn replace_rustup_binary(replacement: &Path, rustup: &Path) -> anyhow::Result<()> {
    // `rename` replaces an existing destination in one step on every platform,
    // so a failure here leaves the installed rustup untouched. The copy fallback
    // is disabled because it would break that guarantee; it is never needed,
    // since the replacement lives in the same directory as the target.
    utils::rename("rustup", replacement, rustup, false)?;
    // Make the rename durable. Windows has no directory handle to sync.
    #[cfg(unix)]
    File::open(
        rustup
            .parent()
            .context("installed rustup binary has no parent directory")?,
    )
    .and_then(|directory| directory.sync_all())
    .context("failed to sync rustup binary directory")?;

    Ok(())
}

fn stage_root(process: &Process) -> anyhow::Result<PathBuf> {
    Ok(process.rustup_home()?.join(INSTALL_DIRECTORY))
}

const INSTALL_DIRECTORY: &str = "install-lock";
const INSTALL_LOCK_FILE: &str = "install.lock";
const PENDING_BINARY_PREFIX: &str = ".rustup-pending-";

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{
        process::TestProcess,
        test::{Env, test_dir},
    };

    #[tokio::test]
    async fn install_lock_is_global() {
        let root = test_dir().unwrap();
        let process = test_process(root.path());
        let lock = InstallLock::lock(&process.process).unwrap();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(stage_root(&process.process).unwrap().join(INSTALL_LOCK_FILE))
            .unwrap();

        assert!(matches!(
            utils::raw::try_lock(&contender),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(lock);
        utils::raw::try_lock(&contender).unwrap();
    }

    #[tokio::test]
    async fn install_bins_preserves_existing_rustup_if_source_disappears() {
        let root = test_dir().unwrap();
        let process = test_process(root.path());
        let rustup = root.path().join(format!("cargo/bin/rustup{EXE_SUFFIX}"));
        fs::create_dir_all(rustup.parent().unwrap()).unwrap();
        fs::write(&rustup, "old rustup").unwrap();

        InstallLock::lock(&process.process)
            .unwrap()
            .install_bins_from(
                &root.path().join("missing-updater"),
                rustup.parent().unwrap(),
                false,
            )
            .unwrap_err();

        assert_eq!(fs::read_to_string(rustup).unwrap(), "old rustup");
    }

    #[test]
    fn failed_replace_preserves_existing_rustup() {
        let root = test_dir().unwrap();
        let rustup = root.path().join(format!("rustup{EXE_SUFFIX}"));
        fs::write(&rustup, "old rustup").unwrap();

        replace_rustup_binary(&root.path().join("missing"), &rustup).unwrap_err();

        assert_eq!(fs::read_to_string(rustup).unwrap(), "old rustup");
    }

    #[test]
    fn replace_publishes_pending_rustup() {
        let root = test_dir().unwrap();
        let rustup = root.path().join(format!("rustup{EXE_SUFFIX}"));
        let pending = root.path().join("pending");
        fs::write(&rustup, "old rustup").unwrap();
        fs::write(&pending, "new rustup").unwrap();

        replace_rustup_binary(&pending, &rustup).unwrap();

        assert_eq!(fs::read_to_string(rustup).unwrap(), "new rustup");
        assert!(!pending.exists());
    }

    fn test_process(root: &Path) -> TestProcess {
        let mut vars = HashMap::new();
        vars.env("HOME", root);
        vars.env("CARGO_HOME", root.join("cargo"));
        vars.env("RUSTUP_HOME", root.join("rustup"));
        TestProcess::with_vars(vars)
    }
}
