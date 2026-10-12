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

use crate::encoder::CodecChoice;
use crate::registry::KvmRole;
use crate::session::Ctx;

const FPS_CHOICES: [u32; 4] = [30, 60, 90, 120];

/// The freemonplay mark redrawn for the menu bar: a vector template image (see
/// build/macos/icon/make-menubar-icon.swift).
const MENUBAR_ICON_PDF: &[u8] = include_bytes!("../../../build/macos/icon/menubar.pdf");
/// Where a keyboard-and-mouse-only screen's monitors sit relative to the main display.
const KVM_SIDES: [(Edge, &str); 4] = [
    (Edge::Right, "Right of the main display"),
    (Edge::Left, "Left of the main display"),
    (Edge::Top, "Above the main display"),
    (Edge::Bottom, "Below the main display"),
];

const CODEC_CHOICES: [(CodecChoice, &str); 3] =
    [(CodecChoice::Auto, "Automatic"), (CodecChoice::H264, "H.264"), (CodecChoice::H265, "HEVC")];

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
                Some(&NSString::from_str("freemonplay")),
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
            // The icon only gets its final position after launch, so check on the first tick.
            if !self.ivars().checked_notch.replace(true) {
                self.warn_if_hidden_by_notch();
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
            if let Some(&(codec, _)) = CODEC_CHOICES.get(sender.tag() as usize) {
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
            alert.setMessageText(&NSString::from_str("Forget all paired screens?"));
            alert.setInformativeText(&NSString::from_str(
                "They'll need the pairing code to connect again. Screens connected now stay connected.",
            ));
            alert.addButtonWithTitle(&NSString::from_str("Forget"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
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
        });
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
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
                 other icons to make room, or open freemonplay again to show its menu at the pointer."
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
            0 => format!("freemonplay on {}: waiting for screens", ctx.host_name),
            1 => format!("freemonplay on {}: 1 screen connected", ctx.host_name),
            n => format!("freemonplay on {}: {n} screens connected", ctx.host_name),
        };
        button.setToolTip(Some(&NSString::from_str(&tip)));
    }

    fn rebuild(&self, menu: &NSMenu) {
        let ctx = &self.ivars().ctx;
        let settings = ctx.settings();
        menu.removeAllItems();

        menu.addItem(&self.item(&format!("freemonplay on {} (port {})", ctx.host_name, ctx.port), None, 0));
        let code = self.item(&format!("Pairing code: {}", ctx.store.pairing_code()), Some(sel!(copyCode:)), 0);
        code.setToolTip(Some(&NSString::from_str("Click to copy. New screens enter this code once.")));
        menu.addItem(&code);
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));

        let sessions = ctx.registry.snapshot();
        if sessions.is_empty() {
            menu.addItem(&self.item("No screens connected", None, 0));
        } else if let Some(network) = crate::platform::network::describe() {
            menu.addItem(&self.item(&format!("This Mac's network: {}", network.summary), None, 0));
            for tip in &network.advice {
                menu.addItem(&self.item(&format!("    Tip: {tip}"), None, 0));
            }
        }
        for s in sessions {
            let rate = if s.fps > 0.0 { format!(", {:.0} fps, {:.1} Mbit/s", s.fps, s.mbps) } else { String::new() };
            let latency = s.latency_ms().map(|ms| format!(", ~{ms:.0} ms")).unwrap_or_default();
            let title = format!("{}: {}×{} {}{rate}{latency}", s.client_name, s.width, s.height, s.codec);
            let details = NSMenu::new(self.mtm());
            details.setAutoenablesItems(false);
            details.addItem(&self.item(&format!("Address: {}", s.addr.ip().to_canonical()), None, 0));
            details.addItem(&self.item(&format!("Encoder: {}", s.encoder), None, 0));
            let bitrate = if s.target_mbps < s.max_mbps - 0.05 {
                format!("Bitrate: {:.1} of {:.0} Mbit/s (lowered: the network is behind)", s.target_mbps, s.max_mbps)
            } else {
                format!("Bitrate: up to {:.0} Mbit/s", s.max_mbps)
            };
            details.addItem(&self.item(&bitrate, None, 0));
            if s.loss_pct >= 0.1 {
                details.addItem(&self.item(&format!("Packets lost: {:.1}%", s.loss_pct), None, 0));
            }
            if let Some(network) = &s.network {
                details.addItem(&self.item(&format!("Screen's network: {}", network.summary), None, 0));
                for tip in &network.advice {
                    details.addItem(&self.item(&format!("    Tip: {tip}"), None, 0));
                }
            }
            details.addItem(&NSMenuItem::separatorItem(self.mtm()));
            match (s.latency_ms(), s.client) {
                (Some(total), Some(client)) => {
                    let lines = [
                        format!("Latency: about {total:.0} ms"),
                        format!("    This Mac (capture to sent): {:.1} ms", s.host_ms),
                        format!("    Network (half the {:.1} ms round trip): {:.1} ms", s.rtt_ms, s.rtt_ms / 2.0),
                        format!("    Screen (received to shown): {:.1} ms", client.display_ms),
                        format!("Screen is showing {:.0} fps", client.shown_fps),
                        match client.late_frames {
                            0 => "No frames delayed in transit".to_string(),
                            n => format!("{n} frames delayed in transit (worst {:.0} ms)", client.worst_late_ms),
                        },
                    ];
                    for line in lines {
                        details.addItem(&self.item(&line, None, 0));
                    }
                    let note = self.item("Plus up to one refresh while the screen waits to display.", None, 0);
                    details.addItem(&note);
                }
                _ => details.addItem(&self.item("Latency: waiting for the screen's first report", None, 0)),
            }
            match s.kvm {
                Some(KvmRole::Mixed) => {
                    details.addItem(&NSMenuItem::separatorItem(self.mtm()));
                    details.addItem(&self.item("Keyboard and mouse: move off this screen's edges onto that computer's other monitors", None, 0));
                    details.addItem(&self.item("    Ctrl+Alt+Shift+K brings them back", None, 0));
                }
                Some(KvmRole::Beside(current)) => {
                    details.addItem(&NSMenuItem::separatorItem(self.mtm()));
                    details.addItem(&self.item("Keyboard and mouse: its monitors sit", None, 0));
                    for (i, (side, label)) in KVM_SIDES.iter().enumerate() {
                        let item = self.item(&format!("    {label}"), Some(sel!(setKvmSide:)), (s.id * 4 + i as u64) as isize);
                        item.setState(if current == *side { NSControlStateValueOn } else { NSControlStateValueOff });
                        details.addItem(&item);
                    }
                    details.addItem(&self.item("    Ctrl+Alt+Shift+K brings them back", None, 0));
                }
                None => {}
            }
            details.addItem(&NSMenuItem::separatorItem(self.mtm()));
            details.addItem(&self.item("Disconnect", Some(sel!(disconnect:)), s.id as isize));
            let entry = self.item(&title, Some(sel!(tick:)), 0);
            entry.setSubmenu(Some(&details));
            menu.addItem(&entry);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));

        let codec_menu = NSMenu::new(self.mtm());
        for (i, (codec, label)) in CODEC_CHOICES.iter().enumerate() {
            let item = self.item(label, Some(sel!(setCodec:)), i as isize);
            item.setState(if settings.codec == *codec { NSControlStateValueOn } else { NSControlStateValueOff });
            item.setEnabled(ctx.overrides.codec.is_none());
            codec_menu.addItem(&item);
        }
        let codec = self.item("Video Codec", Some(sel!(tick:)), 0);
        codec.setSubmenu(Some(&codec_menu));
        menu.addItem(&codec);

        let fps_menu = NSMenu::new(self.mtm());
        for fps in FPS_CHOICES {
            let item = self.item(&format!("{fps} fps"), Some(sel!(setFps:)), fps as isize);
            item.setState(if settings.max_fps == fps { NSControlStateValueOn } else { NSControlStateValueOff });
            item.setEnabled(ctx.overrides.max_fps.is_none());
            fps_menu.addItem(&item);
        }
        let fps = self.item("Maximum Frame Rate", Some(sel!(tick:)), 0);
        fps.setSubmenu(Some(&fps_menu));
        menu.addItem(&fps);

        let input = self.item("Allow Control from Screens", Some(sel!(toggleInput:)), 0);
        input.setState(if settings.allow_input { NSControlStateValueOn } else { NSControlStateValueOff });
        input.setEnabled(!ctx.overrides.no_input);
        input.setToolTip(Some(&NSString::from_str("Mouse, keyboard and touch on a screen control this Mac.")));
        menu.addItem(&input);
        let clipboard = self.item("Share Clipboard with Screens", Some(sel!(toggleClipboard:)), 0);
        clipboard.setState(if settings.share_clipboard { NSControlStateValueOn } else { NSControlStateValueOff });
        clipboard.setToolTip(Some(&NSString::from_str(
            "Text copied here can be pasted on the screens' computers, and the other way round. \
             Passwords copied from password managers are never shared.",
        )));
        menu.addItem(&clipboard);
        menu.addItem(&self.item("Changes apply to screens that connect next.", None, 0));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));

        menu.addItem(&self.item("New Pairing Code (and Copy)", Some(sel!(newCode:)), 0));
        let paired = ctx.store.paired_count();
        let forget = self.item(&format!("Forget Paired Screens ({paired})…"), Some(sel!(forgetAll:)), 0);
        forget.setEnabled(paired > 0);
        menu.addItem(&forget);
        if crate::launched_from_app_bundle() {
            let login = self.item("Open at Login", Some(sel!(toggleLogin:)), 0);
            // SAFETY: reading this app's own login item status.
            let enabled = unsafe { SMAppService::mainAppService().status() } == SMAppServiceStatus::Enabled;
            login.setState(if enabled { NSControlStateValueOn } else { NSControlStateValueOff });
            menu.addItem(&login);
        }
        menu.addItem(&self.item("Show Log", Some(sel!(showLog:)), 0));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        let quit = self.item("Quit freemonplay", Some(sel!(quit:)), 0);
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
    image.setAccessibilityDescription(Some(&NSString::from_str("freemonplay")));
    Some(image)
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
