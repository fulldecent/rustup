use std::{
    env::consts::EXE_SUFFIX,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use anstream::ColorChoice;
use anstyle::Style;
use anyhow::{Context as _, Error, anyhow, bail};
use clap::{
    Args, CommandFactory, Parser, Subcommand, ValueEnum,
    builder::{PossibleValue, ValueHint},
};
use clap_cargo::style::{ERROR, GOOD, TRANSIENT, WARN};
use clap_complete::Shell;
use futures_util::stream::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, Registry, reload::Handle};

use crate::{
    cli::{
        common::{self, PackageUpdate, report_error, update_console_filter},
        completions, docs,
        help::{
            check_help, completions_help, doc_help, install_help, official_toolchain_arg_help,
            resolvable_toolchain_arg_help, run_help, rustup_help, show_active_toolchain_help,
            show_help, toolchain_help, toolchain_install_help, toolchain_link_help, topic_arg_help,
            update_help,
        },
        self_update,
    },
    command, component_for_bin,
    config::{ActiveSource, Cfg, OverrideFile, no_toolchain_error},
    dist::{
        DistOptions, PartialChannelToolchainName, Profile, Switch, TargetTuple,
        download::DownloadCfg,
        manifest::{Component, ManifestWithHash},
    },
    errors::RustupError,
    install::{InstallMethod, UpdateStatus},
    process::Process,
    toolchain::{
        ChannelToolchain, CustomToolchainName, Override, PartialToolchainName,
        PartialToolchainNameOrPath, Toolchain, ToolchainNameExt, ToolchainNameOrPath,
    },
    utils::{self, ExitCode},
};

const TOOLCHAIN_OVERRIDE_ERROR: &str = "To override the toolchain using the 'rustup +toolchain' syntax, \
                        make sure to prefix the toolchain override with a '+'";

fn handle_epipe(res: anyhow::Result<ExitCode>) -> anyhow::Result<ExitCode> {
    match res {
        Err(e) => {
            let root = e.root_cause();
            if let Some(io_err) = root.downcast_ref::<io::Error>()
                && io_err.kind() == io::ErrorKind::BrokenPipe
            {
                return Ok(ExitCode::SUCCESS);
            }
            Err(e)
        }
        res => res,
    }
}

/// The Rust toolchain installer
#[derive(Debug, Parser)]
#[command(
    name = "rustup",
    bin_name = "rustup[EXE]",
    version = common::version(),
    before_help = format!("rustup {}", common::version()),
    after_help = rustup_help(),
    styles = clap_cargo::style::CLAP_STYLING,
)]
pub(super) struct Rustup {
    /// Set log level to 'DEBUG' if 'RUSTUP_LOG' is unset
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,

    /// Disable progress output, set log level to 'WARN' if 'RUSTUP_LOG' is unset
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,

    /// Release channel (e.g. +stable) or custom toolchain to set override
    #[arg(
        name = "+toolchain",
        value_parser = plus_toolchain_value_parser,
        value_hint = ValueHint::Other,
    )]
    plus_toolchain: Option<Override<PartialToolchainNameOrPath>>,

    #[command(subcommand)]
    subcmd: Option<RustupSubcmd>,
}

fn plus_toolchain_value_parser(
    s: &str,
) -> clap::error::Result<Override<PartialToolchainNameOrPath>> {
    use clap::{Error, error::ErrorKind};
    if let Some(stripped) = s.strip_prefix('+') {
        Override::<PartialToolchainNameOrPath>::from_str(stripped)
            .map_err(|e| Error::raw(ErrorKind::InvalidValue, e))
    } else {
        Err(Error::raw(
            ErrorKind::InvalidSubcommand,
            format!(
                "\"{s}\" is not a valid subcommand, so it was interpreted as a toolchain name, but it is also invalid. {TOOLCHAIN_OVERRIDE_ERROR}"
            ),
        ))
    }
}

#[derive(Debug, Subcommand)]
#[command(name = "rustup", bin_name = "rustup[EXE]")]
enum RustupSubcmd {
    /// Install or update the given toolchains, or by default the active toolchain
    #[command(after_help = install_help(), aliases = ["add"])]
    Install {
        #[command(flatten)]
        opts: UpdateOpts,
    },

    /// Uninstall the given toolchains
    #[command(aliases = ["remove", "rm", "delete", "del"])]
    Uninstall {
        #[command(flatten)]
        opts: UninstallOpts,
    },

    /// Dump information about the build
    #[command(hide = true)]
    DumpTestament,

    /// Install, uninstall, or list toolchains
    Toolchain {
        #[command(subcommand)]
        subcmd: ToolchainSubcmd,
    },

    /// Print the toolchain selected for the current directory
    #[command(alias = "resolve", after_help = show_help())]
    Show {
        #[command(subcommand)]
        subcmd: Option<ShowSubcmd>,
    },

    /// Update Rust toolchains
    #[command(
        after_help = update_help(),
        aliases = ["upgrade", "up"],
    )]
    Update {
        /// Toolchain name, such as 'stable', 'nightly', or '1.8.0'. For more information see `rustup help toolchain`
        #[arg(num_args = 1.., value_parser = update_toolchain_value_parser)]
        toolchain: Vec<PartialChannelToolchainName>,

        /// Allow downgrading the toolchain
        #[arg(long)]
        allow_downgrade: bool,

        /// Force an update, even if some components are missing
        #[arg(long)]
        force: bool,

        /// Install toolchains that require an emulator. See https://github.com/rust-lang/rustup/wiki/Non-host-toolchains
        #[arg(long)]
        force_non_host: bool,

        /// Indicate update status via exit code
        #[arg(long)]
        check: bool,
    },

    /// Check for updates to Rust toolchains
    #[command(after_help = check_help())]
    Check,

    /// Modify a toolchain's supported targets
    Target {
        #[command(subcommand)]
        subcmd: TargetSubcmd,
    },

    /// Modify a toolchain's installed components
    Component {
        #[command(subcommand)]
        subcmd: ComponentSubcmd,
    },

    /// Run a command under a toolchain
    #[command(after_help = run_help(), trailing_var_arg = true)]
    Run {
        /// Toolchain name and command, or a command resolved from rust-toolchain.toml
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Display which binary will be run for a given command
    Which {
        command: String,

        #[arg(long, help = resolvable_toolchain_arg_help())]
        toolchain: Option<PartialToolchainName>,
    },

    /// Open the documentation for the current toolchain
    #[command(
        alias = "docs",
        after_help = doc_help(),
    )]
    Doc {
        /// Only print the path to the documentation
        #[arg(long)]
        path: bool,

        /// Serve the documentation over a local HTTP server instead of
        /// opening it directly as a `file://` URL
        #[arg(long, conflicts_with = "path")]
        serve: bool,

        #[arg(long, help = official_toolchain_arg_help())]
        toolchain: Option<PartialChannelToolchainName>,

        #[arg(help = topic_arg_help())]
        topic: Option<String>,

        #[command(flatten)]
        page: docs::DocPage,
    },

    /// View the man page for a given command
    #[cfg(not(windows))]
    Man {
        command: String,

        #[arg(long, help = official_toolchain_arg_help())]
        toolchain: Option<PartialChannelToolchainName>,
    },

    /// Modify the rustup installation
    Self_ {
        #[command(subcommand)]
        subcmd: SelfSubcmd,
    },

    /// Alter rustup settings
    Set {
        #[command(subcommand)]
        subcmd: SetSubcmd,
    },

    /// Generate tab-completion scripts for your shell
    #[command(after_help = completions_help(), arg_required_else_help = true)]
    Completions {
        shell: Shell,

        #[arg(default_value = "rustup")]
        command: CompletionCommand,
    },
}

fn update_toolchain_value_parser(s: &str) -> anyhow::Result<PartialChannelToolchainName> {
    PartialChannelToolchainName::from_str(s)
}

impl RustupSubcmd {
    fn allow_auto_install(&self) -> bool {
        match self {
            // These subcommands execute or rely on the active toolchain, so auto-installing it when
            // missing may be reasonable depending on the user's decision.
            #[cfg(not(windows))]
            Self::Man { .. } => true,
            Self::Doc { .. } | Self::Run { .. } => true,

            // These subcommands don't require the active toolchain, so auto-installing it should be
            // disabled to avoid surprises.
            Self::Check { .. }
            | Self::Completions { .. }
            | Self::Component { .. }
            | Self::DumpTestament
            | Self::Install { .. }
            | Self::Self_ { .. }
            | Self::Set { .. }
            | Self::Show { .. }
            | Self::Target { .. }
            | Self::Toolchain { .. }
            | Self::Uninstall { .. }
            | Self::Update { .. }
            | Self::Which { .. } => false,
        }
    }

    /// Returns whether rustup should warn about having no installed toolchains.
    fn should_warn_empty_setup(&self) -> bool {
        match self {
            // These subcommands are not about toolchains, so the hint would be noise.
            Self::Completions { .. } | Self::DumpTestament | Self::Self_ { .. } => false,

            // For all other subcommands, the hint may be useful if rustup is still unusable after
            // the command has completed.
            Self::Check { .. }
            | Self::Component { .. }
            | Self::Doc { .. }
            | Self::Install { .. }
            | Self::Run { .. }
            | Self::Set { .. }
            | Self::Show { .. }
            | Self::Target { .. }
            | Self::Toolchain { .. }
            | Self::Uninstall { .. }
            | Self::Update { .. }
            | Self::Which { .. } => true,

            #[cfg(not(windows))]
            Self::Man { .. } => true,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ShowSubcmd {
    /// Show the active toolchain
    #[command(after_help = show_active_toolchain_help())]
    ActiveToolchain {
        /// Enable verbose output with rustc information
        #[arg(short, long)]
        verbose: bool,
    },

    /// Display the computed value of RUSTUP_HOME
    Home,

    /// Show the default profile used for the `rustup install` command
    Profile,
}

#[derive(Debug, Subcommand)]
#[command(
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = toolchain_help(),
)]
enum ToolchainSubcmd {
    /// List installed toolchains
    List {
        /// Enable verbose output with toolchain information
        #[arg(short, long)]
        verbose: bool,

        /// Force the output to be a single column
        #[arg(short, long, conflicts_with = "verbose")]
        quiet: bool,
    },

    /// Install or update the given toolchains, or by default the active toolchain
    #[command(aliases = ["update", "up", "add"], after_help = toolchain_install_help())]
    Install {
        #[command(flatten)]
        opts: UpdateOpts,
    },

    /// Uninstall the given toolchains
    #[command(aliases = ["remove", "rm", "delete", "del"])]
    Uninstall {
        #[command(flatten)]
        opts: UninstallOpts,
    },

    /// Create a custom toolchain by symlinking to a directory
    #[command(after_help = toolchain_link_help())]
    Link {
        /// Custom toolchain name
        toolchain: CustomToolchainName,

        /// Path to the directory
        path: PathBuf,
    },

    /// Remove a linked custom toolchain name
    Unlink {
        /// Custom toolchain name
        toolchain: CustomToolchainName,
    },

    /// Write the active toolchain as an override file
    Pin {
        /// Write the toolchain name with host tuple
        #[arg(long)]
        qualified: bool,
    },
}

#[derive(Debug, Default, Args)]
struct UpdateOpts {
    #[arg(
        help = official_toolchain_arg_help(),
        num_args = 1..,
    )]
    toolchain: Vec<PartialChannelToolchainName>,

    #[arg(long, value_enum)]
    profile: Option<Profile>,

    /// Comma-separated list of components to be added on installation
    #[arg(short, long, value_delimiter = ',')]
    component: Vec<String>,

    /// Comma-separated list of targets to be added on installation
    #[arg(short, long, value_delimiter = ',')]
    target: Vec<String>,

    /// Don't try to update the installed toolchain
    #[arg(long)]
    no_update: bool,

    /// Force an update, even if some components are missing
    #[arg(long)]
    force: bool,

    /// Allow rustup to downgrade the toolchain to satisfy your component choice
    #[arg(long)]
    allow_downgrade: bool,

    /// Install toolchains that require an emulator. See https://github.com/rust-lang/rustup/wiki/Non-host-toolchains
    #[arg(long)]
    force_non_host: bool,

    /// Indicate update status via exit code
    #[arg(long)]
    check: bool,
}

#[derive(Debug, Default, Args)]
struct UninstallOpts {
    #[arg(
        help = resolvable_toolchain_arg_help(),
        required = true,
        num_args = 1..,
    )]
    toolchain: Vec<PartialToolchainName>,
}

#[derive(Debug, Subcommand)]
#[command(arg_required_else_help = true, subcommand_required = true)]
enum TargetSubcmd {
    /// List installed and available targets
    List {
        #[arg(
            long,
            help = official_toolchain_arg_help(),
        )]
        toolchain: Option<PartialChannelToolchainName>,

        /// List only installed targets
        #[arg(long)]
        installed: bool,

        /// Force the output to be a single column
        #[arg(long, short)]
        quiet: bool,
    },

    /// Add a target to a Rust toolchain
    #[command(alias = "install")]
    Add {
        /// List of targets to install; "all" installs all available targets
        #[arg(required = true, num_args = 1..)]
        target: Vec<String>,

        #[arg(long, help = official_toolchain_arg_help())]
        toolchain: Option<PartialChannelToolchainName>,
    },

    /// Remove a target from a Rust toolchain
    #[command(aliases = ["uninstall", "rm", "delete", "del"])]
    Remove {
        /// List of targets to uninstall
        #[arg(required = true, num_args = 1..)]
        target: Vec<TargetTuple>,

        #[arg(long, help = official_toolchain_arg_help())]
        toolchain: Option<PartialChannelToolchainName>,
    },
}

#[derive(Debug, Subcommand)]
#[command(arg_required_else_help = true, subcommand_required = true)]
enum ComponentSubcmd {
    /// List installed and available components
    List {
        #[arg(long, help = official_toolchain_arg_help())]
        toolchain: Option<PartialChannelToolchainName>,

        /// List only installed components
        #[arg(long)]
        installed: bool,

        /// Force the output to be a single column
        #[arg(long, short)]
        quiet: bool,
    },

    /// Add a component to a Rust toolchain
    Add {
        #[arg(required = true, num_args = 1..)]
        component: Vec<String>,

        #[arg(long, help = official_toolchain_arg_help())]
        toolchain: Option<PartialChannelToolchainName>,

        #[arg(long)]
        target: Option<String>,
    },

    /// Remove a component from a Rust toolchain
    #[command(aliases = ["uninstall", "rm", "delete", "del"])]
    Remove {
        #[arg(required = true, num_args = 1..)]
        component: Vec<String>,

        #[arg(long, help = official_toolchain_arg_help())]
        toolchain: Option<PartialChannelToolchainName>,

        #[arg(long)]
        target: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
#[command(
    name = "self",
    arg_required_else_help = true,
    subcommand_required = true
)]
enum SelfSubcmd {
    /// Uninstall rustup
    Uninstall {
        /// Disable confirmation prompt
        #[arg(short = 'y', long = "yes")]
        no_prompt: bool,

        /// Do not clean up the `PATH` environment variable
        #[arg(long)]
        no_modify_path: bool,
    },

    /// Upgrade the internal data format
    UpgradeData,
}

#[derive(Debug, Subcommand)]
#[command(arg_required_else_help = true, subcommand_required = true)]
enum SetSubcmd {
    /// The tuple used to identify toolchains when not specified
    DefaultHost { host_tuple: String },

    /// The default components installed with a toolchain
    Profile {
        #[arg(value_enum, default_value_t)]
        profile_name: Profile,
    },

    /// The auto toolchain install mode
    AutoInstall {
        #[arg(value_enum, default_value_t)]
        auto_install_mode: Switch,
    },

    /// The new stable release hint mode
    ReleaseHint {
        #[arg(value_enum, default_value_t)]
        release_hint_mode: Switch,
    },
}

#[tracing::instrument(level = "trace", fields(args = format!("{:?}", process.args_os().collect::<Vec<_>>())), skip(process, console_filter))]
pub async fn main(
    current_dir: PathBuf,
    process: &Process,
    console_filter: Handle<EnvFilter, Registry>,
) -> anyhow::Result<ExitCode> {
    let cfg = &mut Cfg::from_env(current_dir, true, false, process)?;
    clap_complete::CompleteEnv::with_factory(|| completions::rustup_command(cfg))
        .var("RUSTUP_COMPLETE")
        .bin("rustup")
        .complete();

    use clap::error::ErrorKind::*;
    let matches = match Rustup::try_parse_from(process.args_os()) {
        Ok(matches) => matches,
        Err(err) if err.kind() == DisplayHelp => {
            write!(process.stdout().lock(), "{}", err.render().ansi())?;
            return Ok(ExitCode::SUCCESS);
        }
        Err(err) if err.kind() == DisplayVersion => {
            write!(process.stdout().lock(), "{}", err.render().ansi())?;
            display_version(cfg).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Err(err) => {
            if [
                InvalidSubcommand,
                UnknownArgument,
                DisplayHelpOnMissingArgumentOrSubcommand,
            ]
            .contains(&err.kind())
            {
                write!(process.stdout().lock(), "{}", err.render().ansi())?;
            } else {
                write!(process.stderr().lock(), "{}", err.render().ansi())?;
            }
            return Ok(ExitCode::FAILURE);
        }
    };

    update_console_filter(process, &console_filter, matches.quiet, matches.verbose);

    let Some(subcmd) = matches.subcmd else {
        let help = Rustup::command().render_long_help();
        writeln!(process.stderr().lock(), "{}", help.ansi())?;
        return Ok(ExitCode::FAILURE);
    };

    cfg.quiet = matches.quiet;
    cfg.allow_auto_install = subcmd.allow_auto_install();
    cfg.toolchain_override = matches.plus_toolchain;

    let should_warn = subcmd.should_warn_empty_setup();

    let should_notify = !matches.quiet
        && !matches!(
            subcmd,
            RustupSubcmd::Update { .. } | RustupSubcmd::Install { .. }
        );

    let exit_code = match subcmd {
        RustupSubcmd::DumpTestament => common::dump_testament(process),
        RustupSubcmd::Install { opts } => update(cfg, opts, true).await,
        RustupSubcmd::Uninstall { opts } => toolchain_remove(cfg, opts).await,
        RustupSubcmd::Show { subcmd } => handle_epipe(match subcmd {
            None => show(cfg),
            Some(ShowSubcmd::ActiveToolchain { verbose }) => {
                show_active_toolchain(cfg, verbose).await
            }
            Some(ShowSubcmd::Home) => show_rustup_home(cfg),
            Some(ShowSubcmd::Profile) => {
                writeln!(process.stdout().lock(), "{}", cfg.get_profile()?)?;
                Ok(ExitCode::SUCCESS)
            }
        }),
        RustupSubcmd::Update {
            toolchain,
            allow_downgrade,
            force,
            force_non_host,
            check,
        } => {
            update(
                cfg,
                UpdateOpts {
                    toolchain,
                    allow_downgrade,
                    force,
                    force_non_host,
                    check,
                    ..UpdateOpts::default()
                },
                false,
            )
            .await
        }
        RustupSubcmd::Toolchain { subcmd } => match subcmd {
            ToolchainSubcmd::Install { opts } => update(cfg, opts, true).await,
            ToolchainSubcmd::List { verbose, quiet } => {
                handle_epipe(common::list_toolchains(cfg, verbose, quiet).await)
            }
            ToolchainSubcmd::Link { toolchain, path } => {
                toolchain_link(cfg, &toolchain, &path).await
            }
            ToolchainSubcmd::Unlink { toolchain } => toolchain_unlink(cfg, &toolchain),
            ToolchainSubcmd::Uninstall { opts } => toolchain_remove(cfg, opts).await,
            ToolchainSubcmd::Pin { qualified } => pin_active_toolchain(qualified, cfg),
        },
        RustupSubcmd::Check => check_updates(cfg).await,
        RustupSubcmd::Target { subcmd } => match subcmd {
            TargetSubcmd::List {
                toolchain,
                installed,
                quiet,
            } => handle_epipe(target_list(cfg, toolchain, installed, quiet).await),
            TargetSubcmd::Add { target, toolchain } => target_add(cfg, target, toolchain).await,
            TargetSubcmd::Remove { target, toolchain } => {
                target_remove(cfg, target, toolchain).await
            }
        },
        RustupSubcmd::Component { subcmd } => match subcmd {
            ComponentSubcmd::List {
                toolchain,
                installed,
                quiet,
            } => handle_epipe(component_list(cfg, toolchain, installed, quiet).await),
            ComponentSubcmd::Add {
                component,
                toolchain,
                target,
            } => component_add(cfg, component, toolchain, target).await,
            ComponentSubcmd::Remove {
                component,
                toolchain,
                target,
            } => component_remove(cfg, component, toolchain, target).await,
        },
        RustupSubcmd::Run { args } => run(cfg, args).await.map(ExitCode::from),
        RustupSubcmd::Which { command, toolchain } => which(cfg, &command, toolchain).await,
        RustupSubcmd::Doc {
            path,
            serve,
            toolchain,
            topic,
            page,
        } => docs::doc(cfg, path, serve, toolchain, topic.as_deref(), &page).await,
        #[cfg(not(windows))]
        RustupSubcmd::Man { command, toolchain } => docs::man(cfg, &command, toolchain).await,
        RustupSubcmd::Self_ { subcmd } => match subcmd {
            SelfSubcmd::Uninstall {
                no_prompt,
                no_modify_path,
            } => self_update::uninstall(no_prompt, no_modify_path, cfg),
            SelfSubcmd::UpgradeData => cfg.upgrade_data().map(|_| ExitCode::SUCCESS),
        },
        RustupSubcmd::Set { subcmd } => match subcmd {
            SetSubcmd::DefaultHost { host_tuple } => cfg
                .set_default_host_tuple(host_tuple)
                .map(|_| ExitCode::SUCCESS),
            SetSubcmd::Profile { profile_name } => {
                cfg.set_profile(profile_name).map(|_| ExitCode::SUCCESS)
            }
            SetSubcmd::AutoInstall { auto_install_mode } => cfg
                .set_auto_install(auto_install_mode)
                .map(|_| ExitCode::SUCCESS),
            SetSubcmd::ReleaseHint { release_hint_mode } => cfg
                .set_release_hint(release_hint_mode)
                .map(|_| ExitCode::SUCCESS),
        },
        RustupSubcmd::Completions { shell, command } => {
            output_completion_script(shell, command, process)
        }
    }?;

    if should_warn && cfg.list_toolchains(true)?.is_empty() {
        warn!(
            "no toolchain installed\nhelp: run 'rustup toolchain install stable' to download the latest stable release of Rust, then name it in rust-toolchain.toml"
        );
    }

    if should_notify && let Err(e) = cfg.notify_release() {
        warn!("could not check if there is a new Rust release: {e}");
    }

    Ok(exit_code)
}

async fn check_updates(cfg: &Cfg<'_>) -> anyhow::Result<ExitCode> {
    let t = cfg.process.stdout();
    let use_colors = matches!(t.color_choice(), ColorChoice::Auto | ColorChoice::Always);
    let mut update_available = false;
    let channels = cfg.list_tracking_channels()?;
    let channels_len = channels.len();

    // Check updates for all channels unless `RUSTUP_CONCURRENT_DOWNLOADS` is set to 1,
    // since the overhead of spawning many concurrent tasks here is acceptable.
    let concurrent_downloads = match cfg.process.concurrent_downloads() {
        Some(1) => 1,
        Some(_) | None => channels_len,
    };

    let (bold, transient, error, good, warn) = if use_colors {
        (Style::new().bold(), TRANSIENT, ERROR, GOOD, WARN)
    } else {
        Default::default()
    };

    // Ensure that `.buffered()` is never called with 0 as this will cause a hang.
    // See: https://github.com/rust-lang/futures-rs/pull/1194#discussion_r209501774
    if channels_len > 0 {
        let multi_progress_bars =
            MultiProgress::with_draw_target(cfg.process.progress_draw_target());
        // Help avoid flickering by moving the cursor instead of clearing the line.
        multi_progress_bars.set_move_cursor(true);
        let semaphore = Arc::new(Semaphore::new(concurrent_downloads));
        let channels = tokio_stream::iter(channels).map(|distributable| {
            let msg = format!("{bold}{} - {bold:#}", distributable.desc());
            let status = "Checking...";
            let template = format!(
                "{{msg}}{transient}{status}{transient:#} {transient}{{spinner}}{transient:#}"
            );
            let pb = multi_progress_bars.add(ProgressBar::new(1));
            pb.set_style(
                ProgressStyle::with_template(template.as_str())
                    .unwrap()
                    .tick_chars(r"|/-\ "),
            );
            pb.set_message(msg.clone());
            pb.enable_steady_tick(Duration::from_millis(100));

            let sem = semaphore.clone();
            async move {
                let _permit = sem.acquire().await.unwrap();
                let current_version = distributable.show_version()?;
                let dist_version = match distributable.fetch_dist_manifest().await? {
                    Some(ManifestWithHash { manifest, .. }) => {
                        Some(manifest.get_rust_version()?.to_string())
                    }
                    None => None,
                };
                let mut update_a = false;

                let template = match (current_version, dist_version) {
                    (None, None) => {
                        let status = "cannot identify installed or update versions";
                        format!("{msg}{error}{status}{error:#}")
                    }
                    (Some(cv), None) => {
                        let status = "up to date";
                        format!("{msg}{good}{status}{good:#}: {cv}")
                    }
                    (Some(cv), Some(dv)) => {
                        let status = "update available";
                        update_a = true;
                        format!("{msg}{warn}{status}{warn:#}: {cv} -> {dv}")
                    }
                    (None, Some(dv)) => {
                        let status = "update available";
                        update_a = true;
                        format!("{msg}{warn}{status}{warn:#}: (unknown version) -> {dv}")
                    }
                };
                pb.set_style(ProgressStyle::with_template(template.as_str()).unwrap());
                pb.finish();
                Ok::<(bool, String), Error>((update_a, template))
            }
        });

        // If we are running in a TTY, we can use `buffer_unordered` since
        // displaying the output in the correct order is already handled by
        // `indicatif`.
        let has_progress_bars = !multi_progress_bars.is_hidden();
        let channels = if has_progress_bars {
            channels
                .buffer_unordered(channels_len)
                .collect::<Vec<_>>()
                .await
        } else {
            channels
                .buffered(concurrent_downloads)
                .collect::<Vec<_>>()
                .await
        };

        let t = cfg.process.stdout();
        for result in channels {
            let (update_a, message) = result?;
            if update_a {
                update_available = true;
            }
            if !has_progress_bars {
                writeln!(t.lock(), "{message}")?;
            }
        }
        if has_progress_bars {
            // HACK: Add a final newline to avoid possible display issues with `indicatif` in
            // certain terminal emulators. Currently, `indicatif` puts a series of spaces at the end
            // of the line, forcing any output after the progress bar to be on a second line. This
            // might not be desirable in all cases.
            writeln!(t.lock())?;
        }
    }

    Ok(if update_available {
        ExitCode::UPDATES_AVAILABLE
    } else {
        ExitCode::SUCCESS
    })
}

async fn update(
    cfg: &mut Cfg<'_>,
    opts: UpdateOpts,
    ensure_active_toolchain: bool,
) -> anyhow::Result<ExitCode> {
    let mut exit_code = ExitCode::SUCCESS;

    common::warn_if_host_is_emulated(cfg.process);
    let force_non_host = opts.force_non_host;
    cfg.profile_override = opts.profile;

    let cfg = &cfg;
    if cfg.get_profile()? == Profile::Complete {
        warn!("{}", common::WARN_COMPLETE_PROFILE);
    }

    let names = opts.toolchain;
    if !names.is_empty() {
        for name in names {
            // This needs another pass to fix it all up
            if !name.target.is_empty() {
                let host_arch = TargetTuple::from_host_or_build(cfg.process);
                let target_tuple = name.clone().complete(&host_arch)?.target;
                common::check_non_host_toolchain(
                    name.to_string(),
                    &host_arch,
                    &target_tuple,
                    force_non_host,
                )?;
            }
            let desc = name.clone().complete(&cfg.default_host_tuple()?)?;

            let components = opts.component.iter().map(|s| &**s).collect::<Vec<_>>();
            let targets = opts.target.iter().map(|s| &**s).collect::<Vec<_>>();
            let dist_opts = DistOptions::new(
                &components,
                &targets,
                &desc,
                cfg.get_profile()?,
                opts.force,
                cfg,
            )?;

            let status = match ChannelToolchain::new(cfg, desc.clone()) {
                Ok(d) => {
                    if !opts.no_update {
                        InstallMethod::Dist(dist_opts.for_update(&d, opts.allow_downgrade))
                            .install(None)
                            .await?
                    } else {
                        UpdateStatus::Unchanged
                    }
                }
                Err(RustupError::ToolchainNotInstalled { .. }) => {
                    ChannelToolchain::install(dist_opts).await?.status
                }
                Err(e) => Err(e)?,
            };

            writeln!(cfg.process.stdout().lock())?;
            common::show_channel_update(
                cfg,
                PackageUpdate::Toolchain(desc.clone()),
                Ok(status),
            )?;
        }
    } else if ensure_active_toolchain {
        let (toolchain, source) = cfg.ensure_active_toolchain(force_non_host, true).await?;
        info!("the active toolchain `{}` has been installed", *toolchain);
        info!("it's active because: {}", source.to_reason());
    } else {
        exit_code &= common::update_all_channels(cfg, opts.force, opts.check).await?;

        info!("cleaning up downloads & tmp directories");
        utils::delete_dir_contents_following_links(&cfg.download_dir);
        DownloadCfg::new(cfg).tmp_cx.clean();
    }

    Ok(exit_code)
}

fn split_run_args(
    cfg: &Cfg<'_>,
    args: &[String],
) -> anyhow::Result<(Option<PartialToolchainNameOrPath>, Vec<String>)> {
    let Some((first, rest)) = args.split_first() else {
        bail!("rustup run requires a command");
    };

    // A leading +name, -name, or path is an explicit toolchain argument.
    // An uninstalled custom name is the command, so `rustup run cargo build`
    // resolves from rust-toolchain.toml instead of looking up a toolchain
    // called cargo.
    if first.starts_with(['+', '-']) || first.contains(['/', '\\']) {
        let toolchain = PartialToolchainNameOrPath::from_str(first)?;
        if rest.is_empty() {
            bail!("rustup run requires a command");
        }
        return Ok((Some(toolchain), rest.to_vec()));
    }

    if let Ok(parsed) = PartialToolchainNameOrPath::from_str(first) {
        let explicit = match parsed {
            PartialToolchainNameOrPath::Path(_) => true,
            PartialToolchainNameOrPath::Named(PartialToolchainName::Channel(_)) => true,
            PartialToolchainNameOrPath::Named(PartialToolchainName::Custom(ref custom)) => {
                ToolchainNameOrPath::from(custom.clone())
                    .path(cfg)
                    .symlink_metadata()
                    .is_ok()
            }
        };
        if explicit {
            if rest.is_empty() {
                bail!("rustup run requires a command");
            }
            return Ok((
                Some(PartialToolchainNameOrPath::from_str(first)?),
                rest.to_vec(),
            ));
        }
    }

    Ok((None, args.to_vec()))
}

async fn run(cfg: &Cfg<'_>, args: Vec<String>) -> anyhow::Result<ExitStatus> {
    let (toolchain, command) = split_run_args(cfg, &args)?;
    let toolchain = match toolchain {
        Some(toolchain) => {
            let toolchain = toolchain.complete(&cfg.default_host_tuple()?)?;
            Toolchain::from_local(toolchain, true, cfg).await?.inner
        }
        None => {
            let (name, source) = cfg.ensure_active_toolchain(false, false).await?;
            name.warn_auto_install(cfg.process);
            Toolchain::with_source(cfg, name.inner, &source)?
        }
    };
    let cmd = toolchain.command(&command[0])?;
    command::run_command_for_dir(cmd, &command[0], &command[1..])
}

async fn which(
    cfg: &Cfg<'_>,
    binary: &str,
    toolchain: Option<PartialToolchainName>,
) -> anyhow::Result<ExitCode> {
    let (toolchain, _) = cfg
        .local_toolchain(match toolchain {
            Some(name) => Some((
                name.complete(&cfg.default_host_tuple()?)?.into(),
                ActiveSource::CommandLine, // From --toolchain option
            )),
            None => None,
        })
        .await?;

    let binary_path = toolchain.binary_file(binary);
    if utils::is_file(&binary_path) {
        writeln!(cfg.process.stdout().lock(), "{}", binary_path.display())?;
        return Ok(ExitCode::SUCCESS);
    }

    let toolchain_name = toolchain.name();
    let Some(component_name) = component_for_bin(binary) else {
        return Err(anyhow!(
            "unknown binary '{binary}' in toolchain '{toolchain_name}'",
        ));
    };

    let selector = match cfg.active_toolchain() {
        Ok(Some((t, _))) if &t == toolchain_name => String::new(),
        _ => format!("--toolchain {} ", toolchain.name()),
    };

    Err(anyhow!(
        "'{binary}' is not installed for the toolchain '{toolchain_name}'.\nhelp: run `rustup component add {selector}{component_name}` to install it"
    ))
}

fn show(cfg: &Cfg<'_>) -> anyhow::Result<ExitCode> {
    let Some((name, _)) = cfg.active_toolchain()? else {
        return Err(no_toolchain_error(cfg.process));
    };
    writeln!(cfg.process.stdout().lock(), "{name}")?;
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "trace", skip_all)]
async fn show_active_toolchain(cfg: &Cfg<'_>, verbose: bool) -> anyhow::Result<ExitCode> {
    match cfg.maybe_ensure_active_toolchain(None).await? {
        Some((toolchain_name, source)) => {
            let toolchain = Toolchain::with_source(cfg, toolchain_name, &source)?;
            if verbose {
                writeln!(
                    cfg.process.stdout().lock(),
                    "{}\nactive because: {}\ncompiler: {}\npath: {}",
                    toolchain.name(),
                    source.to_reason(),
                    toolchain.rustc_version(),
                    toolchain.path().display()
                )?;
            } else {
                writeln!(
                    cfg.process.stdout().lock(),
                    "{} ({})",
                    toolchain.name(),
                    source.to_reason()
                )?;
            }
        }
        None => return Err(anyhow!("no active toolchain")),
    }
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "trace", skip_all)]
fn show_rustup_home(cfg: &Cfg<'_>) -> anyhow::Result<ExitCode> {
    writeln!(cfg.process.stdout().lock(), "{}", cfg.rustup_dir.display())?;
    Ok(ExitCode::SUCCESS)
}

async fn target_list(
    cfg: &Cfg<'_>,
    toolchain: Option<PartialChannelToolchainName>,
    installed_only: bool,
    quiet: bool,
) -> anyhow::Result<ExitCode> {
    let toolchain = toolchain.map(|desc| (desc, ActiveSource::CommandLine));

    // If a toolchain is Distributable, we can assume it has a manifest and thus print all possible targets and the installed ones.
    // However, if it is a custom toolchain, we can only print the installed targets.
    // NB: this decision is made based on the absence of a manifest in custom toolchains.
    if let Ok(distributable) = ChannelToolchain::from_partial(toolchain.clone(), cfg).await {
        common::list_items(
            distributable.components()?.into_iter().filter_map(|c| {
                if c.component.short_name() == "rust-std" && c.available {
                    c.component.target.map(|target| (target, c.installed))
                } else {
                    None
                }
            }),
            installed_only,
            quiet,
            cfg.process,
        )
    } else {
        let toolchain = cfg.toolchain_from_partial(toolchain).await?.0;
        common::list_items(
            toolchain.installed_targets()?.iter().map(|s| (s, true)),
            installed_only,
            quiet,
            cfg.process,
        )
    }
}

async fn target_add(
    cfg: &Cfg<'_>,
    targets: Vec<String>,
    toolchain: Option<PartialChannelToolchainName>,
) -> anyhow::Result<ExitCode> {
    // XXX: long term move this error to cli ? the normal .into doesn't work
    // because Result here is the wrong sort and expression type ascription
    // isn't a feature yet.
    // list_components *and* add_components would both be inappropriate for
    // custom toolchains.
    let distributable = ChannelToolchain::from_partial(
        toolchain.map(|desc| (desc, ActiveSource::CommandLine)),
        cfg,
    )
    .await?;

    let all = targets.iter().any(|it| it == "all");
    if all && targets.len() != 1 {
        return Err(anyhow!(
            "`rustup target add {}` includes `all`",
            targets.join(" ")
        ));
    }

    if all {
        distributable
            .add_components(distributable.components()?.into_iter().filter_map(|c| {
                (c.available && !c.installed && c.component.short_name() == "rust-std")
                    .then_some(Ok(c.component))
            }))
            .await?;

        return Ok(ExitCode::SUCCESS);
    }

    distributable
        .add_components(
            targets
                .into_iter()
                .map(|target| Ok(Component::std(TargetTuple::new(target)))),
        )
        .await?;

    Ok(ExitCode::SUCCESS)
}

async fn target_remove(
    cfg: &Cfg<'_>,
    targets: Vec<TargetTuple>,
    toolchain: Option<PartialChannelToolchainName>,
) -> anyhow::Result<ExitCode> {
    let distributable = ChannelToolchain::from_partial(
        toolchain.map(|desc| (desc, ActiveSource::CommandLine)),
        cfg,
    )
    .await?;

    if targets.contains(&cfg.default_host_tuple()?) {
        warn!(
            "removing the default host target; proc-macros and build scripts might no longer build"
        );
    }

    let mut remaining_targets = distributable.toolchain.installed_targets()?;
    remaining_targets.retain(|it| !targets.contains(it));
    if remaining_targets.is_empty() {
        warn!("removing the last target; no build targets will be available");
    }

    distributable
        .remove_components(targets.into_iter().map(|c| Ok(Component::std(c))))
        .await?;
    Ok(ExitCode::SUCCESS)
}

async fn component_list(
    cfg: &Cfg<'_>,
    toolchain: Option<PartialChannelToolchainName>,
    installed_only: bool,
    quiet: bool,
) -> anyhow::Result<ExitCode> {
    let toolchain = toolchain.map(|desc| (desc, ActiveSource::CommandLine));

    // downcasting required because the toolchain files can name any toolchain
    if let Ok(distributable) = ChannelToolchain::from_partial(toolchain.clone(), cfg).await {
        common::list_items(
            distributable
                .components()?
                .into_iter()
                .filter_map(|c| c.available.then_some((c.name, c.installed))),
            installed_only,
            quiet,
            cfg.process,
        )
    } else {
        let toolchain = cfg.toolchain_from_partial(toolchain).await?.0;
        common::list_items(
            toolchain
                .installed_components()?
                .iter()
                .map(|s| (s.name(), true)),
            installed_only,
            quiet,
            cfg.process,
        )
    }
}

async fn component_add(
    cfg: &Cfg<'_>,
    components: Vec<String>,
    toolchain: Option<PartialChannelToolchainName>,
    target: Option<String>,
) -> anyhow::Result<ExitCode> {
    let distributable = ChannelToolchain::from_partial(
        toolchain.map(|desc| (desc, ActiveSource::CommandLine)),
        cfg,
    )
    .await?;

    let target = get_target(target, &distributable);

    distributable
        .add_components(
            components
                .into_iter()
                .map(|component| Component::try_new(&component, &distributable, target.as_ref())),
        )
        .await?;

    Ok(ExitCode::SUCCESS)
}

fn get_target(target: Option<String>, distributable: &ChannelToolchain<'_>) -> Option<TargetTuple> {
    target
        .map(TargetTuple::new)
        .or_else(|| Some(distributable.desc().target.clone()))
}

async fn component_remove(
    cfg: &Cfg<'_>,
    components: Vec<String>,
    toolchain: Option<PartialChannelToolchainName>,
    target: Option<String>,
) -> anyhow::Result<ExitCode> {
    let toolchain = toolchain.map(|desc| (desc, ActiveSource::CommandLine));
    let distributable = ChannelToolchain::from_partial(toolchain, cfg).await?;
    let target = get_target(target, &distributable);

    distributable
        .remove_components(
            components
                .iter()
                .map(|component| Component::try_new(component, &distributable, target.as_ref())),
        )
        .await?;
    Ok(ExitCode::SUCCESS)
}

async fn toolchain_link(
    cfg: &Cfg<'_>,
    toolchain: &CustomToolchainName,
    src: &Path,
) -> anyhow::Result<ExitCode> {
    cfg.ensure_toolchains_dir()?;
    let mut pathbuf = PathBuf::from(src);

    pathbuf.push("lib");
    utils::assert_is_directory(&pathbuf)?;
    pathbuf.pop();
    pathbuf.push("bin");
    utils::assert_is_directory(&pathbuf)?;
    pathbuf.push(format!("rustc{EXE_SUFFIX}"));
    utils::assert_is_file(&pathbuf)?;

    InstallMethod::Link {
        src: &cfg.current_dir.join(src),
        toolchain,
        cfg,
    }
    .install(None)
    .await?;

    Ok(ExitCode::SUCCESS)
}

fn toolchain_unlink(cfg: &Cfg<'_>, toolchain: &CustomToolchainName) -> anyhow::Result<ExitCode> {
    let path = ToolchainNameOrPath::from(toolchain.clone()).path(cfg);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("no linked toolchain named '{toolchain}'"))?;
    if !metadata.file_type().is_symlink() {
        bail!("'{toolchain}' is not a linked custom toolchain");
    }
    utils::remove_dir("toolchain link", &path)?;
    info!("unlinked toolchain {toolchain}");
    Ok(ExitCode::SUCCESS)
}

async fn toolchain_remove(cfg: &Cfg<'_>, opts: UninstallOpts) -> anyhow::Result<ExitCode> {
    let active_toolchain = cfg
        .maybe_ensure_active_toolchain(Some(false))
        .await
        .ok()
        .flatten()
        .map(|(it, _)| it);

    for toolchain_name in opts.toolchain {
        let toolchain_name = toolchain_name.complete(&cfg.default_host_tuple()?)?;

        if active_toolchain
            .as_ref()
            .is_some_and(|n| n == &toolchain_name)
        {
            warn!(
                "removing the active toolchain; a toolchain will be required for running Rust tools"
            );
        }

        Toolchain::ensure_removed(cfg, toolchain_name.into())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn pin_active_toolchain(qualified: bool, cfg: &Cfg<'_>) -> anyhow::Result<ExitCode> {
    let default_host = cfg.default_host_tuple()?;

    let Some((mut r#override, _)) = cfg.find_override_config()? else {
        anyhow::bail!("no toolchain is selected to pin");
    };
    if qualified {
        r#override.qualify(&default_host);
    }

    let r#override = OverrideFile::from(r#override);

    let rel_path = "rust-toolchain.toml";
    let path = cfg.process.current_dir()?.join(rel_path);
    if path
        .try_exists()
        .context("failed to check for existing override file")?
    {
        return Err(anyhow!(
            "found existing override file at '{rel_path}', refusing to overwrite"
        ));
    }

    let mut toml_buf = toml::ser::Buffer::new();
    r#override
        .serialize(toml::Serializer::new(&mut toml_buf))
        .context("failed to serialize override file to TOML")?;
    utils::write_file(
        "override file",
        &path,
        &format!(
            "# Check the docs at https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file\n\n{toml_buf}",
        ),
    )?;
    Ok(ExitCode::SUCCESS)
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum CompletionCommand {
    Rustup,
    Cargo,
}

impl ValueEnum for CompletionCommand {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Rustup, Self::Cargo]
    }

    fn to_possible_value<'a>(&self) -> Option<PossibleValue> {
        Some(match self {
            Self::Rustup => PossibleValue::new("rustup"),
            Self::Cargo => PossibleValue::new("cargo"),
        })
    }
}

impl fmt::Display for CompletionCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_possible_value() {
            Some(v) => write!(f, "{}", v.get_name()),
            None => unreachable!(),
        }
    }
}

fn output_completion_script(
    shell: Shell,
    command: CompletionCommand,
    process: &Process,
) -> anyhow::Result<ExitCode> {
    match command {
        CompletionCommand::Rustup => {
            clap_complete::generate(
                shell,
                &mut Rustup::command(),
                "rustup",
                &mut process.stdout().lock(),
            );
        }
        CompletionCommand::Cargo => {
            if let Shell::Zsh = shell {
                writeln!(process.stdout().lock(), "#compdef cargo")?;
            }

            let script = match shell {
                Shell::Bash => "/etc/bash_completion.d/cargo",
                Shell::Zsh => "/share/zsh/site-functions/_cargo",
                _ => {
                    return Err(anyhow!(
                        "{command} does not currently support completions for {shell}"
                    ));
                }
            };

            writeln!(
                process.stdout().lock(),
                "if command -v rustc >/dev/null 2>&1; then\n\
                    \tsource \"$(rustc --print sysroot)\"{script}\n\
                 fi",
            )?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

async fn display_version(cfg: &mut Cfg<'_>) -> anyhow::Result<()> {
    info!("this is the version for the rustup toolchain manager, not the rustc compiler");
    cfg.toolchain_override = cfg
        .process
        .args()
        .find_map(|arg| {
            arg.strip_prefix('+')
                .map(Override::<PartialToolchainNameOrPath>::from_str)
        })
        .transpose()?;

    match cfg.maybe_ensure_active_toolchain(None).await {
        Ok(Some((name, _))) => match Toolchain::new(cfg, name) {
            Ok(tc) => info!(
                "the currently active `rustc` version is `{}`",
                tc.rustc_version()
            ),
            Err(RustupError::ToolchainNotInstalled {
                name,
                is_active: true,
            }) => {
                info!("the active toolchain `{name}` is not installed");
            }
            Err(err) => report_error(
                &anyhow!(err).context("failed to display the current `rustc` version"),
                cfg.process,
            ),
        },
        Ok(None) => info!("no `rustc` is currently active"),
        Err(err) => report_error(
            &err.context("failed to display the current `rustc` version"),
            cfg.process,
        ),
    }

    Ok(())
}
