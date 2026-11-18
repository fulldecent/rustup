use std::path::PathBuf;

use anyhow::{Context, bail};
use tracing::{error, warn};

use super::stage::{self, PreparedUpdater, SelfUpdateLock};
use crate::{process::Process, utils};

// If the user is trying to install with sudo, on some systems this will
// result in writing root-owned files to the user's home directory, because
// sudo is configured not to change $HOME. Don't let that bogosity happen.
pub(crate) fn anti_sudo_check(
    no_prompt: bool,
    process: &Process,
) -> anyhow::Result<utils::ExitCode> {
    pub(crate) fn home_mismatch(process: &Process) -> (bool, PathBuf, PathBuf) {
        let fallback = || (false, PathBuf::new(), PathBuf::new());
        // test runner should set this, nothing else
        if process
            .var_os("RUSTUP_INIT_SKIP_SUDO_CHECK")
            .is_some_and(|s| s == "yes")
        {
            return fallback();
        }

        match (utils::home_dir_from_passwd(), process.var_os("HOME")) {
            (Some(pw), Some(eh)) if eh != pw => return (true, PathBuf::from(eh), pw),
            (None, _) => warn!("getpwuid_r: couldn't get user data"),
            _ => {}
        }
        fallback()
    }

    match home_mismatch(process) {
        (false, _, _) => {}
        (true, env_home, euid_home) => {
            error!("$HOME differs from euid-obtained home directory: you may be using sudo");
            error!("$HOME directory: {}", env_home.display());
            error!("euid-obtained home directory: {}", euid_home.display());
            if !no_prompt {
                error!("if this is what you want, restart the installation with `-y'");
                return Ok(utils::ExitCode(1));
            }
        }
    }

    Ok(utils::ExitCode(0))
}

/// Tell the updater to replace the rustup binary, then wait for it to finish.
pub(super) fn run_update(
    prepared_updater: PreparedUpdater,
    _process: &Process,
) -> anyhow::Result<utils::ExitCode> {
    let setup_path = prepared_updater.to_path_buf();
    let status = prepared_updater
        .spawn_replacer()?
        .wait()
        .with_context(|| format!("unable to wait for updater ({})", setup_path.display()))?;

    if !status.success() {
        bail!("self-updated failed to replace rustup executable");
    }

    Ok(utils::ExitCode(0))
}

/// Replaces `$CARGO_HOME/bin/rustup` with the running executable.
pub(crate) fn self_replace(process: &Process) -> anyhow::Result<utils::ExitCode> {
    let self_update_lock = SelfUpdateLock::lock(process)?;
    #[cfg(feature = "test")]
    process.checkpoint(super::CHECKPOINT_SELF_REPLACE_READY);
    let result = process.cargo_home().and_then(|cargo_home| {
        self_update_lock.install_bins(&cargo_home.join("bin"), super::force_hard_links(process))
    });
    stage::mark_result(result.is_ok(), process);
    result?;

    Ok(utils::ExitCode(0))
}
