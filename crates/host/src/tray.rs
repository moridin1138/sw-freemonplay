//! The Windows tray app: an icon in the notification area with the same menu as the Mac's
//! menu bar app (pairing code, connected screens with live stats, settings, log), and a
//! setup dialog for the display driver, the firewall and connecting a first screen. The
//! window procedure runs on the main thread; the server runs on tokio's threads.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TDCBF_CLOSE_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_USE_COMMAND_LINKS, TDF_USE_HICON_MAIN, TaskDialogIndirect,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos,
    GetMessageW, GetSystemMetrics, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, KillTimer, LR_DEFAULTCOLOR, LoadIconW,
    LoadImageW, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SM_CXICON, SM_CXSMICON, SetForegroundWindow, SetTimer, TPM_BOTTOMALIGN,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE,
    WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_ENDSESSION, WM_NULL, WM_QUERYENDSESSION, WM_TIMER, WM_USER, WNDCLASSW, WS_OVERLAPPED,
};
use rust_i18n::t;
use windows::core::{HSTRING, PCWSTR, w};

use crate::encoder::CodecChoice;
use crate::platform::{display, system};
use crate::session::Ctx;

/// The window class; a second copy of Freemonplay finds the running one by it.
pub const CLASS: PCWSTR = w!("FreemonplayTray");
/// Tray icon events.
const WM_TRAY: u32 = WM_APP + 1;
/// Show the setup dialog (posted at launch, and by a second copy of Freemonplay).
pub const WM_SHOW_SETUP: u32 = WM_APP + 2;
/// Version 4 tray events: a click or Enter on the icon.
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const TIMER: usize = 1;

const FPS_CHOICES: [u32; 4] = [30, 60, 90, 120];
const CODEC_CHOICES: [CodecChoice; 3] = [CodecChoice::Auto, CodecChoice::H264, CodecChoice::H265];

fn codec_label(codec: CodecChoice) -> String {
    match codec {
        CodecChoice::Auto => t!("host.menu.automatic").into_owned(),
        CodecChoice::H264 => "H.264".into(),
        CodecChoice::H265 => "HEVC".into(),
    }
}
/// Where to get the client for other devices.
const DOWNLOAD_URL: &str = "https://github.com/moridin1138/sw-freemonplay/releases/latest";
/// Where to get the display driver when the installer didn't bring it.
const DRIVER_URL: &str = "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases";

/// What a menu item does.
#[derive(Clone, Copy)]
enum Action {
    CopyCode,
    NewCode,
    Disconnect(u64),
    Codec(CodecChoice),
    Fps(u32),
    ToggleInput,
    ToggleClipboard,
    ForgetAll,
    ToggleLogin,
    Setup,
    ShowLog,
    Quit,
}

struct Tray {
    ctx: Arc<Ctx>,
    log: PathBuf,
    hwnd: HWND,
    icon: HICON,
    big_icon: HICON,
    /// Explorer's "the taskbar was recreated" message: the icon has to be added again.
    taskbar_created: u32,
    in_setup: Cell<bool>,
}

thread_local! {
    static TRAY: std::cell::RefCell<Option<Rc<Tray>>> = const { std::cell::RefCell::new(None) };
}

pub fn run(ctx: Arc<Ctx>, log: PathBuf) {
    // SAFETY: standard window and tray icon setup on this thread, which runs the message
    // loop below until Quit.
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&class);
        // A hidden window: it receives the icon's clicks and owns the menu.
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Freemonplay Host"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(e) => {
                system::error_box(&t!("host.error.start", error = e.to_string()));
                return;
            }
        };
        let load = |size: i32| {
            LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
                .map(|h| HICON(h.0))
                .or_else(|_| LoadIconW(None, IDI_APPLICATION))
                .unwrap_or_default()
        };
        let tray = Rc::new(Tray {
            ctx,
            log,
            hwnd,
            icon: load(GetSystemMetrics(SM_CXSMICON)),
            big_icon: load(GetSystemMetrics(SM_CXICON)),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            in_setup: Cell::new(false),
        });
        TRAY.with(|t| *t.borrow_mut() = Some(tray.clone()));
        tray.add_icon();
        SetTimer(Some(hwnd), TIMER, 2000, None);
        // First launch, or screens can only mirror: show the setup dialog.
        if !tray.ctx.settings().welcomed || !display::driver_available() {
            let _ = PostMessageW(Some(hwnd), WM_SHOW_SETUP, WPARAM(0), LPARAM(0));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = KillTimer(Some(hwnd), TIMER);
        let _ = Shell_NotifyIconW(NIM_DELETE, &tray.icon_data());
    }
}

/// Asks a running tray app to quit, and waits (up to 5 s) for it to go.
pub fn quit_running() {
    for i in 0..50 {
        // SAFETY: finding the running copy's window by class and posting to it.
        let Ok(hwnd) = (unsafe { windows::Win32::UI::WindowsAndMessaging::FindWindowW(CLASS, PCWSTR::null()) }) else { return };
        if i == 0 {
            // SAFETY: as above.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(tray) = TRAY.with(|t| t.borrow().clone()) else {
        // SAFETY: default handling for messages before the tray exists.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_TRAY => {
            // Version 4: the event is in the low word.
            let event = (lparam.0 & 0xffff) as u32;
            if matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT) {
                tray.show_menu();
            }
            LRESULT(0)
        }
        WM_SHOW_SETUP => {
            tray.setup();
            LRESULT(0)
        }
        WM_TIMER => {
            tray.update_tip();
            LRESULT(0)
        }
        // Asked to close (the uninstaller, or taskkill): quit cleanly.
        WM_CLOSE => {
            tray.perform(Action::Quit);
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                // Signing out, shutting down or an installer replacing us: remove the
                // virtual monitors first.
                tray.perform(Action::Quit);
            }
            LRESULT(0)
        }
        m if m == tray.taskbar_created && m != 0 => {
            tray.add_icon();
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Tray {
    fn icon_data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        let tip: Vec<u16> = self.tip().encode_utf16().take(data.szTip.len() - 1).collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        data
    }

    fn add_icon(&self) {
        let mut data = self.icon_data();
        // SAFETY: a fully initialised NOTIFYICONDATAW for our window.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &data);
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }

    fn tip(&self) -> String {
        let ctx = &self.ctx;
        match ctx.registry.len() {
            0 => t!("host.tip.none", host = ctx.host_name).into_owned(),
            1 => t!("host.tip.one", host = ctx.host_name).into_owned(),
            n => t!("host.tip.many", host = ctx.host_name, count = n).into_owned(),
        }
    }

    fn update_tip(&self) {
        // SAFETY: as in add_icon.
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.icon_data()) };
    }

    fn show_menu(&self) {
        let ctx = &self.ctx;
        let settings = ctx.settings();
        let mut menu = Menu::new();

        menu.info(&t!("host.menu.header", host = ctx.host_name, port = ctx.port));
        menu.item(&t!("host.menu.copy_code", code = format_code(&ctx.store.pairing_code())), Action::CopyCode, false);
        menu.separator();

        let sessions = ctx.registry.snapshot();
        if sessions.is_empty() {
            menu.info(&t!("host.menu.no_screens"));
        }
        for s in sessions {
            let mut details = Menu::sub(&mut menu);
            for line in s.details() {
                match line {
                    Some(text) => details.info(&text),
                    None => details.separator(),
                }
            }
            details.separator();
            details.item(&t!("host.menu.disconnect"), Action::Disconnect(s.id), false);
            menu.submenu(&s.title(), details);
        }
        if !display::driver_available() {
            menu.separator();
            menu.item(&t!("host.menu.install_driver"), Action::Setup, false);
        }
        menu.separator();

        let mut codecs = Menu::sub(&mut menu);
        for codec in CODEC_CHOICES {
            codecs.add(&codec_label(codec), Some(Action::Codec(codec)), settings.codec == codec, ctx.overrides.codec.is_none());
        }
        menu.submenu(&t!("host.menu.codec"), codecs);
        let mut rates = Menu::sub(&mut menu);
        for fps in FPS_CHOICES {
            rates.add(&format!("{fps} fps"), Some(Action::Fps(fps)), settings.max_fps == fps, ctx.overrides.max_fps.is_none());
        }
        menu.submenu(&t!("host.menu.fps"), rates);
        menu.add(&t!("host.menu.allow_input"), Some(Action::ToggleInput), settings.allow_input, !ctx.overrides.no_input);
        menu.item(&t!("host.menu.clipboard"), Action::ToggleClipboard, settings.share_clipboard);
        menu.info(&t!("host.menu.next_time"));
        menu.separator();

        menu.item(&t!("host.menu.new_code"), Action::NewCode, false);
        let paired = ctx.store.paired_count();
        menu.add(&t!("host.menu.forget", count = paired), Some(Action::ForgetAll), false, paired > 0);
        menu.item(&t!("host.menu.login_pc"), Action::ToggleLogin, system::start_at_login());
        menu.item(&t!("host.menu.setup"), Action::Setup, false);
        menu.item(&t!("host.menu.log"), Action::ShowLog, false);
        menu.separator();
        menu.item(&t!("host.menu.quit"), Action::Quit, false);

        let mut at = POINT::default();
        // SAFETY: showing our popup menu at the pointer. Windows needs our window in front
        // for the menu to close when clicking elsewhere, and the null message afterwards.
        let chosen = unsafe {
            let _ = GetCursorPos(&mut at);
            let _ = SetForegroundWindow(self.hwnd);
            let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_RIGHTALIGN | TPM_BOTTOMALIGN;
            let id = TrackPopupMenu(menu.handle, flags, at.x, at.y, None, self.hwnd, None).0 as usize;
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            id
        };
        let action = chosen.checked_sub(1).and_then(|i| menu.actions.borrow().get(i).copied());
        drop(menu);
        if let Some(action) = action {
            self.perform(action);
        }
    }

    fn perform(&self, action: Action) {
        let ctx = &self.ctx;
        match action {
            Action::CopyCode => copy_to_clipboard(&ctx.store.pairing_code()),
            Action::NewCode => {
                let code = ctx.store.new_pairing_code();
                tracing::info!("new pairing code generated");
                copy_to_clipboard(&code);
            }
            Action::Disconnect(id) => ctx.registry.disconnect(id),
            Action::Codec(codec) => ctx.update_settings(|s| s.codec = codec),
            Action::Fps(fps) => ctx.update_settings(|s| s.max_fps = fps),
            Action::ToggleInput => ctx.update_settings(|s| s.allow_input = !s.allow_input),
            Action::ToggleClipboard => {
                ctx.update_settings(|s| s.share_clipboard = !s.share_clipboard);
                ctx.clipboard.set_enabled(ctx.settings().share_clipboard);
            }
            Action::ForgetAll => {
                let question = format!("{}\n\n{}", t!("host.forget.title"), t!("host.forget.body"));
                if system::confirm(self.hwnd, &question) {
                    match ctx.store.forget_all() {
                        Ok(()) => tracing::info!("forgot all paired clients"),
                        Err(e) => tracing::error!("couldn't forget paired clients: {e:#}"),
                    }
                }
            }
            Action::ToggleLogin => {
                if let Err(e) = system::set_start_at_login(!system::start_at_login()) {
                    tracing::error!("{e:#}");
                }
            }
            Action::Setup => self.setup(),
            Action::ShowLog => system::open_path(&self.log),
            Action::Quit => {
                tracing::info!("quitting");
                // Disconnect clients so their virtual monitors are removed cleanly.
                ctx.shutdown();
                std::thread::sleep(Duration::from_millis(300));
                // SAFETY: ends our message loop.
                unsafe { PostQuitMessage(0) };
            }
        }
    }

    /// The setup dialog: the display driver, the firewall rule and connecting a screen.
    /// Shown again after each step, with fresh statuses, until it's closed.
    fn setup(&self) {
        if self.in_setup.replace(true) {
            return;
        }
        const INSTALL: i32 = 101;
        const FIREWALL: i32 = 102;
        const COPY: i32 = 103;
        const DOWNLOAD: i32 = 104;
        loop {
            let driver = display::driver_available();
            let firewall = system::firewall_allowed();
            let package = system::driver_package();
            let code = format_code(&self.ctx.store.pairing_code());

            let content = format!(
                "{}\n\n{}",
                t!("host.welcome.subtitle_pc"),
                t!(
                    "host.setup.status",
                    driver = if driver { t!("host.setup.driver_ok") } else { t!("host.setup.driver_missing") },
                    firewall = if firewall { t!("host.setup.firewall_ok") } else { t!("host.setup.firewall_missing") },
                    code = code,
                ),
            );
            let mut buttons: Vec<(i32, HSTRING)> = Vec::new();
            if !driver {
                let text = if package.is_some() { t!("host.setup.install_driver") } else { t!("host.setup.get_driver") };
                buttons.push((INSTALL, HSTRING::from(&*text)));
            }
            if !firewall {
                buttons.push((FIREWALL, HSTRING::from(&*t!("host.setup.firewall"))));
            }
            buttons.push((COPY, HSTRING::from(&*t!("host.setup.copy", code = code))));
            buttons.push((DOWNLOAD, HSTRING::from(&*t!("host.setup.get_screen"))));
            let raw: Vec<TASKDIALOG_BUTTON> =
                buttons.iter().map(|(id, text)| TASKDIALOG_BUTTON { nButtonID: *id, pszButtonText: PCWSTR(text.as_ptr()) }).collect();
            let content = HSTRING::from(content);
            let footer = HSTRING::from(&*t!("host.setup.footer"));
            let (window_title, title) = (HSTRING::from(&*t!("host.welcome.window")), HSTRING::from(&*t!("host.welcome.title")));
            let config = TASKDIALOGCONFIG {
                cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
                dwFlags: TDF_USE_COMMAND_LINKS | TDF_USE_HICON_MAIN | TDF_ALLOW_DIALOG_CANCELLATION,
                dwCommonButtons: TDCBF_CLOSE_BUTTON,
                pszWindowTitle: PCWSTR(window_title.as_ptr()),
                Anonymous1: TASKDIALOGCONFIG_0 { hMainIcon: self.big_icon },
                pszMainInstruction: PCWSTR(title.as_ptr()),
                pszContent: PCWSTR(content.as_ptr()),
                cButtons: raw.len() as u32,
                pButtons: raw.as_ptr(),
                pszFooter: PCWSTR(footer.as_ptr()),
                ..Default::default()
            };
            let mut pressed = 0;
            // SAFETY: every string and the button array outlive the (modal) call.
            let shown = unsafe {
                let _ = SetForegroundWindow(self.hwnd);
                TaskDialogIndirect(&config, Some(&mut pressed), None, None)
            };
            if let Err(e) = shown {
                tracing::error!("couldn't show the setup dialog: {e}");
                break;
            }
            match pressed {
                INSTALL => match &package {
                    Some(folder) => self.elevated(&format!("--install-display-driver \"{}\"", folder.display())),
                    None => system::open(DRIVER_URL),
                },
                FIREWALL => self.elevated("--allow-firewall"),
                COPY => copy_to_clipboard(&self.ctx.store.pairing_code()),
                DOWNLOAD => system::open(DOWNLOAD_URL),
                _ => {
                    self.ctx.update_settings(|s| s.welcomed = true);
                    break;
                }
            }
        }
        self.in_setup.set(false);
    }

    /// Runs one of our administrator steps (Windows asks first).
    fn elevated(&self, args: &str) {
        match system::run_elevated(args) {
            Ok(true) => tracing::info!("finished: freemonplay-host {args}"),
            Ok(false) => tracing::warn!("didn't finish (or was cancelled): freemonplay-host {args}"),
            Err(e) => tracing::error!("couldn't run freemonplay-host {args}: {e:#}"),
        }
    }
}

/// A popup menu being built, and what each of its items does (by command id - 1).
struct Menu {
    handle: HMENU,
    actions: Rc<std::cell::RefCell<Vec<Action>>>,
    /// Submenus are destroyed with their parent.
    owned: bool,
}

impl Menu {
    fn new() -> Self {
        // SAFETY: a new, empty popup menu, destroyed on drop.
        Self { handle: unsafe { CreatePopupMenu() }.unwrap_or_default(), actions: Default::default(), owned: true }
    }

    /// A submenu sharing the parent's command ids.
    fn sub(parent: &mut Menu) -> Self {
        // SAFETY: as above; the parent destroys it.
        Self { handle: unsafe { CreatePopupMenu() }.unwrap_or_default(), actions: parent.actions.clone(), owned: false }
    }

    fn add(&mut self, text: &str, action: Option<Action>, checked: bool, enabled: bool) {
        let mut flags = MF_STRING;
        if checked {
            flags |= MF_CHECKED;
        }
        if !enabled || action.is_none() {
            flags |= MF_GRAYED;
        }
        let id = match action {
            Some(action) => {
                let mut actions = self.actions.borrow_mut();
                actions.push(action);
                actions.len()
            }
            None => 0,
        };
        // "&" marks a keyboard shortcut in menus; show it as itself.
        let text = HSTRING::from(text.replace('&', "&&"));
        // SAFETY: appending to our own menu with a valid string.
        let _ = unsafe { AppendMenuW(self.handle, flags, id, &text) };
    }

    fn item(&mut self, text: &str, action: Action, checked: bool) {
        self.add(text, Some(action), checked, true);
    }

    /// Greyed-out text, for information.
    fn info(&mut self, text: &str) {
        self.add(text, None, false, false);
    }

    fn separator(&mut self) {
        // SAFETY: appending a separator to our own menu.
        let _ = unsafe { AppendMenuW(self.handle, MF_SEPARATOR, 0, PCWSTR::null()) };
    }

    fn submenu(&mut self, text: &str, sub: Menu) {
        let text = HSTRING::from(text.replace('&', "&&"));
        // SAFETY: the parent takes ownership of the submenu.
        let _ = unsafe { AppendMenuW(self.handle, MF_POPUP, sub.handle.0 as usize, &text) };
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: destroys our menu and its submenus.
            let _ = unsafe { DestroyMenu(self.handle) };
        }
    }
}

/// "123456" as "123 456", easier to read and type.
fn format_code(code: &str) -> String {
    if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_string() }
}

fn copy_to_clipboard(text: &str) {
    if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        tracing::error!("couldn't copy to the clipboard: {e}");
    }
}
