//! Self-installation and updating
//!
//! This is the installer at the heart of Rust. If it breaks
//! everything breaks. It is conceptually very simple, as rustup is
//! distributed as a single binary, and installation mostly requires
//! copying it into place. There are some tricky bits though, mostly
//! because of workarounds to self-delete an exe on Windows.
//!
//! During install (as `rustup-init`):
//!
//! * copy the self exe to $CARGO_HOME/bin
//!
//! The installer does not create proxy names, edit PATH, or download a toolchain.
//!
//! During upgrade (`rustup self update`):
//!
//! * download rustup-init to a managed path under `$RUSTUP_HOME`
//! * run the downloaded binary in replacement mode
//! * atomically replace the rustup binary. On Windows this happens
//!   after the update command exits.
//!
//! During uninstall (`rustup self uninstall`):
//!
//! * Delete `$RUSTUP_HOME`.
//! * Delete all entries in `$CARGO_HOME` except `bin`.
//! * Delete the rustup binary, and any proxy names that are the same
//!   file, from `$CARGO_HOME/bin`.
//! * Delete `$CARGO_HOME/bin` if it is empty after uninstall.
//! * Delete `$CARGO_HOME` if it is empty after uninstall.
//!
//! Deleting the running binary during uninstall is tricky
//! and racy on Windows.

#[cfg(unix)]
use std::borrow::Cow;
use std::{
    env::{self, consts::EXE_SUFFIX},
    fmt, fs,
    io::{self, Write},
    path::{MAIN_SEPARATOR, Path, PathBuf},
    process::Command,
    str::FromStr,
};

use anstyle::Style;
use anyhow::{Context, anyhow};
use clap::{ValueEnum, builder::PossibleValue};
use clap_cargo::style::{GOOD, WARN};
use itertools::Itertools;
use same_file::is_same_file;
use serde::{Deserialize, Serialize};
use tracing::{error, info, trace, warn};

use crate::{
    DUP_TOOLS, TOOLS,
    cli::{
        common::{self, Confirm, PackageUpdate, report_error},
        errors::CliError,
        markdown::md,
    },
    config::Cfg,
    dist::{
        Profile, TargetTuple,
        download::DownloadCfg,
    },
    download::{DownloadError, DownloadOptions},
    errors::RustupError,
    install::UpdateStatus,
    process::Process,
    settings::SettingsFile,
    toolchain::{
        MaybeChannelToolchainName, PartialToolchainName, Toolchain,
    },
    utils::{self, ExitCode},
};

#[macro_use]
mod msg;

mod stage;
#[cfg(feature = "test")]
pub use stage::{Marker, SELF_UPDATE_DIRECTORY, updater_path};
use stage::{PreparedUpdater, SelfUpdateLock};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::self_replace;
#[cfg(unix)]
use unix::run_update;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::complete_windows_uninstall;
#[cfg(windows)]
pub(crate) use windows::self_replace;
#[cfg(all(windows, feature = "test"))]
pub use windows::{RUSTUP_REGISTRY_TEST_ID, RegistryValueId, USER_PATH, get_path};
#[cfg(windows)]
use windows::{add_uninstall_registry_entry, remove_uninstall_registry_entry, run_update};

pub(crate) struct InstallOpts<'a> {
    pub default_host_tuple: Option<String>,
    pub default_toolchain: Option<MaybeChannelToolchainName>,
    pub profile: Profile,
    pub no_modify_path: bool,
    pub no_update_toolchain: bool,
    pub components: &'a [&'a str],
    pub targets: &'a [&'a str],
}

impl InstallOpts<'_> {
    /// Copies the running binary to the cargo bin directory.
    ///
    /// Profile and the default host triple are recorded for later
    /// `rustup toolchain install` commands. Toolchain names, components,
    /// targets, and PATH flags are accepted and ignored.
    pub(crate) async fn install(
        mut self,
        current_dir: PathBuf,
        no_prompt: bool,
        quiet: bool,
        process: &Process,
    ) -> anyhow::Result<ExitCode> {
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut exit_code = ExitCode::SUCCESS;

        self.validate(process).map_err(|e| {
            anyhow!(
                "Pre-checks for host and toolchain failed: {e:#}\n\
            If you are unsure of suitable values, the 'stable' toolchain is the default.\n\
            Valid host tuples look something like: {}",
                TargetTuple::from_host_or_build(process)
            )
        })?;

        // Toolchain, component, target, and PATH flags stay on the command
        // line so existing invocations still parse. They do not change install.
        let _ = (
            &self.default_toolchain,
            self.components,
            self.targets,
            self.no_modify_path,
            self.no_update_toolchain,
        );

        if process
            .var_os("RUSTUP_INIT_SKIP_EXISTENCE_CHECKS")
            .is_none_or(|s| s != "yes")
        {
            check_existence_of_settings_file(process)?;
        }

        #[cfg(unix)]
        {
            exit_code &= unix::anti_sudo_check(no_prompt, process)?;
        }

        let mut term = process.stdout();

        #[cfg(windows)]
        windows::maybe_install_msvc(&mut term, no_prompt, quiet, &self, process).await?;

        if !no_prompt {
            let msg = pre_install_msg(process)?;

            md(&mut term, msg);
            let mut customized_install = false;
            loop {
                md(&mut term, self.display(process));
                match common::confirm_advanced(customized_install, process)? {
                    Confirm::No => {
                        info!("aborting installation");
                        return Ok(ExitCode::SUCCESS);
                    }
                    Confirm::Yes => break,
                    Confirm::Advanced => {
                        customized_install = true;
                        self.customize(process)?;
                    }
                }
            }
        }

        if let Err(e) = self.install_rust(current_dir, quiet, process).await {
            report_error(&e, process);

            // On windows, where installation happens in a console
            // that may have opened just for this purpose, give
            // the user an opportunity to see the error before the
            // window closes.
            #[cfg(windows)]
            if !no_prompt {
                windows::ensure_prompt(process)?;
            }

            return Ok(ExitCode::FAILURE);
        }

        let cargo_home = process.cargo_home()?;
        let home_dir = process.home_dir();
        let msg = format!(
            post_install_msg!(),
            cargo_bin_dir = HomeDisplay::new(&cargo_home.join("bin"), home_dir.as_deref()),
        );
        md(&mut term, msg);

        #[cfg(unix)]
        warn_if_default_linker_missing(process);

        #[cfg(windows)]
        if !no_prompt {
            // On windows, where installation happens in a console
            // that may have opened just for this purpose, require
            // the user to press a key to continue.
            windows::ensure_prompt(process)?;
        }

        Ok(exit_code)
    }

    /// Copies the rustup binary and records profile and host.
    async fn install_rust(
        self,
        current_dir: PathBuf,
        quiet: bool,
        process: &Process,
    ) -> anyhow::Result<()> {
        let cargo_bin = process.cargo_home()?.join("bin");
        install_bins(process, &cargo_bin, force_hard_links(process))?;

        #[cfg(windows)]
        add_uninstall_registry_entry(process)?;

        // If RUSTUP_HOME is not set, make sure it exists
        if process.var_os("RUSTUP_HOME").is_none() {
            let home = process
                .home_dir()
                .map(|p| p.join(".rustup"))
                .ok_or_else(|| anyhow::anyhow!("could not find home dir to put .rustup in"))?;

            fs::create_dir_all(home).context("unable to create ~/.rustup")?;
        }

        let mut cfg = Cfg::from_env(current_dir, quiet, false, process)?;
        // Record profile and host for later `rustup toolchain install`.
        // Do not download a toolchain and do not record a default.
        cfg.set_profile(self.profile)?;
        if let Some(default_host_tuple) = &self.default_host_tuple {
            info!("setting default host tuple to {}", default_host_tuple);
            cfg.set_default_host_tuple(default_host_tuple.to_owned())?;
        } else {
            info!("default host tuple is {}", cfg.default_host_tuple()?);
        }
        Ok(())
    }

    // Interactive editing of the install options
    fn customize(&mut self, process: &Process) -> anyhow::Result<()> {
        writeln!(
            process.stdout().lock(),
            "I'm going to ask you the value of each of these installation options.\n\
         You may simply press the Enter key to leave unchanged."
        )?;

        writeln!(process.stdout().lock())?;

        self.default_host_tuple = Some(common::question_str(
            "Default host tuple?",
            &self
                .default_host_tuple
                .take()
                .unwrap_or_else(|| TargetTuple::from_host_or_build(process).to_string()),
            process,
        )?);

        self.profile = <Profile as FromStr>::from_str(&common::question_str(
            &format!(
                "Profile (which tools and data to install)? ({})",
                Profile::value_variants().iter().join("/"),
            ),
            self.profile.as_str(),
            process,
        )?)?;

        Ok(())
    }

    fn validate(&self, process: &Process) -> anyhow::Result<()> {
        common::warn_if_host_is_emulated(process);

        let host_tuple = self
            .default_host_tuple
            .as_ref()
            .map(TargetTuple::new)
            .unwrap_or_else(|| TargetTuple::from_host_or_build(process));
        let partial_channel = match &self.default_toolchain {
            None | Some(MaybeChannelToolchainName::None) => {
                PartialToolchainName::from_str("stable")?
            }
            Some(MaybeChannelToolchainName::Some(s)) => s.into(),
        };
        let resolved = partial_channel.complete(&host_tuple)?;
        trace!("Successfully resolved installation toolchain as: {resolved}");
        Ok(())
    }

    fn display(&self, process: &Process) -> String {
        format!(
            r"Current installation options:

- `  `default host tuple: `{}`
- `             `profile: `{}`
",
            self.default_host_tuple.as_ref().map_or_else(
                || TargetTuple::from_host_or_build(process),
                TargetTuple::new,
            ),
            self.profile,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelfUpdateMode {
    #[default]
    Enable,
    Disable,
    CheckOnly,
}

impl SelfUpdateMode {
    pub(crate) fn from_cfg(cfg: &Cfg<'_>) -> anyhow::Result<Self> {
        if cfg.process.is_ci() {
            return Ok(Self::Disable);
        }

        cfg.settings_file.with(|s| {
            Ok(match s.auto_self_update {
                Some(mode) => mode,
                None => Self::Enable,
            })
        })
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::CheckOnly => "check-only",
        }
    }

    /// Optionally performs a self-update: check policy, download, apply and exit.
    ///
    /// Whether the self-update is executed is based on both compile-time and runtime
    /// configurations, where the priority is as follows:
    /// no-self-update feature > self update mode > CLI flag
    ///
    /// i.e. update only if rustup does **not** have the no-self-update feature,
    /// and self update mode is configured to **enable**
    /// and has **no** `--no-self-update` CLI flag.
    pub(crate) async fn update(
        &self,
        should_self_update: bool,
        dl_cfg: &DownloadCfg<'_>,
    ) -> anyhow::Result<ExitCode> {
        if cfg!(feature = "no-self-update") {
            info!("self-update is disabled for this build of rustup");
            info!("any updates to rustup will need to be fetched with your system package manager");
            return Ok(ExitCode::SUCCESS);
        }
        match self {
            Self::Enable if should_self_update => (),
            Self::CheckOnly => {
                check_rustup_update(dl_cfg).await?;
                return Ok(ExitCode::SUCCESS);
            }
            _ => return Ok(ExitCode::SUCCESS),
        }

        match self_update_permitted(false)? {
            SelfUpdatePermission::HardFail => {
                error!("Unable to self-update.  STOP");
                return Ok(ExitCode::FAILURE);
            }
            #[cfg(not(windows))]
            SelfUpdatePermission::Skip => return Ok(ExitCode::SUCCESS),
            SelfUpdatePermission::Permit => {}
        }

        let prepared_updater = prepare_update(dl_cfg).await?;

        if let Some(prepared_updater) = prepared_updater {
            return run_update(prepared_updater, dl_cfg.process);
        }

        Ok(ExitCode::SUCCESS)
    }
}

impl ValueEnum for SelfUpdateMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Enable, Self::Disable, Self::CheckOnly]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(self.as_str()))
    }

    fn from_str(input: &str, _: bool) -> Result<Self, String> {
        <Self as FromStr>::from_str(input).map_err(|e| e.to_string())
    }
}

impl FromStr for SelfUpdateMode {
    type Err = anyhow::Error;

    fn from_str(mode: &str) -> anyhow::Result<Self> {
        match mode {
            "enable" => Ok(Self::Enable),
            "disable" => Ok(Self::Disable),
            "check-only" => Ok(Self::CheckOnly),
            _ => Err(anyhow!(format!(
                "unknown self update mode: '{}'; valid modes are {}",
                mode,
                Self::value_variants().iter().join(", ")
            ))),
        }
    }
}

impl fmt::Display for SelfUpdateMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn update_root(process: &Process) -> String {
    process
        .var("RUSTUP_UPDATE_ROOT")
        .inspect(|url| trace!("`RUSTUP_UPDATE_ROOT` has been set to `{url}`"))
        .unwrap_or_else(|_| String::from(DEFAULT_UPDATE_ROOT))
}

/// Displays an installation path with a platform-specific home abbreviation.
struct HomeDisplay<'a> {
    path: &'a Path,
    home_prefix: Option<&'a str>,
}

impl<'a> HomeDisplay<'a> {
    fn new(path: &'a Path, home_dir: Option<&Path>) -> Self {
        match home_dir.and_then(|home| path.strip_prefix(home).ok()) {
            Some(relative) => Self {
                path: relative,
                home_prefix: Some(cfg_select! {
                    windows => "%USERPROFILE%",
                    _ => "$HOME",
                }),
            },
            None => Self {
                path,
                home_prefix: None,
            },
        }
    }
}

impl fmt::Display for HomeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(home_prefix) = self.home_prefix {
            f.write_str(home_prefix)?;
            if !self.path.is_empty() {
                write!(f, "{MAIN_SEPARATOR}")?;
            }
        }
        self.path.display().fmt(f)
    }
}

fn check_existence_of_settings_file(process: &Process) -> anyhow::Result<()> {
    let rustup_dir = process.rustup_home()?;
    let settings_file = SettingsFile::new(rustup_dir.join("settings.toml"));
    if !utils::path_exists(&settings_file.path) {
        return Ok(());
    }
    let settings_toolchain = settings_file.with(|s| Ok(s.default_toolchain.clone()))?;
    // If there is already a non-empty `settings.toml` file (e.g., not a fresh install),
    // then we warn the user that there was an already configured default toolchain.
    if settings_toolchain.is_none()
        && settings_file.with(|s| Ok(s.default_host_tuple.is_none()))?
    {
        return Ok(());
    }
    warn!("it looks like you have an existing rustup settings file at:");
    warn!("{}", settings_file.path.display());
    Ok(())
}

fn pre_install_msg(process: &Process) -> anyhow::Result<String> {
    let cargo_home = process.cargo_home()?;
    let cargo_bin_dir = cargo_home.join("bin");
    let rustup_home = home::rustup_home()?;
    Ok(format!(
        pre_install_msg!(),
        cargo_home = cargo_home.display(),
        cargo_bin_dir = cargo_bin_dir.display(),
        rustup_home = rustup_home.display(),
    ))
}

#[cfg(unix)]
fn warn_if_default_linker_missing(process: &Process) {
    // Search for linker in PATH
    let Some(path) = process.var_os("PATH") else {
        warn!("unable to search PATH for a default linker");
        warn!("many Rust crates require a system C toolchain to build");
        return;
    };

    // If we have the host tuple, attempt to determine the CC/linker path that
    // `cc-rs` would use, as this is *usually* why we need a linker to invoke.
    //
    // Doing it this way allows us to correctly diagnose quirky systems like
    // solaris and illumos that use `gcc` rather than `cc` for historical reasons.
    let cc_tool = TargetTuple::from_host(process).and_then(|tuple| {
        // Fill in some dummy settings for `Build`/`Tool` to be able to properly
        // give us the metadata we want
        cc::Build::new()
            .cargo_metadata(false)
            .opt_level(0)
            .target(&tuple)
            .host(&tuple)
            .try_get_compiler()
            .ok()
    });

    let cc_binary = if let Some(cc_tool) = &cc_tool {
        Cow::Borrowed(cc_tool.path())
    } else {
        // If we don't get info from cc-rs, fall back to just looking for the literal `cc`.
        Cow::Owned(format!("cc{EXE_SUFFIX}").into())
    };

    // Search the path for the selected binary
    let found = env::split_paths(&path).any(|mut p| {
        p.push(&cc_binary);
        p.is_file()
    });

    if !found {
        let bin_disp = cc_binary.display();
        warn!("no default linker (`{bin_disp}`) was found in your PATH");
        warn!("many Rust crates require a system C toolchain to build");
    }
}

fn install_bins(process: &Process, bin_path: &Path, force_hard_links: bool) -> anyhow::Result<()> {
    SelfUpdateLock::lock(process)?.install_bins(bin_path, force_hard_links)
}

fn force_hard_links(process: &Process) -> bool {
    // HACK: On Windows CI machines, some Docker setups don't like symlinks, so we force hard
    // links in this case.
    // See: <https://github.com/rust-lang/rustup/issues/4291>
    (cfg!(windows) && process.is_ci()) || process.var_os("RUSTUP_FORCE_HARDLINK_PROXIES").is_some()
}

/// Uninstall process:
/// 1. Remove all installed toolchains.
/// 2. Remove rustup home.
/// 3. Remove all entries in `$CARGO_HOME` except `bin`.
/// 4. Remove the rustup binary, and proxy names that are the same file.
/// 5. Try to remove $CARGO_HOME/bin directory if it's empty.
/// 6. Try to remove $CARGO_HOME directory if it's empty.
pub(crate) fn uninstall(
    no_prompt: bool,
    no_modify_path: bool,
    cfg: &Cfg<'_>,
) -> anyhow::Result<ExitCode> {
    if cfg!(feature = "no-self-update") {
        error!("self-uninstall is disabled for this build of rustup");
        error!("you should probably use your system package manager to uninstall rustup");
        return Ok(ExitCode::FAILURE);
    }

    let process = cfg.process;
    let cargo_home = process.cargo_home()?;

    if !cargo_home.join(format!("bin/rustup{EXE_SUFFIX}")).exists() {
        return Err(CliError::NotSelfInstalled { p: cargo_home }.into());
    }

    if !no_prompt {
        writeln!(process.stdout().lock())?;
        let msg = pre_uninstall_msg!().to_owned();
        md(&mut process.stdout(), msg);
        if !common::confirm("\nContinue? (y/N)", false, process)? {
            info!("aborting uninstallation");
            return Ok(ExitCode::SUCCESS);
        }
    }

    info!("removing toolchains");
    for toolchain in cfg.list_toolchains(true)? {
        Toolchain::ensure_removed(cfg, toolchain.into())?;
    }

    info!("removing rustup home");

    // Delete RUSTUP_HOME
    let rustup_dir = home::rustup_home()?;
    if rustup_dir.exists() {
        utils::remove_dir("rustup_home", &rustup_dir)?;
    }

    // Delete rustup.
    #[cfg(unix)]
    clean_cargo_home(no_modify_path, process, &cargo_home)?;
    // NOTE: On windows, this is tricky because this is *probably*
    // the running executable and on Windows can't be unlinked until
    // the process exits.
    // see: windows::{complete_windows_uninstall,spawn_uninstall_gc}
    #[cfg(windows)]
    windows::spawn_uninstall_gc(no_modify_path, &cargo_home)?;

    info!("rustup is uninstalled");

    Ok(ExitCode::SUCCESS)
}

/// Remove rustup-owned cargo-home state.
/// This removes non-`bin` entries in `$CARGO_HOME`, removes rustup tool links and executable from
/// `$CARGO_HOME/bin`, then removes `$CARGO_HOME/bin` and `$CARGO_HOME` only if they are empty.
/// Nonempty directories are left in place.
fn clean_cargo_home(
    no_modify_path: bool,
    process: &Process,
    cargo_home: &Path,
) -> anyhow::Result<()> {
    let cargo_bin = cargo_home.join("bin");

    info!("removing cargo home");

    // Delete everything in CARGO_HOME except the bin directory first.
    let diriter = fs::read_dir(cargo_home).map_err(|e| CliError::ReadDirError {
        p: cargo_home.to_owned(),
        source: e,
    })?;
    for dirent in diriter {
        let dirent = dirent.map_err(|e| CliError::ReadDirError {
            p: cargo_home.to_owned(),
            source: e,
        })?;
        if dirent.file_name().to_str() != Some("bin") {
            if dirent.path().is_dir() {
                utils::remove_dir("cargo_home", &dirent.path())?;
            } else {
                utils::remove_file("cargo_home", &dirent.path())?;
            }
        }
    }

    info!("removing rustup tool links and binary");

    let rustup_path = cargo_bin.join(format!("rustup{EXE_SUFFIX}"));

    let proxy_paths = TOOLS
        .iter()
        .chain(DUP_TOOLS.iter())
        .map(|tool| cargo_bin.join(format!("{tool}{EXE_SUFFIX}")));

    for proxy_path in proxy_paths {
        if is_same_file(&proxy_path, &rustup_path).unwrap_or(false) {
            utils::remove_file("rustup tool proxy", &proxy_path)?;
        }
    }

    utils::remove_file("rustup_bin", &rustup_path)?;

    #[cfg(windows)]
    remove_uninstall_registry_entry(process)?;

    let cargo_bin_display = cargo_bin.display();
    info!("removing empty cargo bin directory `{cargo_bin_display}`");

    match fs::remove_dir(&cargo_bin) {
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
            warn!("keeping non-empty cargo bin directory `{cargo_bin_display}`")
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!("failed to remove cargo bin directory `{cargo_bin_display}`")
            });
        }
        Ok(()) => {
            let _ = no_modify_path;
            let _ = process;
        }
    }

    let cargo_home_display = cargo_home.display();
    info!("removing empty cargo home directory `{cargo_home_display}`");

    match fs::remove_dir(cargo_home) {
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
            warn!("keeping non-empty cargo home directory `{cargo_home_display}`");
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!("failed to remove cargo home directory `{cargo_home_display}`")
            });
        }
        Ok(()) => {}
    }

    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum SelfUpdatePermission {
    HardFail,
    #[cfg(not(windows))]
    Skip,
    Permit,
}

#[cfg(windows)]
pub(crate) fn self_update_permitted(_explicit: bool) -> anyhow::Result<SelfUpdatePermission> {
    Ok(SelfUpdatePermission::Permit)
}

#[cfg(not(windows))]
pub(crate) fn self_update_permitted(explicit: bool) -> anyhow::Result<SelfUpdatePermission> {
    // Detect if rustup is not meant to self-update
    let current_exe = env::current_exe()?;
    let current_exe_dir = current_exe.parent().expect("Rustup isn't in a directory‽");
    if let Err(e) = tempfile::Builder::new()
        .prefix("updtest")
        .tempdir_in(current_exe_dir)
    {
        match e.kind() {
            io::ErrorKind::PermissionDenied => {
                trace!("Skipping self-update because we cannot write to the rustup dir");
                if explicit {
                    return Ok(SelfUpdatePermission::HardFail);
                } else {
                    return Ok(SelfUpdatePermission::Skip);
                }
            }
            _ => return Err(e.into()),
        }
    }
    Ok(SelfUpdatePermission::Permit)
}

/// Downloads the managed updater and runs it in replacement mode.
///
/// The updater is removed by a later rustup invocation because Windows
/// cannot delete the updater while its process is still running.
pub(crate) async fn update(cfg: &Cfg<'_>) -> anyhow::Result<ExitCode> {
    common::warn_if_host_is_emulated(cfg.process);

    use SelfUpdatePermission::*;
    let update_permitted = if cfg!(feature = "no-self-update") {
        HardFail
    } else {
        self_update_permitted(true)?
    };
    match update_permitted {
        HardFail => {
            // TODO: Detect which package manager and be more useful.
            error!("self-update is disabled for this build of rustup");
            error!("you should probably use your system package manager to update rustup");
            return Ok(ExitCode::FAILURE);
        }
        #[cfg(not(windows))]
        Skip => {
            info!("Skipping self-update at this time");
            return Ok(ExitCode::SUCCESS);
        }
        Permit => {}
    }

    match prepare_update(&DownloadCfg::new(cfg)).await? {
        Some(prepared_updater) => {
            let Some(version) = get_and_parse_new_rustup_version(&prepared_updater) else {
                error!("failed to get rustup version");
                return Ok(ExitCode::FAILURE);
            };

            let _ = common::show_channel_update(
                cfg,
                PackageUpdate::Rustup,
                Ok(UpdateStatus::Updated(version)),
            );
            return run_update(prepared_updater, cfg.process);
        }
        None => {
            let _ = common::show_channel_update(
                cfg,
                PackageUpdate::Rustup,
                Ok(UpdateStatus::Unchanged),
            );
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn get_and_parse_new_rustup_version(path: &Path) -> Option<String> {
    get_new_rustup_version(path).map(parse_new_rustup_version)
}

fn get_new_rustup_version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    String::from_utf8(output.stdout).ok()
}

fn parse_new_rustup_version(version: String) -> String {
    use std::sync::LazyLock;

    use regex::Regex;

    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[0-9]+.[0-9]+.[0-9]+[0-9a-zA-Z-]*").unwrap());

    let capture = RE.captures(&version);
    let matched_version = match capture {
        Some(cap) => cap.get(0).unwrap().as_str(),
        None => "(unknown)",
    };
    String::from(matched_version)
}

async fn prepare_update(dl_cfg: &DownloadCfg<'_>) -> anyhow::Result<Option<PreparedUpdater>> {
    let cargo_home = dl_cfg.process.cargo_home()?;
    let rustup_path = cargo_home.join(format!("bin{MAIN_SEPARATOR}rustup{EXE_SUFFIX}"));

    if !rustup_path.exists() {
        return Err(CliError::NotSelfInstalled { p: cargo_home }.into());
    }
    let self_update_lock = SelfUpdateLock::lock(dl_cfg.process)?;

    // Get build tuple
    let tuple = TargetTuple::from_build();

    // For windows x86 builds seem slow when used with windows defender.
    // The website defaulted to i686-windows-gnu builds for a long time.
    // This ensures that we update to a version that's appropriate for users
    // and also works around if the website messed up the detection.
    // If someone really wants to use another version, they still can enforce
    // that using the environment variable RUSTUP_OVERRIDE_HOST_TUPLE.
    #[cfg(windows)]
    let tuple = TargetTuple::from_host(dl_cfg.process).unwrap_or(tuple);

    // Get update root.
    let update_root = update_root(dl_cfg.process);

    // Get current version
    let current_version = env!("CARGO_PKG_VERSION");

    // Get available version
    info!("checking for self-update (current version: {current_version})");
    let available_version = match dl_cfg.process.var_opt("RUSTUP_VERSION")? {
        Some(ver) => {
            info!("`RUSTUP_VERSION` has been set to `{ver}`");
            ver
        }
        None => get_available_rustup_version(dl_cfg).await?,
    };

    // If up-to-date
    if available_version == current_version {
        return Ok(None);
    }

    // Get download URL
    let url = format!("{update_root}/archive/{available_version}/{tuple}/rustup-init{EXE_SUFFIX}");

    // Get download path
    let download_url = utils::parse_url(&url)?;
    let prepared_updater = PreparedUpdater::try_from(self_update_lock)?;

    // Download the new rustup executable
    info!("downloading self-update (new version: {available_version})");
    match dl_cfg.download_hash(download_url.as_str()).await {
        Ok(hash) => {
            let name = "rustup";
            let status = dl_cfg.status_for(name, name.len());

            let installer = dl_cfg
                .download_with_retry(&download_url, &hash, &status, dl_cfg.max_retries())
                .await?;

            utils::rename(
                name,
                &installer,
                &prepared_updater,
                dl_cfg.permit_copy_rename,
            )?;
        }
        // If the corresponding `.sha256` file is not found, skip hash check and use simple download
        // as a fallback.
        Err(e)
            if matches!(
                e.downcast_ref::<DownloadError>(),
                Some(DownloadError::HttpStatus(404) | DownloadError::FileNotFound)
            ) =>
        {
            warn!(url = %download_url, "`.sha256` checksum not found, skipping hash check");
            DownloadOptions::try_from(dl_cfg.process)?
                .start(&download_url, &prepared_updater)
                .download()
                .await?;
        }
        Err(e) => return Err(e),
    };

    // Mark as executable
    utils::make_executable(&prepared_updater)?;

    #[cfg(feature = "test")]
    dl_cfg.process.checkpoint(CHECKPOINT_SELF_UPDATE_PREPARED);

    Ok(Some(prepared_updater))
}

async fn get_available_rustup_version(dl_cfg: &DownloadCfg<'_>) -> anyhow::Result<String> {
    let update_root = update_root(dl_cfg.process);
    let tempdir = tempfile::Builder::new()
        .prefix("rustup-update")
        .tempdir()
        .context("error creating temp directory")?;

    // Parse the release file.
    let release_file_url = format!("{update_root}/release-stable.toml");
    let release_file_url = utils::parse_url(&release_file_url)?;
    let release_file = tempdir.path().join("release-stable.toml");
    DownloadOptions::try_from(dl_cfg.process)?
        .start(&release_file_url, &release_file)
        .download()
        .await?;

    let release_toml_str = utils::read_file("rustup release", &release_file)?;
    let release_toml = toml::from_str::<RustupManifest>(&release_toml_str)
        .context("unable to parse rustup release file")?;

    Ok(release_toml.version)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
struct RustupManifest {
    schema_version: SchemaVersion,
    version: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum SchemaVersion {
    #[serde(rename = "1")]
    #[default]
    V1,
}

impl SchemaVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::V1 => "1",
        }
    }
}

impl FromStr for SchemaVersion {
    type Err = RustupError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "1" => Ok(Self::V1),
            _ => Err(RustupError::UnsupportedVersion(s.to_owned())),
        }
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Returns whether an update was available
pub(crate) async fn check_rustup_update(dl_cfg: &DownloadCfg<'_>) -> anyhow::Result<bool> {
    let t = dl_cfg.process.stdout();
    let mut t = t.lock();
    // Get current rustup version
    let current_version = env!("CARGO_PKG_VERSION");

    // Get available rustup version
    let available_version = get_available_rustup_version(dl_cfg).await?;

    let bold = Style::new().bold();
    let yellow = WARN;
    let green = GOOD;

    write!(t, "{bold}rustup - {bold:#}")?;

    Ok(if current_version != available_version {
        writeln!(
            t,
            "{yellow}update available{yellow:#} : {current_version} -> {available_version}"
        )?;
        true
    } else {
        writeln!(t, "{green}up to date{green:#} : {current_version}")?;
        false
    })
}

#[tracing::instrument(level = "trace")]
pub(crate) fn cleanup_self_updater(process: &Process, bin_path: &Path) -> anyhow::Result<()> {
    stage::cleanup(process, bin_path)
}

static DEFAULT_UPDATE_ROOT: &str = "https://static.rust-lang.org/rustup";
#[cfg(feature = "test")]
pub const CHECKPOINT_SELF_UPDATE_PREPARED: &str = "self-update-prepared";
#[cfg(feature = "test")]
pub const CHECKPOINT_SELF_REPLACE_READY: &str = "self-replace-ready";

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path};

    use super::HomeDisplay;
    use crate::{
        process::TestProcess,
        test::{Env, test_dir},
    };

    #[test]
    fn bin_home_display() {
        let home = Path::new("/home/user");
        let cargo_bin_dir = home.join(".cargo").join("bin");
        assert_eq!(
            HomeDisplay::new(&cargo_bin_dir, Some(home)).to_string(),
            cfg_select! {
                windows => r"%USERPROFILE%\.cargo\bin",
                _ => "$HOME/.cargo/bin",
            }
        );

        let local_bin_dir = home.join(".local").join("bin");
        assert_eq!(
            HomeDisplay::new(&local_bin_dir, Some(home)).to_string(),
            cfg_select! {
                windows => r"%USERPROFILE%\.local\bin",
                _ => "$HOME/.local/bin",
            }
        );
        assert_eq!(
            HomeDisplay::new(home, Some(home)).to_string(),
            cfg_select! {
                windows => "%USERPROFILE%",
                _ => "$HOME",
            }
        );

        for bin_home in ["/opt/rust/bin", "/home/username/bin", ".cargo/bin"] {
            let bin_home = Path::new(bin_home);
            assert_eq!(
                HomeDisplay::new(bin_home, Some(home)).to_string(),
                bin_home.display().to_string()
            );
        }

        assert_eq!(
            HomeDisplay::new(&cargo_bin_dir, None).to_string(),
            cargo_bin_dir.display().to_string()
        );
    }

    #[test]
    fn install_bins_creates_cargo_home() {
        let root_dir = test_dir().unwrap();
        let cargo_home = root_dir.path().join("cargo");
        let rustup_home = root_dir.path().join("rustup");
        let mut vars = HashMap::new();
        vars.env("CARGO_HOME", cargo_home.to_string_lossy().to_string());
        vars.env("RUSTUP_HOME", rustup_home);
        let tp = TestProcess::with_vars(vars);
        super::install_bins(&tp.process, &cargo_home.join("bin"), false).unwrap();
        assert!(cargo_home.exists());
    }
}
