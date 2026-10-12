//! The setup window: walks a new user through the two macOS permissions and connecting a
//! first screen. Shown on first launch and whenever Screen Recording is missing; "Setup
//! Guide…" in the menu reopens it. Its buttons call actions on the menu controller.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSFont, NSImageView, NSLayoutAttribute, NSStackView,
    NSStackViewDistribution, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString};

use rust_i18n::t;

use crate::platform;

const WIDTH: f64 = 540.0;
const TEXT_WIDTH: f64 = WIDTH - 2.0 * 28.0;
/// Where to get the client for other devices.
pub const DOWNLOAD_URL: &str = "https://github.com/moridin1138/sw-freemonplay/releases/latest";

/// The window and the parts that change as permissions are granted.
pub struct Welcome {
    pub window: Retained<NSWindow>,
    screen_status: Retained<NSTextField>,
    screen_button: Retained<NSButton>,
    restart_button: Retained<NSButton>,
    input_status: Retained<NSTextField>,
    input_button: Retained<NSButton>,
    /// The steps' button rows, hidden when none of their buttons apply.
    screen_row: Retained<NSStackView>,
    input_row: Retained<NSStackView>,
    code: Retained<NSTextField>,
    /// Whether Screen Recording was already allowed when this app started (a new grant
    /// needs a restart to take effect).
    screen_at_launch: bool,
}

impl Welcome {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject, pairing_code: &str, screen_at_launch: bool) -> Self {
        let button = |title: &str, action: Sel| -> Retained<NSButton> {
            // SAFETY: the target (the menu controller) lives as long as the app.
            unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm) }
        };

        // Header: the app icon, title and a sentence about what this Mac is for.
        let icon = NSApplication::sharedApplication(mtm).applicationIconImage().map(|image| {
            image.setSize(NSSize::new(64.0, 64.0));
            let view = NSImageView::imageViewWithImage(&image, mtm);
            view.into_super()
        });
        let title = label(mtm, &t!("host.welcome.title"), 22.0, true);
        let subtitle = wrapping(mtm, &t!("host.welcome.subtitle_mac"), 13.0);
        subtitle.setTextColor(Some(&NSColor::secondaryLabelColor()));
        subtitle.setPreferredMaxLayoutWidth(TEXT_WIDTH - 80.0);
        let titles = vstack(mtm, &[title.into_super().into_super(), subtitle.clone().into_super().into_super()], 4.0);
        let mut header_views: Vec<Retained<NSView>> = Vec::new();
        if let Some(icon) = icon {
            header_views.push(icon.into_super());
        }
        header_views.push(titles.into_super());
        let header = hstack(mtm, &header_views, 14.0);

        // Step 1: Screen Recording.
        let screen_status = label(mtm, "", 13.0, true);
        let screen_button = button(&t!("host.welcome.allow"), sel!(allowScreenRecording:));
        let restart_button = button(&t!("host.welcome.restart"), sel!(restartApp:));
        let (step1, screen_row) = step(
            mtm,
            &t!("host.welcome.screen_title"),
            &t!("host.welcome.screen_help"),
            &screen_status,
            &[screen_button.clone().into_super().into_super(), restart_button.clone().into_super().into_super()],
        );

        // Step 2: Accessibility.
        let input_status = label(mtm, "", 13.0, true);
        let input_button = button(&t!("host.welcome.allow"), sel!(allowAccessibility:));
        let (step2, input_row) = step(
            mtm,
            &t!("host.welcome.input_title"),
            &t!("host.welcome.input_help"),
            &input_status,
            &[input_button.clone().into_super().into_super()],
        );

        // Step 3: connect a screen.
        let code = label(mtm, &format_code(pairing_code), 26.0, true);
        code.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(26.0, 0.4)));
        code.setSelectable(true);
        let (step3, _) = step(
            mtm,
            &t!("host.welcome.connect_title"),
            &t!("host.welcome.connect_help"),
            &label(mtm, "", 13.0, false),
            &[
                code.clone().into_super().into_super(),
                button(&t!("host.welcome.copy"), sel!(copyCodeButton:)).into_super().into_super(),
                button(&t!("host.welcome.get_screen"), sel!(openDownload:)).into_super().into_super(),
            ],
        );

        let done = button(&t!("host.welcome.done"), sel!(closeWelcome:));
        done.setKeyEquivalent(&NSString::from_str("\r"));
        let footer_note = wrapping(mtm, &t!("host.welcome.footer"), 12.0);
        footer_note.setTextColor(Some(&NSColor::secondaryLabelColor()));
        footer_note.setPreferredMaxLayoutWidth(TEXT_WIDTH - 90.0);
        let footer = hstack(mtm, &[footer_note.into_super().into_super(), done.into_super().into_super()], 12.0);

        let content = vstack(
            mtm,
            &[header.into_super(), step1.into_super(), step2.into_super(), step3.into_super(), footer.into_super()],
            22.0,
        );
        content.setEdgeInsets(NSEdgeInsets { top: 24.0, left: 28.0, bottom: 22.0, right: 28.0 });

        // SAFETY: standard window creation on the main thread; kept alive by `Welcome`.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, 560.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str(&t!("host.welcome.window")));
        window.setContentView(Some(&content));
        window.setContentSize(NSSize::new(WIDTH, content.fittingSize().height));
        window.center();

        let welcome =
            Self { window, screen_status, screen_button, restart_button, input_status, input_button, screen_row, input_row, code, screen_at_launch };
        welcome.refresh(pairing_code);
        welcome
    }

    /// Updates the statuses (called every couple of seconds while the window is open).
    pub fn refresh(&self, pairing_code: &str) {
        let screen = platform::screen_recording_allowed();
        let (text, color) = match (screen, self.screen_at_launch) {
            (true, true) => (t!("host.welcome.allowed"), NSColor::systemGreenColor()),
            (true, false) => (t!("host.welcome.allowed_restart"), NSColor::systemOrangeColor()),
            (false, _) => (t!("host.welcome.not_allowed"), NSColor::systemOrangeColor()),
        };
        self.screen_status.setStringValue(&NSString::from_str(&text));
        self.screen_status.setTextColor(Some(&color));
        // Only the buttons that still do something.
        self.screen_button.setHidden(screen);
        self.restart_button.setHidden(!screen || self.screen_at_launch);
        self.screen_row.setHidden(screen && self.screen_at_launch);

        let input = platform::input_allowed();
        let (text, color) = if input {
            (t!("host.welcome.allowed"), NSColor::systemGreenColor())
        } else {
            (t!("host.welcome.not_allowed"), NSColor::systemOrangeColor())
        };
        self.input_status.setStringValue(&NSString::from_str(&text));
        self.input_status.setTextColor(Some(&color));
        self.input_button.setHidden(input);
        self.input_row.setHidden(input);

        self.code.setStringValue(&NSString::from_str(&format_code(pairing_code)));
        // Fit the window to what's showing (rows come and go as permissions change).
        if let Some(content) = self.window.contentView() {
            let height = content.fittingSize().height;
            if (self.window.contentLayoutRect().size.height - height).abs() > 1.0 {
                self.window.setContentSize(NSSize::new(WIDTH, height));
            }
        }
    }

    pub fn show(&self, mtm: MainThreadMarker) {
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
    }
}

/// "123456" as "123 456", easier to read and type.
pub fn format_code(code: &str) -> String {
    if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_string() }
}

/// A numbered step: a title with its status beside it, an explanation, and buttons.
fn step(
    mtm: MainThreadMarker,
    title: &str,
    detail: &str,
    status: &NSTextField,
    buttons: &[Retained<NSView>],
) -> (Retained<NSStackView>, Retained<NSStackView>) {
    let title = label(mtm, title, 15.0, true);
    let heading = hstack(mtm, &[title.into_super().into_super(), retained_view(status)], 10.0);
    let detail = wrapping(mtm, detail, 13.0);
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let row = hstack(mtm, buttons, 10.0);
    let step = vstack(mtm, &[heading.into_super(), detail.into_super().into_super(), row.clone().into_super()], 6.0);
    (step, row)
}

fn retained_view(field: &NSTextField) -> Retained<NSView> {
    // SAFETY: an NSTextField is an NSView; we just take another reference to it.
    unsafe { Retained::retain(field as *const NSTextField as *mut NSView).expect("non-null view") }
}

fn label(mtm: MainThreadMarker, text: &str, size: f64, bold: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    field.setFont(Some(&font));
    field
}

fn wrapping(mtm: MainThreadMarker, text: &str, size: f64) -> Retained<NSTextField> {
    let field = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(size)));
    field.setPreferredMaxLayoutWidth(TEXT_WIDTH);
    field.setSelectable(false);
    field
}

fn vstack(mtm: MainThreadMarker, views: &[Retained<NSView>], spacing: f64) -> Retained<NSStackView> {
    let stack = stack(mtm, views, spacing);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack
}

fn hstack(mtm: MainThreadMarker, views: &[Retained<NSView>], spacing: f64) -> Retained<NSStackView> {
    let stack = stack(mtm, views, spacing);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    stack.setAlignment(NSLayoutAttribute::CenterY);
    stack
}

fn stack(mtm: MainThreadMarker, views: &[Retained<NSView>], spacing: f64) -> Retained<NSStackView> {
    let refs: Vec<&NSView> = views.iter().map(|v| &**v).collect();
    let stack = NSStackView::stackViewWithViews(&NSArray::from_slice(&refs), mtm);
    stack.setSpacing(spacing);
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack
}
