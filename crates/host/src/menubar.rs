//! The menu bar app: a status item showing connected screens, the pairing code and
//! settings. AppKit owns the main thread; the server runs on tokio's threads.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSControlStateValueOff, NSControlStateValueOn, NSEvent, NSImage, NSMenu, NSMenuDelegate, NSMenuItem,
    NSPasteboard, NSPasteboardTypeString, NSScreen, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
    NSWorkspace,
};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString, NSTimer, NSURL};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use fmp_proto::Edge;
use rust_i18n::t;

use crate::encoder::CodecChoice;
use crate::registry::KvmRole;
use crate::session::Ctx;
use crate::welcome::{self, Welcome};

const FPS_CHOICES: [u32; 4] = [30, 60, 90, 120];

/// The Freemonplay mark redrawn for the menu bar: a vector template image (see
/// build/macos/icon/make-menubar-icon.swift).
const MENUBAR_ICON_PDF: &[u8] = include_bytes!("../../../build/macos/icon/menubar.pdf");
/// Where a keyboard-and-mouse-only screen's monitors sit relative to the main display.
const KVM_SIDES: [(Edge, &str); 4] = [
    (Edge::Right, "host.side.right"),
    (Edge::Left, "host.side.left"),
    (Edge::Top, "host.side.top"),
    (Edge::Bottom, "host.side.bottom"),
];

const CODEC_CHOICES: [CodecChoice; 3] = [CodecChoice::Auto, CodecChoice::H264, CodecChoice::H265];

fn codec_label(codec: CodecChoice) -> String {
    match codec {
        CodecChoice::Auto => t!("host.menu.automatic").into_owned(),
        CodecChoice::H264 => "H.264".into(),
        CodecChoice::H265 => "HEVC".into(),
    }
}

pub fn run(ctx: Arc<Ctx>, log: PathBuf) {
    let mtm = MainThreadMarker::new().expect("the menu bar must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon or app menu; just the status item.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let controller = Controller::new(mtm, ctx, log);
    app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    // Remembers where the user Cmd-drags the icon to.
    item.setAutosaveName(Some(&NSString::from_str("freemonplay")));
    if let Some(button) = item.button(mtm) {
        match menubar_icon().or_else(|| {
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("display.2"),
                Some(&NSString::from_str("Freemonplay Host")),
            )
        }) {
            Some(image) => {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            None => button.setTitle(&NSString::from_str("FMP")),
        }
    }
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    item.setMenu(Some(&menu));
    *controller.ivars().item.borrow_mut() = Some(item);
    *controller.ivars().menu.borrow_mut() = Some(menu);
    controller.update_status();
    // First launch, or nothing can work yet: show the setup window.
    if !controller.ivars().ctx.settings().welcomed || !controller.ivars().screen_at_launch {
        controller.show_welcome();
    }

    // Keep the screen count next to the icon current.
    let target: &AnyObject = &controller;
    // SAFETY: the controller implements `tick:` and outlives the timer (both live until exit).
    let _timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(2.0, target, sel!(tick:), None, true)
    };

    app.run();
}

struct Ivars {
    ctx: Arc<Ctx>,
    log: PathBuf,
    item: RefCell<Option<Retained<NSStatusItem>>>,
    menu: RefCell<Option<Retained<NSMenu>>>,
    checked_notch: std::cell::Cell<bool>,
    welcome: RefCell<Option<Welcome>>,
    /// Screen Recording as macOS saw it at launch (a new grant needs a restart).
    screen_at_launch: bool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FreemonplayMenuController"]
    #[ivars = Ivars]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSApplicationDelegate for Controller {
        /// Opening the app again (Finder, Spotlight, Launchpad) while it's running shows the
        /// menu at the pointer. That's the way in when the icon is hidden behind the notch.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            if let Some(menu) = self.ivars().menu.borrow().as_ref() {
                activate(self.mtm());
                menu.popUpMenuPositioningItem_atLocation_inView(None, NSEvent::mouseLocation(), None);
            }
            false
        }
    }

    unsafe impl NSMenuDelegate for Controller {
        /// Rebuilt every time the menu opens, so it always shows current sessions.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.rebuild(menu);
        }
    }

    impl Controller {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            self.update_status();
            if let Some(w) = self.ivars().welcome.borrow().as_ref()
                && w.window.isVisible()
            {
                w.refresh(&self.ivars().ctx.store.pairing_code());
            }
            // The icon only gets its final position after launch, so check on the first tick.
            if !self.ivars().checked_notch.replace(true) {
                self.warn_if_hidden_by_notch();
            }
        }

        #[unsafe(method(showWelcome:))]
        fn show_welcome_action(&self, _sender: &AnyObject) {
            self.show_welcome();
        }

        #[unsafe(method(allowScreenRecording:))]
        fn allow_screen_recording(&self, _sender: &AnyObject) {
            crate::platform::request_screen_recording();
            open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture");
        }

        #[unsafe(method(allowAccessibility:))]
        fn allow_accessibility(&self, _sender: &AnyObject) {
            crate::platform::request_input();
            open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
        }

        /// Relaunches the app so a new Screen Recording grant takes effect.
        #[unsafe(method(restartApp:))]
        fn restart_app(&self, _sender: &AnyObject) {
            let bundle = objc2_foundation::NSBundle::mainBundle().bundlePath().to_string();
            tracing::info!("restarting to pick up new permissions");
            // A detached shell waits for us to exit, then opens the app again.
            let spawned = std::process::Command::new("/bin/sh").args(["-c", "sleep 1; open \"$0\"", &bundle]).spawn();
            if let Err(e) = spawned {
                tracing::error!("couldn't restart: {e}");
                return;
            }
            self.ivars().ctx.shutdown();
            std::thread::sleep(Duration::from_millis(300));
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        #[unsafe(method(copyCodeButton:))]
        fn copy_code_button(&self, _sender: &AnyObject) {
            copy_to_clipboard(&self.ivars().ctx.store.pairing_code());
        }

        #[unsafe(method(openDownload:))]
        fn open_download(&self, _sender: &AnyObject) {
            open_url(welcome::DOWNLOAD_URL);
        }

        #[unsafe(method(closeWelcome:))]
        fn close_welcome(&self, _sender: &AnyObject) {
            self.ivars().ctx.update_settings(|s| s.welcomed = true);
            if let Some(w) = self.ivars().welcome.borrow().as_ref() {
                w.window.orderOut(None);
            }
        }

        #[unsafe(method(copyCode:))]
        fn copy_code(&self, _sender: &NSMenuItem) {
            copy_to_clipboard(&self.ivars().ctx.store.pairing_code());
        }

        #[unsafe(method(newCode:))]
        fn new_code(&self, _sender: &NSMenuItem) {
            let code = self.ivars().ctx.store.new_pairing_code();
            tracing::info!("new pairing code generated");
            copy_to_clipboard(&code);
        }

        #[unsafe(method(disconnect:))]
        fn disconnect(&self, sender: &NSMenuItem) {
            self.ivars().ctx.registry.disconnect(sender.tag() as u64);
        }

        /// Tag: session id * 4 + index into `KVM_SIDES`.
        #[unsafe(method(setKvmSide:))]
        fn set_kvm_side(&self, sender: &NSMenuItem) {
            let tag = sender.tag() as u64;
            if let Some(&(side, _)) = KVM_SIDES.get((tag % 4) as usize) {
                self.ivars().ctx.set_kvm_side(tag / 4, side);
            }
        }

        #[unsafe(method(setCodec:))]
        fn set_codec(&self, sender: &NSMenuItem) {
            if let Some(&codec) = CODEC_CHOICES.get(sender.tag() as usize) {
                self.ivars().ctx.update_settings(|s| s.codec = codec);
            }
        }

        #[unsafe(method(setFps:))]
        fn set_fps(&self, sender: &NSMenuItem) {
            let fps = sender.tag() as u32;
            self.ivars().ctx.update_settings(|s| s.max_fps = fps);
        }

        #[unsafe(method(toggleClipboard:))]
        fn toggle_clipboard(&self, _sender: &NSMenuItem) {
            let ctx = &self.ivars().ctx;
            ctx.update_settings(|s| s.share_clipboard = !s.share_clipboard);
            ctx.clipboard.set_enabled(ctx.settings().share_clipboard);
        }

        #[unsafe(method(toggleInput:))]
        fn toggle_input(&self, _sender: &NSMenuItem) {
            self.ivars().ctx.update_settings(|s| s.allow_input = !s.allow_input);
        }

        #[unsafe(method(forgetAll:))]
        fn forget_all(&self, _sender: &NSMenuItem) {
            let mtm = self.mtm();
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(&t!("host.forget.title")));
            alert.setInformativeText(&NSString::from_str(&t!("host.forget.body")));
            alert.addButtonWithTitle(&NSString::from_str(&t!("host.forget.button")));
            alert.addButtonWithTitle(&NSString::from_str(&t!("host.cancel")));
            activate(mtm);
            if alert.runModal() == NSAlertFirstButtonReturn {
                match self.ivars().ctx.store.forget_all() {
                    Ok(()) => tracing::info!("forgot all paired clients"),
                    Err(e) => tracing::error!("couldn't forget paired clients: {e:#}"),
                }
            }
        }

        #[unsafe(method(toggleLogin:))]
        fn toggle_login(&self, _sender: &NSMenuItem) {
            // SAFETY: plain ServiceManagement calls for this app's own login item.
            unsafe {
                let service = SMAppService::mainAppService();
                let result = if service.status() == SMAppServiceStatus::Enabled {
                    service.unregisterAndReturnError()
                } else {
                    service.registerAndReturnError()
                };
                if let Err(e) = result {
                    tracing::error!("couldn't change Open at Login: {}", e.localizedDescription());
                }
            }
        }

        #[unsafe(method(showLog:))]
        fn show_log(&self, _sender: &NSMenuItem) {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&self.ivars().log.to_string_lossy()));
            NSWorkspace::sharedWorkspace().openURL(&url);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &NSMenuItem) {
            tracing::info!("quitting from the menu");
            // Disconnect clients so their virtual displays are removed cleanly.
            self.ivars().ctx.shutdown();
            std::thread::sleep(Duration::from_millis(300));
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker, ctx: Arc<Ctx>, log: PathBuf) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            ctx,
            log,
            item: RefCell::new(None),
            menu: RefCell::new(None),
            checked_notch: std::cell::Cell::new(false),
            welcome: RefCell::new(None),
            screen_at_launch: crate::platform::screen_recording_allowed(),
        });
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }

    /// Shows the setup window, creating it the first time.
    fn show_welcome(&self) {
        let mtm = self.mtm();
        let code = self.ivars().ctx.store.pairing_code();
        let mut slot = self.ivars().welcome.borrow_mut();
        if slot.is_none() {
            let target: &AnyObject = self;
            *slot = Some(Welcome::new(mtm, target, &code, self.ivars().screen_at_launch));
        }
        let w = slot.as_ref().unwrap();
        w.refresh(&code);
        w.show(mtm);
    }

    /// On notched MacBooks, status icons that don't fit beside the notch are silently hidden.
    fn warn_if_hidden_by_notch(&self) {
        let mtm = self.mtm();
        let item = self.ivars().item.borrow();
        let frame = item.as_ref().and_then(|i| i.button(mtm)).and_then(|b| b.window()).map(|w| w.frame());
        let Some(frame) = frame else { return };
        let Some(screen) = NSScreen::mainScreen(mtm) else { return };
        let (left, right) = (screen.auxiliaryTopLeftArea(), screen.auxiliaryTopRightArea());
        let notch = (left.origin.x + left.size.width)..right.origin.x;
        let mid = frame.origin.x + frame.size.width / 2.0;
        if right.size.width > 0.0 && notch.contains(&mid) {
            tracing::warn!(
                "the menu bar icon is hidden behind the notch (too many menu bar icons). Cmd-drag \
                 other icons to make room, or open Freemonplay again to show its menu at the pointer."
            );
        } else {
            tracing::info!("menu bar icon at x={:.0}", frame.origin.x);
        }
    }

    /// Screen count next to the icon, plus a tooltip.
    fn update_status(&self) {
        let ctx = &self.ivars().ctx;
        let count = ctx.registry.len();
        let item = self.ivars().item.borrow();
        let Some(button) = item.as_ref().and_then(|i| i.button(self.mtm())) else { return };
        button.setTitle(&NSString::from_str(&if count > 0 { format!(" {count}") } else { String::new() }));
        let tip = match count {
            0 => t!("host.tip.none", host = ctx.host_name),
            1 => t!("host.tip.one", host = ctx.host_name),
            n => t!("host.tip.many", host = ctx.host_name, count = n),
        };
        button.setToolTip(Some(&NSString::from_str(&tip)));
    }

    fn rebuild(&self, menu: &NSMenu) {
        let ctx = &self.ivars().ctx;
        let settings = ctx.settings();
        menu.removeAllItems();

        menu.addItem(&self.item(&t!("host.menu.header", host = ctx.host_name, port = ctx.port), None, 0));
        let code = self.item(&t!("host.menu.code", code = welcome::format_code(&ctx.store.pairing_code())), Some(sel!(copyCode:)), 0);
        code.setToolTip(Some(&NSString::from_str(&t!("host.menu.code_tip"))));
        menu.addItem(&code);
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));

        let sessions = ctx.registry.snapshot();
        if sessions.is_empty() {
            menu.addItem(&self.item(&t!("host.menu.no_screens"), None, 0));
        } else if let Some(network) = crate::platform::network::describe() {
            menu.addItem(&self.item(&t!("host.menu.network_mac", summary = network.summary), None, 0));
            for tip in &network.advice {
                menu.addItem(&self.item(&format!("    {}", t!("host.menu.tip", tip = tip)), None, 0));
            }
        }
        for s in sessions {
            let details = NSMenu::new(self.mtm());
            details.setAutoenablesItems(false);
            for line in s.details() {
                match line {
                    Some(text) => details.addItem(&self.item(&text, None, 0)),
                    None => details.addItem(&NSMenuItem::separatorItem(self.mtm())),
                }
            }
            match s.kvm {
                Some(KvmRole::Mixed) => {
                    details.addItem(&NSMenuItem::separatorItem(self.mtm()));
                    details.addItem(&self.item(&t!("host.menu.kvm_mixed"), None, 0));
                    details.addItem(&self.item(&format!("    {}", t!("host.menu.kvm_back", keys = crate::shortcut('K'))), None, 0));
                }
                Some(KvmRole::Beside(current)) => {
                    details.addItem(&NSMenuItem::separatorItem(self.mtm()));
                    details.addItem(&self.item(&t!("host.menu.kvm_beside"), None, 0));
                    for (i, (side, label)) in KVM_SIDES.iter().enumerate() {
                        let item = self.item(&format!("    {}", t!(*label)), Some(sel!(setKvmSide:)), (s.id * 4 + i as u64) as isize);
                        item.setState(if current == *side { NSControlStateValueOn } else { NSControlStateValueOff });
                        details.addItem(&item);
                    }
                    details.addItem(&self.item(&format!("    {}", t!("host.menu.kvm_back", keys = crate::shortcut('K'))), None, 0));
                }
                None => {}
            }
            details.addItem(&NSMenuItem::separatorItem(self.mtm()));
            details.addItem(&self.item(&t!("host.menu.disconnect"), Some(sel!(disconnect:)), s.id as isize));
            let entry = self.item(&s.title(), Some(sel!(tick:)), 0);
            entry.setSubmenu(Some(&details));
            menu.addItem(&entry);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));

        let codec_menu = NSMenu::new(self.mtm());
        for (i, codec) in CODEC_CHOICES.iter().enumerate() {
            let item = self.item(&codec_label(*codec), Some(sel!(setCodec:)), i as isize);
            item.setState(if settings.codec == *codec { NSControlStateValueOn } else { NSControlStateValueOff });
            item.setEnabled(ctx.overrides.codec.is_none());
            codec_menu.addItem(&item);
        }
        let codec = self.item(&t!("host.menu.codec"), Some(sel!(tick:)), 0);
        codec.setSubmenu(Some(&codec_menu));
        menu.addItem(&codec);

        let fps_menu = NSMenu::new(self.mtm());
        for fps in FPS_CHOICES {
            let item = self.item(&format!("{fps} fps"), Some(sel!(setFps:)), fps as isize);
            item.setState(if settings.max_fps == fps { NSControlStateValueOn } else { NSControlStateValueOff });
            item.setEnabled(ctx.overrides.max_fps.is_none());
            fps_menu.addItem(&item);
        }
        let fps = self.item(&t!("host.menu.fps"), Some(sel!(tick:)), 0);
        fps.setSubmenu(Some(&fps_menu));
        menu.addItem(&fps);

        let input = self.item(&t!("host.menu.allow_input"), Some(sel!(toggleInput:)), 0);
        input.setState(if settings.allow_input { NSControlStateValueOn } else { NSControlStateValueOff });
        input.setEnabled(!ctx.overrides.no_input);
        input.setToolTip(Some(&NSString::from_str(&t!("host.menu.allow_input_tip"))));
        menu.addItem(&input);
        let clipboard = self.item(&t!("host.menu.clipboard"), Some(sel!(toggleClipboard:)), 0);
        clipboard.setState(if settings.share_clipboard { NSControlStateValueOn } else { NSControlStateValueOff });
        clipboard.setToolTip(Some(&NSString::from_str(&t!("host.menu.clipboard_tip"))));
        menu.addItem(&clipboard);
        menu.addItem(&self.item(&t!("host.menu.next_time"), None, 0));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));

        menu.addItem(&self.item(&t!("host.menu.new_code"), Some(sel!(newCode:)), 0));
        let paired = ctx.store.paired_count();
        let forget = self.item(&t!("host.menu.forget", count = paired), Some(sel!(forgetAll:)), 0);
        forget.setEnabled(paired > 0);
        menu.addItem(&forget);
        if crate::launched_from_app_bundle() {
            let login = self.item(&t!("host.menu.login_mac"), Some(sel!(toggleLogin:)), 0);
            // SAFETY: reading this app's own login item status.
            let enabled = unsafe { SMAppService::mainAppService().status() } == SMAppServiceStatus::Enabled;
            login.setState(if enabled { NSControlStateValueOn } else { NSControlStateValueOff });
            menu.addItem(&login);
        }
        menu.addItem(&self.item(&t!("host.menu.setup"), Some(sel!(showWelcome:)), 0));
        menu.addItem(&self.item(&t!("host.menu.log"), Some(sel!(showLog:)), 0));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        let quit = self.item(&t!("host.menu.quit"), Some(sel!(quit:)), 0);
        quit.setKeyEquivalent(&NSString::from_str("q"));
        menu.addItem(&quit);
    }

    /// A menu item. Without an action it's shown greyed out, as information.
    fn item(&self, title: &str, action: Option<Sel>, tag: isize) -> Retained<NSMenuItem> {
        // SAFETY: standard NSMenuItem construction; the target (self) outlives the menu.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(title),
                action,
                &NSString::from_str(""),
            )
        };
        if action.is_some() {
            let target: &AnyObject = self;
            // SAFETY: as above.
            unsafe { item.setTarget(Some(target)) };
        } else {
            item.setEnabled(false);
        }
        item.setTag(tag);
        item
    }
}

/// The embedded menu bar icon, as a template image macOS tints for light/dark menu bars.
fn menubar_icon() -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(MENUBAR_ICON_PDF);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    image.setSize(NSSize::new(18.0, 18.0));
    image.setAccessibilityDescription(Some(&NSString::from_str("Freemonplay Host")));
    Some(image)
}

fn open_url(url: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

fn copy_to_clipboard(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: NSPasteboardTypeString is a framework constant.
    pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

/// Bring our (Dock-less) app forward so a dialog appears on top.
fn activate(mtm: MainThreadMarker) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}
