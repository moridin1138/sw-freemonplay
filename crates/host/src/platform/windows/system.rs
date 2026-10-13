//! Windows odds and ends for the tray app: the firewall rule, starting at sign-in,
//! administrator steps, message boxes and a single running copy.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, WAIT_OBJECT_0};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::Win32::System::Threading::{CreateMutexW, GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_YESNO, MessageBoxW, SW_HIDE, SW_SHOWNORMAL,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Programs we run without a console window flashing up.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The inbound firewall rule that lets screens reach the host.
const FIREWALL_RULE: &str = "Freemonplay (screens)";

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("freemonplay");

/// A command that runs without showing a console window.
pub fn quiet_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// The host is a windowed program (no console of its own). From a terminal, write to that
/// terminal instead. Returns whether there is one.
pub fn attach_console() -> bool {
    // SAFETY: attaching to the parent's console, if it has one.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS).is_ok() }
}

pub fn exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("freemonplay-host.exe"))
}

/// The Virtual Display Driver package the installer puts beside the program.
pub fn driver_package() -> Option<PathBuf> {
    let dir = exe().parent()?.to_path_buf();
    ["driver", "VirtualDisplayDriver"].iter().map(|d| dir.join(d)).find(|d| d.join("MttVDD.inf").exists())
}

/// Whether Windows Firewall lets screens reach this program on the networks this PC is on
/// now. Any rule counts, including one made by accepting Windows' own "allow access"
/// prompt (often for private networks only). Reading the rules needs no administrator rights.
pub fn firewall_allowed() -> bool {
    match firewall::allows(&exe()) {
        Ok(allowed) => allowed,
        Err(e) => {
            tracing::debug!("couldn't read the firewall rules ({e}); looking for ours by name");
            quiet_command("netsh")
                .args(["advfirewall", "firewall", "show", "rule", &format!("name={FIREWALL_RULE}")])
                .output()
                .is_ok_and(|o| o.status.success())
        }
    }
}

mod firewall {
    //! Windows Firewall's rules, through its COM interface (`INetFwPolicy2`).

    use std::path::Path;

    use windows::Win32::NetworkManagement::WindowsFirewall::{
        INetFwPolicy2, INetFwRule, NET_FW_ACTION_ALLOW, NET_FW_IP_PROTOCOL_ANY, NET_FW_IP_PROTOCOL_UDP,
        NET_FW_PROFILE_TYPE2, NET_FW_PROFILE2_DOMAIN, NET_FW_PROFILE2_PRIVATE, NET_FW_PROFILE2_PUBLIC,
        NET_FW_RULE_DIR_IN, NetFwPolicy2,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::System::Ole::IEnumVARIANT;
    use windows::Win32::System::Variant::{VARIANT, VariantClear};
    use windows::core::{Interface, Result};

    /// An inbound rule for our program: which network profiles it covers, and whether it
    /// allows (rather than blocks).
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Rule {
        pub profiles: i32,
        pub allow: bool,
    }

    /// Per active network profile: the firewall is off there, or a rule allows the program
    /// and none blocks it (a block wins, as in the firewall itself).
    pub fn allows(exe: &Path) -> Result<bool> {
        // SAFETY: COM calls on objects we create here. Joining the multithreaded apartment is
        // harmless when this thread is already in it (the host pins it at startup).
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)?;
            let rules = rules_for(&policy, exe)?;
            let active = policy.CurrentProfileTypes()?;
            for profile in [NET_FW_PROFILE2_DOMAIN, NET_FW_PROFILE2_PRIVATE, NET_FW_PROFILE2_PUBLIC] {
                if active & profile.0 == 0 || !policy.get_FirewallEnabled(profile)?.as_bool() {
                    continue;
                }
                if policy.get_BlockAllInboundTraffic(profile)?.as_bool() || !decide(&rules, profile) {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }

    /// Whether `rules` let the program through on `profile`.
    pub(super) fn decide(rules: &[Rule], profile: NET_FW_PROFILE_TYPE2) -> bool {
        let applies = |r: &&Rule| r.profiles & profile.0 != 0;
        let mut matching = rules.iter().filter(applies);
        matching.clone().any(|r| r.allow) && !matching.any(|r| !r.allow)
    }

    /// The enabled inbound rules for this program that cover UDP.
    unsafe fn rules_for(policy: &INetFwPolicy2, exe: &Path) -> Result<Vec<Rule>> {
        let exe = exe.to_string_lossy().to_lowercase();
        let mut out = Vec::new();
        // SAFETY: walking the rule collection's enumerator; each VARIANT is cleared after use.
        unsafe {
            let all: IEnumVARIANT = policy.Rules()?._NewEnum()?.cast()?;
            loop {
                let mut item = [VARIANT::default()];
                let mut fetched = 0u32;
                if all.Next(&mut item, &mut fetched).is_err() || fetched == 0 {
                    break;
                }
                let rule = (*item[0].Anonymous.Anonymous).Anonymous.pdispVal.as_ref().and_then(|d| d.cast::<INetFwRule>().ok());
                if let Some(rule) = rule
                    && let Some(r) = check(&rule, &exe)
                {
                    out.push(r);
                }
                let _ = VariantClear(&mut item[0]);
            }
        }
        Ok(out)
    }

    unsafe fn check(rule: &INetFwRule, exe: &str) -> Option<Rule> {
        // SAFETY: reading properties of a rule object.
        unsafe {
            let app = expand(&rule.ApplicationName().ok()?.to_string()).to_lowercase();
            let protocol = rule.Protocol().ok()?;
            let relevant = app == exe
                && rule.Direction().ok()? == NET_FW_RULE_DIR_IN
                && rule.Enabled().ok()?.as_bool()
                && (protocol == NET_FW_IP_PROTOCOL_ANY.0 || protocol == NET_FW_IP_PROTOCOL_UDP.0);
            relevant.then(|| Some(Rule { profiles: rule.Profiles().ok()?, allow: rule.Action().ok()? == NET_FW_ACTION_ALLOW }))?
        }
    }

    /// Rules may name programs with environment variables (`%ProgramFiles%\...`).
    fn expand(path: &str) -> String {
        let mut out = String::new();
        let mut rest = path;
        while let Some(start) = rest.find('%') {
            let Some(len) = rest[start + 1..].find('%') else { break };
            let name = &rest[start + 1..start + 1 + len];
            out.push_str(&rest[..start]);
            out.push_str(&std::env::var(name).unwrap_or_else(|_| format!("%{name}%")));
            rest = &rest[start + 2 + len..];
        }
        out.push_str(rest);
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_block_wins_and_profiles_matter() {
            let private_allow = Rule { profiles: NET_FW_PROFILE2_PRIVATE.0, allow: true };
            let all_block = Rule { profiles: NET_FW_PROFILE2_DOMAIN.0 | NET_FW_PROFILE2_PRIVATE.0 | NET_FW_PROFILE2_PUBLIC.0, allow: false };
            assert!(decide(&[private_allow], NET_FW_PROFILE2_PRIVATE));
            assert!(!decide(&[private_allow], NET_FW_PROFILE2_PUBLIC));
            assert!(!decide(&[private_allow, all_block], NET_FW_PROFILE2_PRIVATE));
            assert!(!decide(&[], NET_FW_PROFILE2_PRIVATE));
        }

        /// Reads this PC's real firewall rules: `FMP_FIREWALL_EXE=<path to freemonplay-host.exe>
        /// cargo test -- --ignored --nocapture firewall`.
        #[test]
        #[ignore]
        fn reads_this_pcs_rules() {
            let exe = std::env::var("FMP_FIREWALL_EXE").expect("set FMP_FIREWALL_EXE");
            println!("screens allowed through the firewall: {:?}", allows(Path::new(&exe)));
        }

        #[test]
        fn expands_environment_variables() {
            // SAFETY: a test-only variable, set before it's read.
            unsafe { std::env::set_var("FMP_TEST_DIR", r"C:\Apps") };
            assert_eq!(expand(r"%FMP_TEST_DIR%\x.exe"), r"C:\Apps\x.exe");
            assert_eq!(expand("no vars"), "no vars");
        }
    }
}

/// `--allow-firewall` (as administrator): lets screens connect to this program on any
/// network profile.
pub fn allow_firewall() -> Result<()> {
    let name = format!("name={FIREWALL_RULE}");
    // Replace rather than duplicate (the program may have moved).
    let _ = quiet_command("netsh").args(["advfirewall", "firewall", "delete", "rule", &name]).output();
    let program = format!("program={}", exe().display());
    let out = quiet_command("netsh")
        .args(["advfirewall", "firewall", "add", "rule", &name, "dir=in", "action=allow", &program, "enable=yes", "profile=any"])
        .output()?;
    if !out.status.success() {
        bail!("couldn't add the firewall rule (run this as administrator): {}", String::from_utf8_lossy(&out.stdout).trim());
    }
    println!("Screens can now reach Freemonplay through Windows Firewall.");
    Ok(())
}

/// `--remove-firewall` (as administrator; the uninstaller runs it).
pub fn remove_firewall() -> Result<()> {
    quiet_command("netsh").args(["advfirewall", "firewall", "delete", "rule", &format!("name={FIREWALL_RULE}")]).output()?;
    Ok(())
}

/// Runs this program again with `args`, as administrator (Windows asks first), and waits
/// for it. Returns whether it succeeded; false if the user said no.
pub fn run_elevated(args: &str) -> Result<bool> {
    let exe = HSTRING::from(exe().as_os_str());
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: valid strings that outlive the call; we wait on and close the process handle.
    unsafe {
        if ShellExecuteExW(&mut info).is_err() {
            // Most often: the user chose No at the administrator prompt.
            return Ok(false);
        }
        if info.hProcess.is_invalid() {
            return Ok(false);
        }
        let waited = WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        Ok(waited == WAIT_OBJECT_0 && code == 0)
    }
}

/// Starts Freemonplay when this user signs in (the per-user Run key).
pub fn start_at_login() -> bool {
    // SAFETY: a size-only query of our own value.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_start_at_login(on: bool) -> Result<()> {
    // SAFETY: writing or deleting our own value under the current user's Run key.
    let status = unsafe {
        if on {
            let command: Vec<u16> = format!("\"{}\"", exe().display()).encode_utf16().chain([0]).collect();
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0, Some(command.as_ptr().cast()), (command.len() * 2) as u32)
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE)
        }
    };
    status.ok().context("couldn't change the sign-in setting")
}

/// Opens a file or web address with its usual program.
pub fn open(target: &str) {
    // SAFETY: valid strings for the duration of the call.
    unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
}

pub fn open_path(path: &Path) {
    open(&path.to_string_lossy());
}

pub fn error_box(text: &str) {
    // SAFETY: a modal message box with valid strings.
    unsafe { MessageBoxW(None, &HSTRING::from(text), w!("Freemonplay Host"), MB_OK | MB_ICONERROR | MB_SETFOREGROUND) };
}

pub fn confirm(owner: HWND, text: &str) -> bool {
    // SAFETY: as above.
    unsafe { MessageBoxW(Some(owner), &HSTRING::from(text), w!("Freemonplay Host"), MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND) == IDYES }
}

/// Holds the "already running" marker for as long as this copy runs.
pub struct SingleInstance(#[allow(dead_code)] windows::Win32::Foundation::HANDLE);

/// None if Freemonplay is already running for this user.
pub fn single_instance() -> Option<SingleInstance> {
    // SAFETY: a named mutex, kept until exit.
    unsafe {
        let handle = CreateMutexW(None, false, w!(r"Local\freemonplay-host")).ok()?;
        (GetLastError() != ERROR_ALREADY_EXISTS).then_some(SingleInstance(handle))
    }
}
