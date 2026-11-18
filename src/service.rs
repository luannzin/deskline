//! Windows installed mode.
//!
//! `deskline --install` (elevated) copies the exe to Program Files and registers the "Deskline"
//! service. The service (LocalSystem, session 0) keeps `deskline --server` running as SYSTEM in
//! the active console session, so the sign-in screen, the lock screen and UAC prompts can be
//! seen and controlled, and relays Ctrl+Alt+Del to `SendSAS`.
use std::ffi::{OsStr, OsString, c_void};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::{CreateDirectoryW, DELETE, MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Services::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{BOOL, GUID, HSTRING, Interface, Owned, PCWSTR, PWSTR, s, w};
use windows_registry::LOCAL_MACHINE;

const SERVICE: PCWSTR = w!("Deskline");
const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Deskline";
const POLICY_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
/// Remembers what install changed, so uninstall undoes only that.
const OUR_KEY: &str = r"SOFTWARE\Deskline";
/// SYSTEM and Administrators only, not inherited from %ProgramData% (which every user can read).
const DATA_ACL: PCWSTR = w!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");

/// The service is registered on this machine.
pub fn is_installed() -> bool {
    open_service(SERVICE_QUERY_STATUS).is_ok()
}

/// Elevated: copy this exe to `%ProgramFiles%\Deskline`, register and start the service, add
/// the Start menu shortcut, the Apps & features entry, the firewall rule and the SAS policy.
pub fn install() -> anyhow::Result<()> {
    anyhow::ensure!(is_elevated(), "Installing needs administrator rights.");
    let dir = install_dir()?;
    let exe = dir.join("deskline.exe");
    let me = std::env::current_exe()?;
    let scm = unsafe { Owned::new(OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS)?) };
    let old = unsafe { OpenServiceW(*scm, SERVICE, SERVICE_ALL_ACCESS) }.ok().map(|s| unsafe { Owned::new(s) });

    if !(exe.exists() && fs::canonicalize(&me)? == fs::canonicalize(&exe)?) {
        // Reinstalling or updating: the service runs the exe being replaced.
        if let Some(svc) = &old {
            stop(**svc)?;
        }
        fs::create_dir_all(&dir)?;
        copy_exe(&me, &exe).with_context(|| format!("Couldn't copy Deskline to {}", dir.display()))?;
    }

    let bin = HSTRING::from(cmdline(&exe, &["--service"]));
    let svc = unsafe {
        match old {
            // Also repairs a service someone disabled.
            Some(svc) => {
                ChangeServiceConfigW(
                    *svc,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    &bin,
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SERVICE,
                )?;
                svc
            }
            // No account name: LocalSystem.
            None => Owned::new(CreateServiceW(
                *scm,
                SERVICE,
                SERVICE,
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                &bin,
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
            )?),
        }
    };
    configure(*svc)?;
    add_shortcut(&exe)?;
    add_uninstall_entry(&dir, &exe)?;
    firewall(Some(&exe));
    allow_sas()?;
    match unsafe { StartServiceW(*svc, None) } {
        Err(e) if e.code() != ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => Err(e).context("Couldn't start the Deskline service"),
        _ => Ok(()),
    }
}

/// Elevated: undo `install`.
pub fn uninstall() -> anyhow::Result<()> {
    anyhow::ensure!(is_elevated(), "Uninstalling needs administrator rights.");
    if let Ok(svc) = open_service(SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE.0) {
        stop(*svc)?;
        unsafe { DeleteService(*svc) }.context("Couldn't remove the Deskline service")?;
    }
    let _ = fs::remove_file(shortcut()?);
    let _ = LOCAL_MACHINE.remove_tree(UNINSTALL_KEY);
    firewall(None);
    undo_sas();
    // %ProgramData%\Deskline stays: it holds this computer's identity and settings, so installing
    // again keeps the same code.
    remove_later(&install_dir()?)
}

/// Start this exe again with `args` through the UAC prompt and wait for it to finish. Errors with
/// "Cancelled" when the prompt was declined.
pub fn elevate(args: &[&str]) -> anyhow::Result<()> {
    let exe = HSTRING::from(std::env::current_exe()?.as_path());
    let params = HSTRING::from(args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        anyhow::ensure!(e.code() != ERROR_CANCELLED.to_hresult(), "Cancelled");
        return Err(e.into());
    }
    let process = unsafe { Owned::new(info.hProcess) };
    let mut code = 0;
    unsafe {
        WaitForSingleObject(*process, INFINITE);
        GetExitCodeProcess(*process, &mut code)?;
    }
    anyhow::ensure!(code == 0, "That didn't work (exit code {code}).");
    Ok(())
}

/// This process runs elevated (or as SYSTEM).
pub fn is_elevated() -> bool {
    elevated(unsafe { GetCurrentProcess() })
}

/// This process runs as LocalSystem (the `--server` started by the service).
pub fn is_system() -> bool {
    unsafe {
        let mut token = Owned::<HANDLE>::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut *token).is_err() {
            return false;
        }
        // TOKEN_USER and the SID it points to (at most 68 bytes), suitably aligned.
        let mut buf = [0u64; 32];
        let mut len = 0;
        if GetTokenInformation(*token, TokenUser, Some(buf.as_mut_ptr().cast()), size_of_val(&buf) as u32, &mut len).is_err() {
            return false;
        }
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        IsWellKnownSid(user.User.Sid, WinLocalSystemSid).as_bool()
    }
}

/// `--service`: hand this process to the service control manager. Returns when the service stops.
pub fn run() -> anyhow::Result<()> {
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(SERVICE.as_ptr().cast_mut()), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.context("Only Windows starts the Deskline service")?;
    Ok(())
}

/// Restart the service from a detached helper (after an update replaced the exe). The helper
/// leaves the service's job, so it survives the service stopping.
pub fn restart_detached() -> anyhow::Result<()> {
    Command::new(std::env::current_exe()?)
        .arg("--restart-service")
        .creation_flags((DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB).0)
        // Not our stdout: that pipe must close when we exit.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// `--restart-service`: stop the service, wait, start it again.
pub fn restart_service() -> anyhow::Result<()> {
    let svc = open_service(SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS).context("The Deskline service isn't installed")?;
    stop(*svc)?;
    unsafe { StartServiceW(*svc, None) }.context("Couldn't start the Deskline service")?;
    Ok(())
}

/// Ctrl+Alt+Del: asks the service to call `SendSAS` (only a service may).
pub fn request_sas() {
    if is_system() {
        // The service reads our stdout.
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"sas\n").and_then(|_| out.flush());
    } else {
        send_sas(true);
    }
}

/// Run `f` as the user signed in to this session (file access for the viewer). Runs `f` directly
/// when this process isn't SYSTEM. Errors when nobody is signed in.
///
/// Impersonation covers the calling thread only: call it from a thread of its own
/// (`spawn_blocking`), never across an `.await`.
pub fn as_user<T>(f: impl FnOnce() -> T) -> anyhow::Result<T> {
    if !is_system() {
        return Ok(f());
    }
    let token = user_token()?;
    unsafe { ImpersonateLoggedOnUser(*token)? };
    // Back to SYSTEM even if `f` panics.
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            unsafe {
                let _ = RevertToSelf();
            }
        }
    }
    let _revert = Revert;
    Ok(f())
}

/// The signed-in user's Downloads folder.
pub fn user_downloads() -> anyhow::Result<PathBuf> {
    let token = if is_system() { Some(user_token()?) } else { None };
    Ok(known_folder(&FOLDERID_Downloads, token.as_deref().copied())?)
}

/// Start Deskline's window as the user signed in to this session, with `args`.
pub fn spawn_ui(args: &[&str]) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    if !is_system() {
        Command::new(exe).args(args).spawn()?;
        return Ok(());
    }
    // It stays in the service's job, so it closes with the service (uninstall, update).
    let token = user_token()?;
    unsafe {
        let mut env = std::ptr::null_mut();
        CreateEnvironmentBlock(&mut env, Some(*token), false)?;
        let mut desktop = wide("winsta0\\default");
        let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, lpDesktop: PWSTR(desktop.as_mut_ptr()), ..Default::default() };
        let mut cmd = wide(cmdline(&exe, args));
        let mut pi = PROCESS_INFORMATION::default();
        let started = CreateProcessAsUserW(
            Some(*token),
            &HSTRING::from(exe.as_path()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT,
            Some(env.cast_const()),
            PCWSTR::null(),
            &si,
            &mut pi,
        );
        let _ = DestroyEnvironmentBlock(env);
        started?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// Whether process `pid` runs elevated (for settings changes over the local pipe).
pub fn process_is_elevated(pid: u32) -> bool {
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => elevated(*unsafe { Owned::new(h) }),
        Err(_) => false,
    }
}

/// The Windows session this process runs in.
pub fn session_id() -> u32 {
    let mut id = 0;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut id);
    }
    id
}

/// `%ProgramData%\Deskline`, created readable and writable only by SYSTEM and Administrators.
/// An existing folder is left as it is.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    let dir = known_folder(&FOLDERID_ProgramData, None)?.join("Deskline");
    if !dir.is_dir() {
        let sd = descriptor(DATA_ACL)?;
        let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: (*sd).0, bInheritHandle: false.into() };
        match unsafe { CreateDirectoryW(&HSTRING::from(dir.as_path()), Some(&sa)) } {
            Err(e) if e.code() != ERROR_ALREADY_EXISTS.to_hresult() => {
                return Err(e).with_context(|| format!("Couldn't create {}", dir.display()));
            }
            _ => {}
        }
    }
    // The folder holds the computer's identity. One a standard user made before Deskline was
    // installed would be theirs to read (and to plant an identity in): refuse it.
    anyhow::ensure!(
        owned_by_system_or_admins(&dir),
        "{} wasn't created by Deskline. Delete it and install again.",
        dir.display()
    );
    Ok(dir)
}

fn owned_by_system_or_admins(path: &Path) -> bool {
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    let mut owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let err = unsafe { GetNamedSecurityInfoW(&HSTRING::from(path), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, &mut sd) };
    if err != ERROR_SUCCESS {
        return false;
    }
    let ok = unsafe { IsWellKnownSid(owner, WinLocalSystemSid).as_bool() || IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool() };
    unsafe { LocalFree(Some(HLOCAL(sd.0))) };
    ok
}

// ---------------------------------------------------------------- install helpers

fn install_dir() -> windows::core::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramFiles, None)?.join("Deskline"))
}

fn open_service(access: u32) -> windows::core::Result<Owned<SC_HANDLE>> {
    unsafe {
        let scm = Owned::new(OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)?);
        Ok(Owned::new(OpenServiceW(*scm, SERVICE, access)?))
    }
}

/// Stop the service and wait until it has (at most 30 s).
fn stop(svc: SC_HANDLE) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut st = SERVICE_STATUS::default();
    loop {
        unsafe { QueryServiceStatus(svc, &mut st)? };
        if st.dwCurrentState == SERVICE_STOPPED {
            return Ok(());
        }
        anyhow::ensure!(Instant::now() < deadline, "The Deskline service didn't stop.");
        // Refused while it's still starting or already stopping: just ask again.
        unsafe {
            let _ = ControlService(svc, SERVICE_CONTROL_STOP, &mut st);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A Deskline window may still run the old exe: it can't be overwritten but can be moved aside.
fn copy_exe(me: &Path, exe: &Path) -> std::io::Result<()> {
    let old = exe.with_extension("old");
    let _ = fs::remove_file(&old);
    if exe.exists() {
        fs::rename(exe, &old)?;
    }
    fs::copy(me, exe)?;
    // Still running: the next install or the uninstall removes it.
    let _ = fs::remove_file(&old);
    Ok(())
}

/// The description in services.msc, and a restart 5 s after a crash.
fn configure(svc: SC_HANDLE) -> windows::core::Result<()> {
    let mut text = wide("Keeps Deskline ready for remote support, including at the sign-in screen and for admin prompts.");
    let mut restart = [SC_ACTION { Type: SC_ACTION_RESTART, Delay: 5000 }; 3];
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 24 * 60 * 60,
        cActions: restart.len() as u32,
        lpsaActions: restart.as_mut_ptr(),
        ..Default::default()
    };
    let description = SERVICE_DESCRIPTIONW { lpDescription: PWSTR(text.as_mut_ptr()) };
    unsafe {
        ChangeServiceConfig2W(svc, SERVICE_CONFIG_DESCRIPTION, Some((&raw const description).cast()))?;
        ChangeServiceConfig2W(svc, SERVICE_CONFIG_FAILURE_ACTIONS, Some((&raw const failure).cast()))
    }
}

/// All users' Start menu.
fn shortcut() -> windows::core::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_CommonPrograms, None)?.join("Deskline.lnk"))
}

fn add_shortcut(exe: &Path) -> anyhow::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe))?;
        link.SetDescription(w!("Remote desktop"))?;
        link.cast::<IPersistFile>()?.Save(&HSTRING::from(shortcut()?.as_path()), true)?;
    }
    Ok(())
}

/// Settings > Apps & features.
fn add_uninstall_entry(dir: &Path, exe: &Path) -> anyhow::Result<()> {
    let key = LOCAL_MACHINE.create(UNINSTALL_KEY)?;
    key.set_string("DisplayName", "Deskline")?;
    key.set_string("DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    key.set_string("Publisher", "luannzin")?;
    key.set_string("DisplayIcon", exe.display().to_string())?;
    key.set_string("InstallLocation", dir.display().to_string())?;
    key.set_string("UninstallString", cmdline(exe, &["--uninstall"]))?;
    key.set_u32("NoModify", 1)?;
    key.set_u32("NoRepair", 1)?;
    key.set_u32("EstimatedSize", (fs::metadata(exe)?.len() / 1024) as u32)?; // KB
    Ok(())
}

/// Inbound rule for the exe, deleted first so reinstalling never adds a second one. Best effort:
/// with the Windows firewall service off there's nothing to open.
fn firewall(exe: Option<&Path>) {
    let netsh = |args: String| {
        let Ok(netsh) = system32("netsh.exe") else { return };
        let _ = Command::new(netsh).raw_arg(args).creation_flags(CREATE_NO_WINDOW.0).stdout(Stdio::null()).stderr(Stdio::null()).status();
    };
    netsh(r#"advfirewall firewall delete rule name="Deskline""#.into());
    if let Some(exe) = exe {
        netsh(format!(
            r#"advfirewall firewall add rule name="Deskline" dir=in action=allow program="{}" enable=yes profile=any"#,
            exe.display()
        ));
    }
}

/// Let services send Ctrl+Alt+Del (SoftwareSASGeneration 1) when nothing allows it yet.
fn allow_sas() -> anyhow::Result<()> {
    let policy = LOCAL_MACHINE.create(POLICY_KEY)?;
    if policy.get_u32("SoftwareSASGeneration").unwrap_or(0) == 0 {
        policy.set_u32("SoftwareSASGeneration", 1)?;
        LOCAL_MACHINE.create(OUR_KEY)?.set_u32("SasPolicySet", 1)?;
    }
    Ok(())
}

fn undo_sas() {
    if matches!(LOCAL_MACHINE.open(OUR_KEY).and_then(|k| k.get_u32("SasPolicySet")), Ok(1)) {
        if let Ok(policy) = LOCAL_MACHINE.create(POLICY_KEY) {
            // Unset or 0 before, services couldn't send it either way; unset is the default.
            if matches!(policy.get_u32("SoftwareSASGeneration"), Ok(1)) {
                let _ = policy.remove_value("SoftwareSASGeneration");
            }
        }
    }
    let _ = LOCAL_MACHINE.remove_tree(OUR_KEY);
}

/// Delete the install folder a few seconds after this process exits: it may be running from there.
fn remove_later(dir: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let cmd = Command::new(system32("cmd.exe")?)
        .raw_arg(format!(r#"/c ping -n 3 127.0.0.1 >nul & rmdir /s /q "{}""#, dir.display()))
        .current_dir(std::env::temp_dir())
        .creation_flags((CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB).0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if cmd.is_err() {
        // At the next restart instead. A folder must be empty by then, so its files go first.
        for f in fs::read_dir(dir)?.flatten() {
            unsafe { MoveFileExW(&HSTRING::from(f.path().as_path()), PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT)? };
        }
        unsafe { MoveFileExW(&HSTRING::from(dir), PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT)? };
    }
    Ok(())
}

/// A System32 tool by full path: an elevated installer run from Downloads must not pick up a
/// netsh.exe or cmd.exe lying next to it.
fn system32(name: &str) -> windows::core::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_System, None)?.join(name))
}

// ---------------------------------------------------------------- the service

/// Set by the control handler, polled by the supervisor.
static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let Ok(status) = (unsafe { RegisterServiceCtrlHandlerExW(SERVICE, Some(on_control), None) }) else { return };
    let report = |state: SERVICE_STATUS_CURRENT_STATE, controls: u32| {
        let st = SERVICE_STATUS { dwServiceType: SERVICE_WIN32_OWN_PROCESS, dwCurrentState: state, dwControlsAccepted: controls, ..Default::default() };
        unsafe {
            let _ = SetServiceStatus(status, &st);
        }
    };
    report(SERVICE_RUNNING, SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN);
    supervise();
    report(SERVICE_STOPPED, 0);
}

unsafe extern "system" fn on_control(control: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => STOP.store(true, Ordering::SeqCst),
        SERVICE_CONTROL_INTERROGATE => {}
        _ => return ERROR_CALL_NOT_IMPLEMENTED.0,
    }
    NO_ERROR.0
}

/// Keep `--server` running in the console session until the service stops.
fn supervise() {
    let Ok(exe) = std::env::current_exe() else { return };
    // Children die with the service, even if it crashes.
    let job = job().ok();
    let mut quick_exits = 0u32;
    while !STOP.load(Ordering::SeqCst) {
        // None while no session owns the console, e.g. during a user switch.
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if session == u32::MAX {
            pause(Duration::from_secs(1));
            continue;
        }
        let started = Instant::now();
        if let Ok(child) = start_server(&exe, session, job.as_deref().copied()) {
            if watch(*child, session) {
                // The console moved to another session: follow it right away.
                quick_exits = 0;
                continue;
            }
        }
        // 1 s, doubling up to 30 s while it keeps exiting right away.
        quick_exits = if started.elapsed() < Duration::from_secs(30) { quick_exits + 1 } else { 0 };
        pause(Duration::from_secs((1u64 << quick_exits.saturating_sub(1).min(5)).min(30)));
    }
}

/// Kills its processes when the service exits, but lets them break away (`restart_detached`).
fn job() -> windows::core::Result<Owned<HANDLE>> {
    unsafe {
        let job = Owned::new(CreateJobObjectW(None, PCWSTR::null())?);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(*job, JobObjectExtendedLimitInformation, (&raw const info).cast(), size_of_val(&info) as u32)?;
        Ok(job)
    }
}

/// `"<exe>" --server` as SYSTEM on the session's desktop, its stdout piped back here for "sas".
fn start_server(exe: &Path, session: u32, job: Option<HANDLE>) -> anyhow::Result<Owned<HANDLE>> {
    unsafe {
        // Our own SYSTEM token, moved to that session.
        let mut own = Owned::<HANDLE>::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE, &mut *own)?;
        let mut token = Owned::<HANDLE>::default();
        DuplicateTokenEx(*own, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut *token)?;
        SetTokenInformation(*token, TokenSessionId, (&raw const session).cast(), size_of::<u32>() as u32)?;

        // Only the child inherits the write end.
        let (mut read, mut write) = (HANDLE::default(), Owned::<HANDLE>::default());
        let inherit = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, bInheritHandle: true.into(), ..Default::default() };
        CreatePipe(&mut read, &mut *write, Some(&inherit), 0)?;
        let out = File::from_raw_handle(read.0);
        SetHandleInformation(read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;

        let mut desktop = wide("winsta0\\default");
        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            dwFlags: STARTF_USESTDHANDLES,
            hStdOutput: *write,
            hStdError: *write,
            ..Default::default()
        };
        let mut cmd = wide(cmdline(exe, &["--server"]));
        let mut pi = PROCESS_INFORMATION::default();
        // Suspended until it's in the job, so nothing it starts can slip out.
        CreateProcessAsUserW(
            Some(*token),
            &HSTRING::from(exe),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW | CREATE_SUSPENDED,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )?;
        let (process, thread) = (Owned::new(pi.hProcess), Owned::new(pi.hThread));
        // Ours closed, the pipe ends when the child exits.
        drop(write);
        if let Some(job) = job {
            let _ = AssignProcessToJobObject(job, *process);
        }
        ResumeThread(*thread);
        std::thread::spawn(move || relay_sas(out));
        Ok(process)
    }
}

/// The `--server` prints "sas" when the viewer pressed Ctrl+Alt+Del. Reads until it exits, so its
/// writes never block.
fn relay_sas(out: File) {
    for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
        if line.trim_ascii() == b"sas" {
            send_sas(false);
        }
    }
}

/// Wait for the child to exit. Ends it when the service stops or the console moves to another
/// session; true for the latter.
fn watch(child: HANDLE, session: u32) -> bool {
    loop {
        if unsafe { WaitForSingleObject(child, 500) } != WAIT_TIMEOUT {
            return false;
        }
        let moved = unsafe { WTSGetActiveConsoleSessionId() } != session;
        if moved || STOP.load(Ordering::SeqCst) {
            unsafe {
                let _ = TerminateProcess(child, 0);
                WaitForSingleObject(child, 5000);
            }
            return moved;
        }
    }
}

/// Sleep, but wake up for a stop request.
fn pause(d: Duration) {
    let end = Instant::now() + d;
    while !STOP.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `SendSAS` from sas.dll, looked up at run time and only in System32.
fn send_sas(as_user: bool) {
    unsafe {
        let Ok(dll) = LoadLibraryExW(w!("sas.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) else { return };
        if let Some(f) = GetProcAddress(dll, s!("SendSAS")) {
            let send: unsafe extern "system" fn(BOOL) = std::mem::transmute(f);
            send(as_user.into());
        }
    }
}

// ---------------------------------------------------------------- small helpers

/// The token of the user signed in to this session (we are SYSTEM).
fn user_token() -> anyhow::Result<Owned<HANDLE>> {
    let mut token = Owned::<HANDLE>::default();
    unsafe { WTSQueryUserToken(session_id(), &mut *token) }.map_err(|_| anyhow::anyhow!("Nobody is signed in on that computer."))?;
    Ok(token)
}

/// Whether `process` runs elevated. False when that can't be told.
fn elevated(process: HANDLE) -> bool {
    unsafe {
        let mut token = Owned::<HANDLE>::default();
        if OpenProcessToken(process, TOKEN_QUERY, &mut *token).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0;
        GetTokenInformation(*token, TokenElevation, Some((&raw mut e).cast()), size_of_val(&e) as u32, &mut len).is_ok()
            && e.TokenIsElevated != 0
    }
}

fn known_folder(id: &GUID, token: Option<HANDLE>) -> windows::core::Result<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, token)?;
        let path = PathBuf::from(OsString::from_wide(p.as_wide()));
        CoTaskMemFree(Some(p.0.cast_const().cast()));
        Ok(path)
    }
}

/// A security descriptor from SDDL, freed on drop.
fn descriptor(sddl: PCWSTR) -> windows::core::Result<Owned<HLOCAL>> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &mut sd, None)?;
        Ok(Owned::new(HLOCAL(sd.0)))
    }
}

/// `"<exe>" args…`, each argument quoted so Windows splits it back exactly.
fn cmdline(exe: &Path, args: &[&str]) -> String {
    let mut s = format!("\"{}\"", exe.display());
    for a in args {
        s.push(' ');
        s.push_str(&quote(a));
    }
    s
}

/// One argument as `CommandLineToArgvW` reads it back: quoted when it has spaces or quotes, with
/// backslashes doubled only where they precede a quote.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.into();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            if c == '"' {
                out.extend(std::iter::repeat_n('\\', backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', backslashes));
    out.push('"');
    out
}

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Windows makes of a command line: the arguments after the program name.
    fn split(cmd: &str) -> Vec<String> {
        unsafe {
            let mut n = 0;
            let argv = CommandLineToArgvW(&HSTRING::from(cmd), &mut n);
            let args = (1..n as usize).map(|i| (*argv.add(i)).to_string().unwrap()).collect();
            LocalFree(Some(HLOCAL(argv.cast())));
            args
        }
    }

    #[test]
    fn arguments_survive_quoting() {
        let args = ["--server", "two words", "", r#"say "hi""#, r"C:\dir\", r"C:\my dir\", r#"a\"b"#, "tab\there"];
        let cmd = cmdline(Path::new(r"C:\Program Files\Deskline\deskline.exe"), &args);
        assert!(cmd.starts_with(r#""C:\Program Files\Deskline\deskline.exe" --server "two words" "" "#), "{cmd}");
        assert_eq!(split(&cmd), args);
    }

    #[test]
    fn data_acl_parses() {
        descriptor(DATA_ACL).unwrap();
    }

    #[test]
    fn this_process() {
        assert!(!is_system());
        assert_ne!(session_id(), 0, "tests run in a signed-in session, not the services' session 0");
        assert_eq!(process_is_elevated(std::process::id()), is_elevated());
        assert_eq!(as_user(|| 7).unwrap(), 7);
        assert!(user_downloads().unwrap().is_absolute());
        assert!(system32("cmd.exe").unwrap().is_file());
    }
}
