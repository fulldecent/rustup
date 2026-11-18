use std::path::PathBuf;

use tracing::{error, warn};

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
