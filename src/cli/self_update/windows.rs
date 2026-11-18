use std::{
    borrow::Cow,
    env::{consts::EXE_SUFFIX, split_paths},
    ffi::{OsStr, OsString},
    fmt,
    fs::OpenOptions,
    io::{self, Write},
    mem,
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr, thread,
    time::Duration,
};

use anyhow::{Context, anyhow};
use cc::windows_registry::{find_tool, find_vs_version};
use tracing::{info, warn};
#[cfg(any(test, feature = "test"))]
use windows_registry::Value;
use windows_registry::{CURRENT_USER, HSTRING, Key};
use windows_result::WIN32_ERROR;
use windows_sys::Win32::{
    Foundation::{
        DuplicateHandle, ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    },
    Storage::FileSystem::{
        FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ, SYNCHRONIZE,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32, Process32First, Process32Next,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetCurrentProcess, GetCurrentProcessId, INFINITE, OpenProcess, WaitForSingleObject,
        },
    },
};

use super::{
    InstallOpts, report_error,
    stage::{self, PreparedUpdater, SelfUpdateLock},
};
use crate::{
    cli::{common, errors::CliError, markdown::md},
    dist::TargetTuple,
    download::DownloadOptions,
    process::{ColorableTerminal, Process},
    utils,
};

pub(crate) fn ensure_prompt(process: &Process) -> anyhow::Result<()> {
    writeln!(process.stdout().lock(),)?;
    writeln!(process.stdout().lock(), "Press the Enter key to continue.")?;
    common::read_line(process)?;
    Ok(())
}

fn choice(max: u8, process: &Process) -> anyhow::Result<Option<u8>> {
    write!(process.stdout().lock(), ">")?;

    let _ = io::stdout().flush();
    let input = common::read_line(process)?;

    let r = match str::parse(&input) {
        Ok(n) if n <= max => Some(n),
        _ => None,
    };

    writeln!(process.stdout().lock())?;
    Ok(r)
}

pub(crate) fn choose_vs_install(process: &Process) -> anyhow::Result<Option<VsInstallPlan>> {
    writeln!(
        process.stdout().lock(),
        "\n1) Quick install via the Visual Studio Community installer"
    )?;
    writeln!(
        process.stdout().lock(),
        "   (free for individuals, academic uses, and open source)."
    )?;
    writeln!(
        process.stdout().lock(),
        "\n2) Manually install the prerequisites"
    )?;
    writeln!(
        process.stdout().lock(),
        "   (for enterprise and advanced users)."
    )?;
    writeln!(
        process.stdout().lock(),
        "\n3) Don't install the prerequisites"
    )?;
    writeln!(
        process.stdout().lock(),
        "   (if you're targeting the GNU ABI).\n"
    )?;

    let choice = loop {
        if let Some(n) = choice(3, process)? {
            break n;
        }
        writeln!(process.stdout().lock(), "Select option 1, 2 or 3")?;
    };
    let plan = match choice {
        1 => Some(VsInstallPlan::Automatic),
        2 => Some(VsInstallPlan::Manual),
        _ => None,
    };
    Ok(plan)
}

pub(super) async fn maybe_install_msvc(
    term: &mut ColorableTerminal,
    no_prompt: bool,
    quiet: bool,
    opts: &InstallOpts<'_>,
    process: &Process,
) -> anyhow::Result<()> {
    let Some(plan) = msvc_check(opts, process) else {
        return Ok(());
    };

    if no_prompt {
        warn!("installing msvc toolchain without its prerequisites");
    } else if !quiet && plan == VsInstallPlan::Automatic {
        md(term, MSVC_AUTO_INSTALL_MESSAGE);
        match choose_vs_install(process)? {
            Some(VsInstallPlan::Automatic) => {
                match try_install_msvc(opts, process).await {
                    Err(e) => {
                        // Make sure the console doesn't exit before the user can
                        // see the error and give the option to continue anyway.
                        report_error(&e, process);
                        if !common::question_bool("\nContinue?", false, process)? {
                            info!("aborting installation");
                        }
                    }
                    Ok(ContinueInstall::No) => ensure_prompt(process)?,
                    _ => {}
                }
            }
            Some(VsInstallPlan::Manual) => {
                md(term, MSVC_MANUAL_INSTALL_MESSAGE);
                if !common::question_bool("\nContinue?", false, process)? {
                    info!("aborting installation");
                }
            }
            None => {}
        }
    } else {
        md(term, MSVC_MESSAGE);
        md(term, MSVC_MANUAL_INSTALL_MESSAGE);
        if !common::question_bool("\nContinue?", false, process)? {
            info!("aborting installation");
        }
    }

    Ok(())
}

static MSVC_MESSAGE: &str = r#"# Rust Visual C++ prerequisites

Rust requires the Microsoft C++ build tools for Visual Studio 2017 or
later, but they don't seem to be installed.

"#;

static MSVC_MANUAL_INSTALL_MESSAGE: &str = r#"
You can acquire the build tools by installing Microsoft Visual Studio.

    https://visualstudio.microsoft.com/downloads/

Check the box for "Desktop development with C++" which will ensure that the
needed components are installed. If your locale language is not English,
then additionally check the box for English under Language packs.

For more details see:

    https://rust-lang.github.io/rustup/installation/windows-msvc.html

_Install the C++ build tools before proceeding_.

If you will be targeting the GNU ABI or otherwise know what you are
doing then it is fine to continue installation without the build
tools, but otherwise, install the C++ build tools before proceeding.
"#;

static MSVC_AUTO_INSTALL_MESSAGE: &str = r#"# Rust Visual C++ prerequisites

Rust requires a linker and Windows API libraries but they don't seem to be available.

These components can be acquired through a Visual Studio installer.

"#;

#[derive(PartialEq, Eq)]
pub(crate) enum VsInstallPlan {
    Automatic,
    Manual,
}

// Provide guidance about setting up MSVC if it doesn't appear to be
// installed
pub(crate) fn msvc_check(opts: &InstallOpts<'_>, process: &Process) -> Option<VsInstallPlan> {
    // Test suite skips this since it's env dependent
    if process.var("RUSTUP_INIT_SKIP_MSVC_CHECK").is_ok() {
        return None;
    }

    let host_tuple = if let Some(tuple) = opts.default_host_tuple.as_ref() {
        tuple.to_owned()
    } else {
        TargetTuple::from_host_or_build(process).to_string()
    };
    let installing_msvc = host_tuple.contains("msvc");
    let have_msvc = find_tool(&host_tuple, "cl.exe").is_some();
    if installing_msvc && !have_msvc {
        // Visual Studio build tools are required.
        // If the user does not have Visual Studio installed and their host
        // machine is i686 or x86_64 then it's OK to try an auto install.
        // Otherwise a manual install will be required.
        let has_any_vs = find_vs_version().is_ok();
        let is_x86 = host_tuple.contains("i686") || host_tuple.contains("x86_64");
        if is_x86 && !has_any_vs {
            Some(VsInstallPlan::Automatic)
        } else {
            Some(VsInstallPlan::Manual)
        }
    } else {
        None
    }
}

#[derive(Debug, Eq, PartialEq)]
struct VsInstallError(i32);
impl std::error::Error for VsInstallError {}
impl fmt::Display for VsInstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // See https://docs.microsoft.com/en-us/visualstudio/install/use-command-line-parameters-to-install-visual-studio?view=vs-2022#error-codes
        let message = match self.0 {
            740 => "elevation required",
            1001 => "Visual Studio installer process is running",
            1003 => "Visual Studio is in use",
            1602 => "operation was canceled",
            1618 => "another installation running",
            1641 => "operation completed successfully, and reboot was initiated",
            3010 => {
                "operation completed successfully, but install requires reboot before it can be used"
            }
            5003 => "bootstrapper failed to download installer",
            5004 => "operation was canceled",
            5005 => "bootstrapper command-line parse error",
            5007 => "operation was blocked - the computer does not meet the requirements",
            8001 => "arm machine check failure",
            8002 => "background download precheck failure",
            8003 => "out of support selectable failure",
            8004 => "target directory failure",
            8005 => "verifying source payloads failure",
            8006 => "Visual Studio processes running",
            -1073720687 => "connectivity failure",
            -1073741510 => "Microsoft Visual Studio Installer was terminated",
            _ => "error installing Visual Studio",
        };
        write!(f, "{} (exit code {})", message, self.0)
    }
}
impl VsInstallError {
    const REBOOTING_NOW: Self = Self(1641);
    const REBOOT_REQUIRED: Self = Self(3010);
}

pub(crate) enum ContinueInstall {
    Yes,
    No,
}

/// Tries to install the needed Visual Studio components.
///
/// Returns `Ok(ContinueInstall::No)` if installing Visual Studio was successful
/// but the rustup install should not be continued at this time.
pub(crate) async fn try_install_msvc(
    opts: &InstallOpts<'_>,
    process: &Process,
) -> anyhow::Result<ContinueInstall> {
    // download the installer
    let visual_studio_url = utils::parse_url("https://aka.ms/vs/17/release/vs_community.exe")?;

    let tempdir = tempfile::Builder::new()
        .prefix("rustup-visualstudio")
        .tempdir()
        .context("error creating temp directory")?;

    let visual_studio = tempdir.path().join("vs_setup.exe");

    info!("downloading Visual Studio installer");
    DownloadOptions::try_from(process)?
        .start(&visual_studio_url, &visual_studio)
        .download()
        .await?;

    // Run the installer. Arguments are documented at:
    // https://docs.microsoft.com/en-us/visualstudio/install/use-command-line-parameters-to-install-visual-studio
    let mut cmd = Command::new(visual_studio);
    cmd.arg("--wait")
        // Display an interactive GUI focused on installing just the selected components.
        .arg("--focusedUi")
        // Add the English language pack
        .args(["--addProductLang", "En-us"])
        // Add the linker and C runtime libraries.
        .args(["--add", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64"]);

    // It's possible an earlier or later version of the Windows SDK has been
    // installed separately from Visual Studio so installing it can be skipped.
    if !has_windows_sdk_libs(process) {
        cmd.args([
            "--add",
            "Microsoft.VisualStudio.Component.Windows11SDK.26100",
        ]);
    }
    info!("running the Visual Studio install");
    info!("rustup will continue once Visual Studio installation is complete\n");
    let exit_status = cmd
        .spawn()
        .and_then(|mut child| child.wait())
        .context("error running Visual Studio installer")?;

    if exit_status.success() {
        Ok(ContinueInstall::Yes)
    } else {
        match VsInstallError(exit_status.code().unwrap()) {
            err @ VsInstallError::REBOOT_REQUIRED => {
                // A reboot is required but the user opted to delay it.
                warn!("{}", err);
                Ok(ContinueInstall::Yes)
            }
            err @ VsInstallError::REBOOTING_NOW => {
                // The user is wanting to reboot right now, so we should
                // not continue the install.
                warn!("{}", err);
                info!("\nRun rustup-init after restart to continue install");
                Ok(ContinueInstall::No)
            }
            err => {
                // It's possible that the installer returned a non-zero exit code
                // even though the required components were successfully installed.
                // In that case we warn about the error but continue on.
                let have_msvc = msvc_check(opts, process).is_none();
                let has_libs = has_windows_sdk_libs(process);
                if have_msvc && has_libs {
                    warn!("Visual Studio is installed but a problem occurred during installation");
                    warn!("{}", err);
                    Ok(ContinueInstall::Yes)
                } else {
                    Err(err).context("failed to install Visual Studio")
                }
            }
        }
    }
}

fn has_windows_sdk_libs(process: &Process) -> bool {
    if let Some(paths) = process.var_os("lib") {
        for mut path in split_paths(&paths) {
            path.push("kernel32.lib");
            if path.exists() {
                return true;
            }
        }
    };
    false
}

/// Run by rustup-gc-$num.exe to delete CARGO_HOME
#[tracing::instrument(level = "trace")]
pub fn complete_windows_uninstall(process: &Process) -> anyhow::Result<utils::ExitCode> {
    let uninstall = wait_for_parent(process).and_then(|()| {
        let no_modify_path = process.var_os(GC_MODIFY_PATH).as_deref() != Some(OsStr::new("1"));

        // Now that the parent has exited there are hopefully no more files open in CARGO_HOME.
        let cargo_home = process.cargo_home()?;
        super::clean_cargo_home(no_modify_path, process, &cargo_home)
    });

    // Now, run a *system* binary to inherit the DELETE_ON_CLOSE
    // handle to *this* process, then exit. The OS will delete the gc
    // exe when it exits. Do this even if uninstalling failed.
    // Leave stdin inherited so the standard library passes GC's delete-on-close
    // handle to the cleanup child without raw handle APIs.
    let cleanup = Command::new("net")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(CliError::WindowsUninstallMadness);

    // Preserve the original uninstall error if starting cleanup also failed.
    uninstall?;
    cleanup?;
    Ok(utils::ExitCode(0))
}

/// Starts a helper that calls `wait_for_parent` before replacing or deleting files.
pub(super) fn spawn_with_parent_handle(command: &mut Command) -> io::Result<Child> {
    let mut parent = ptr::null_mut();
    // SAFETY: GetCurrentProcess is valid for this call, and parent is writable.
    // Duplicate the pseudo-handle into a real, inheritable SYNCHRONIZE handle.
    let duplicated = unsafe {
        let current = GetCurrentProcess();
        DuplicateHandle(current, current, current, &mut parent, SYNCHRONIZE, 1, 0)
    };
    if duplicated == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle succeeded and ownership has not been transferred yet.
    let parent = unsafe { OwnedHandle::from_raw_handle(parent) };

    // Keep our handle open until spawn returns; the child then owns its inherited copy.
    command
        .env(PARENT_HANDLE, (parent.as_raw_handle() as usize).to_string())
        .spawn()
}

fn wait_for_parent(process: &Process) -> anyhow::Result<()> {
    let Some(parent) = process.var_opt(PARENT_HANDLE)? else {
        return wait_for_parent_legacy();
    };
    let parent = parent.parse::<usize>()? as RawHandle;
    // SAFETY: spawn_with_parent_handle transfers a dedicated inherited process handle.
    // This is its only Rust owner in the helper, which calls this function once.
    let parent = unsafe { OwnedHandle::from_raw_handle(parent) };
    // SAFETY: parent keeps the inherited SYNCHRONIZE handle open throughout the wait.
    if unsafe { WaitForSingleObject(parent.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error()).context("failed to wait for parent process");
    }
    Ok(())
}

// Compatibility with launchers which do not pass a parent process handle.
// TODO: Delete this when it's suitable.
fn wait_for_parent_legacy() -> anyhow::Result<()> {
    unsafe {
        // Take a snapshot of system processes, one of which is ours
        // and contains our parent's pid
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            return Err(err).context(CliError::WindowsUninstallMadness);
        }

        let snapshot = OwnedHandle::from_raw_handle(snapshot);

        let mut entry: PROCESSENTRY32 = mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32>() as u32;

        // Iterate over system processes looking for ours
        let success = Process32First(snapshot.as_raw_handle(), &mut entry);
        if success == 0 {
            let err = io::Error::last_os_error();
            return Err(err).context(CliError::WindowsUninstallMadness);
        }

        let this_pid = GetCurrentProcessId();
        while entry.th32ProcessID != this_pid {
            let success = Process32Next(snapshot.as_raw_handle(), &mut entry);
            if success == 0 {
                let err = io::Error::last_os_error();
                return Err(err).context(CliError::WindowsUninstallMadness);
            }
        }

        // FIXME: Using the process ID exposes a race condition
        // wherein the parent process already exited and the OS
        // reassigned its ID.
        let parent_id = entry.th32ParentProcessID;

        // Get a handle to the parent process
        let parent = OpenProcess(SYNCHRONIZE, 0, parent_id);
        if parent.is_null() {
            // This just means the parent has already exited.
            return Ok(());
        }

        let parent = OwnedHandle::from_raw_handle(parent);

        // Wait for our parent to exit
        let res = WaitForSingleObject(parent.as_raw_handle(), INFINITE);

        if res != WAIT_OBJECT_0 {
            let err = io::Error::last_os_error();
            return Err(err).context(CliError::WindowsUninstallMadness);
        }
    }

    Ok(())
}

const RUSTUP_UNINSTALL_ENTRY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Rustup";

impl Process {
    fn registry_sub_key_path<'a>(&self, sub_key: &'a str) -> Cow<'a, str> {
        #[cfg(any(test, feature = "test"))]
        if let Ok(test_id) = self.var(RUSTUP_REGISTRY_TEST_ID) {
            return Cow::Owned(format!(r"RustupTest-{test_id}\{sub_key}"));
        }

        Cow::Borrowed(sub_key)
    }

    fn registry_key(&self, sub_key: &str, parent: &Key) -> windows_registry::Result<Key> {
        let mut options = parent.options();
        options.read().write().create();

        #[cfg(any(test, feature = "test"))]
        if self.var(RUSTUP_REGISTRY_TEST_ID).is_ok() {
            options.volatile();
        }

        options.open(self.registry_sub_key_path(sub_key))
    }

    fn registry_environment_key(&self) -> windows_registry::Result<Key> {
        self.registry_key("Environment", CURRENT_USER)
    }
}

fn rustup_uninstall_registry_key(process: &Process) -> anyhow::Result<Key> {
    process
        .registry_key(RUSTUP_UNINSTALL_ENTRY, CURRENT_USER)
        .context("Failed creating uninstall key")
}

pub(crate) fn update_uninstall_registry_display_version(
    version: &str,
    process: &Process,
) -> anyhow::Result<()> {
    rustup_uninstall_registry_key(process)?
        .set_string("DisplayVersion", version)
        .context("Failed to set `DisplayVersion`")
}

pub(crate) fn add_uninstall_registry_entry(process: &Process) -> anyhow::Result<()> {
    let key = rustup_uninstall_registry_key(process)?;

    // Don't overwrite registry if Rustup is already installed
    let prev = key.get_hstring("UninstallString");
    if let Ok(s) = prev {
        let mut path = PathBuf::from(s.to_os_string());
        path.pop();
        if path.exists() {
            return Ok(());
        }
    }

    let mut path = process.cargo_home()?;
    path.push("bin\\rustup.exe");
    let mut uninstall_cmd = OsString::from("\"");
    uninstall_cmd.push(path);
    uninstall_cmd.push("\" self uninstall");

    key.set_hstring("UninstallString", &HSTRING::from(uninstall_cmd))
        .context("Failed to set `UninstallString`")?;
    key.set_string("DisplayName", "Rustup: the Rust toolchain installer")
        .context("Failed to set `DisplayName`")?;
    update_uninstall_registry_display_version(env!("CARGO_PKG_VERSION"), process)?;

    Ok(())
}

pub(crate) fn remove_uninstall_registry_entry(process: &Process) -> anyhow::Result<()> {
    match CURRENT_USER.remove_tree(process.registry_sub_key_path(RUSTUP_UNINSTALL_ENTRY)) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == WIN32_ERROR(ERROR_FILE_NOT_FOUND).to_hresult() => Ok(()),
        Err(e) => Err(anyhow!(e)),
    }
}

pub(super) fn run_update(
    prepared_updater: PreparedUpdater,
    _process: &Process,
) -> anyhow::Result<utils::ExitCode> {
    prepared_updater.spawn_replacer()?;

    Ok(utils::ExitCode(0))
}

pub(crate) fn self_replace(process: &Process) -> anyhow::Result<utils::ExitCode> {
    wait_for_parent(process)?;
    let self_update_lock = SelfUpdateLock::lock(process)?;
    let result = process.cargo_home().and_then(|cargo_home| {
        self_update_lock.install_bins(&cargo_home.join("bin"), super::force_hard_links(process))?;
        update_uninstall_registry_display_version(env!("CARGO_PKG_VERSION"), process)
    });
    stage::mark_result(result.is_ok(), process);
    result?;

    Ok(utils::ExitCode(0))
}

// Spawn a temporary rustup-gc-$random.exe to finish Windows uninstall
// after the original rustup.exe process exits. On Unix, the running
// executable can be deleted directly. On Windows you can't delete files
// while they are open, like when they are running.
//
// Here's what we're going to do:
// - Copy rustup.exe to a temporary file in
//   CARGO_HOME/../rustup-gc-$random.exe.
// - Open the gc exe with the FILE_FLAG_DELETE_ON_CLOSE and
//   FILE_SHARE_DELETE flags. This is going to be the last
//   file to remove, and the OS is going to do it for us.
//   Pass this handle as stdin so the standard library manages inheritance.
//   GC does not read stdin; it uses it only to carry the deletion handle.
// - Run the gc exe, which waits for the original rustup.exe
//   process to close, then deletes CARGO_HOME. This process
//   has inherited a FILE_FLAG_DELETE_ON_CLOSE handle to itself.
// - Finally, spawn yet another system binary inheriting stdin,
//   so *it* inherits the FILE_FLAG_DELETE_ON_CLOSE handle to
//   the gc exe. If the gc exe exits before the system exe then at
//   last it will be deleted when the handle closes.
//
// This is the DELETE_ON_CLOSE method from
// https://www.catch22.net/tuts/win32/self-deleting-executables
//
// ... which doesn't actually work because Windows won't really
// delete a FILE_FLAG_DELETE_ON_CLOSE process when it exits.
//
// .. augmented with this SO answer
// https://stackoverflow.com/questions/10319526/understanding-a-self-deleting-program-in-c
pub(crate) fn spawn_uninstall_gc(no_modify_path: bool, cargo_home: &Path) -> anyhow::Result<()> {
    // The rustup.exe bin
    let rustup_path = cargo_home.join(format!("bin/rustup{EXE_SUFFIX}"));

    // The directory containing CARGO_HOME
    let work_path = cargo_home
        .parent()
        .expect("CARGO_HOME doesn't have a parent?");

    // Generate a unique name for the files we're about to move out
    // of CARGO_HOME.
    let numbah: u32 = rand::random();
    let gc_exe = work_path.join(format!("rustup-gc-{numbah:x}.exe"));
    // Copy rustup (probably this process's exe) to the gc exe
    utils::copy_file_symlink_to_source(&rustup_path, &gc_exe)?;
    // OpenOptions preserves the read, sharing and delete-on-close flags while
    // letting File own the handle until it is passed to Command below.
    let gc_handle = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(&gc_exe)
        .context(CliError::WindowsUninstallMadness)?;

    let mut command = Command::new(gc_exe);
    command
        .stdin(gc_handle)
        .env(GC_MODIFY_PATH, if no_modify_path { "0" } else { "1" });
    spawn_with_parent_handle(&mut command).context(CliError::WindowsUninstallMadness)?;

    // Command retains the deletion handle after spawn; keep it alive through the sleep.
    // The catch 22 article says we must sleep here to give
    // Windows a chance to bump the processes file reference
    // count. acrichto though is in disbelief and *demanded* that
    // we not insert a sleep. If Windows failed to uninstall
    // correctly it is because of him.

    // (.. and months later acrichto owes me a beer).
    thread::sleep(Duration::from_millis(100));

    Ok(())
}

// The rustup-gc executable cannot accept normal function call here,
// so we use env var here, notifying it if we need to remove $CARGO_HOME/bin from $PATH
const GC_MODIFY_PATH: &str = "RUSTUP_GC_MODIFY_PATH";

// Decimal value of the process handle inherited by GC or the self-replacer.
// Older launchers omit it, so their helpers use PID lookup instead.
const PARENT_HANDLE: &str = "RUSTUP_PARENT_HANDLE";

/// Environment variable carrying the per-test registry ID.
#[cfg(any(test, feature = "test"))]
pub const RUSTUP_REGISTRY_TEST_ID: &str = "RUSTUP_REGISTRY_TEST_ID";

#[cfg(any(test, feature = "test"))]
pub fn get_path(test_id: &str) -> anyhow::Result<Option<Value>> {
    USER_PATH.get(test_id, CURRENT_USER)
}

#[cfg(any(test, feature = "test"))]
pub const USER_PATH: RegistryValueId = RegistryValueId {
    sub_key: "Environment",
    value_name: "PATH",
};

#[cfg(any(test, feature = "test"))]
pub struct RegistryValueId {
    pub sub_key: &'static str,
    pub value_name: &'static str,
}

#[cfg(any(test, feature = "test"))]
impl RegistryValueId {
    pub fn get(&self, test_id: &str, parent: &Key) -> anyhow::Result<Option<Value>> {
        let mut options = parent.options();
        options.read().write().create().volatile();
        let sub_key = options.open(format!(r"RustupTest-{test_id}\{}", self.sub_key))?;
        match sub_key.get_value(self.value_name) {
            Ok(val) => Ok(Some(val)),
            Err(e) if e.code() == WIN32_ERROR(ERROR_FILE_NOT_FOUND).to_hresult() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set(&self, new: Option<&Value>, test_id: &str, parent: &Key) -> anyhow::Result<()> {
        let mut options = parent.options();
        options.read().write().create().volatile();
        let sub_key = options.open(format!(r"RustupTest-{test_id}\{}", self.sub_key))?;
        match new {
            Some(new) => Ok(sub_key.set_value(self.value_name, new)?),
            None => match sub_key.remove_value(self.value_name) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == WIN32_ERROR(ERROR_FILE_NOT_FOUND).to_hresult() => Ok(()),
                Err(e) => Err(e.into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{process::TestProcess, test::test_id};

    fn test_process(test_id: &str) -> TestProcess {
        let vars: HashMap<String, String> = [
            ("HOME".to_string(), "/unused".to_string()),
            (RUSTUP_REGISTRY_TEST_ID.to_string(), test_id.to_owned()),
        ]
        .into_iter()
        .collect();
        TestProcess::with_vars(vars)
    }

    #[test]
    fn windows_registry_isolated_per_test_id() {
        let first_id = test_id();
        let second_id = test_id();
        let first = test_process(&first_id);
        let second = test_process(&second_id);
        let first_value = Value::from("first");
        let second_value = Value::from("second");

        USER_PATH
            .set(Some(&first_value), &first_id, CURRENT_USER)
            .unwrap();
        USER_PATH
            .set(Some(&second_value), &second_id, CURRENT_USER)
            .unwrap();

        assert_eq!(
            first
                .process
                .registry_environment_key()
                .unwrap()
                .get_hstring("PATH")
                .unwrap(),
            HSTRING::from("first")
        );
        assert_eq!(
            second
                .process
                .registry_environment_key()
                .unwrap()
                .get_hstring("PATH")
                .unwrap(),
            HSTRING::from("second")
        );

        let programs_version = RegistryValueId {
            sub_key: RUSTUP_UNINSTALL_ENTRY,
            value_name: "DisplayVersion",
        };
        programs_version
            .set(Some(&first_value), &first_id, CURRENT_USER)
            .unwrap();
        assert_eq!(
            programs_version.get(&first_id, CURRENT_USER).unwrap(),
            Some(first_value)
        );
        assert_eq!(
            programs_version.get(&second_id, CURRENT_USER).unwrap(),
            None
        );
    }

    #[test]
    fn windows_registry_cleanup_is_idempotent() {
        let test_id = test_id();
        let tp = test_process(&test_id);

        remove_uninstall_registry_entry(&tp.process).unwrap();
    }


}
