#![deny(unsafe_op_in_unsafe_fn)]
use std::cell::RefCell;

use crate::counter::Interval;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusBarButton, NSVariableStatusItemLength,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString, NSTimer};

struct UiElements {
    #[allow(dead_code)]
    status_bar: Retained<NSStatusBar>, // Kept alive for proper cleanup
    #[allow(dead_code)]
    status_bar_button: Retained<NSStatusBarButton>, // Kept alive for proper cleanup
    #[allow(dead_code)]
    update_timer: Retained<NSTimer>, // Kept alive to prevent timer invalidation
    #[allow(dead_code)]
    menu: Retained<NSMenu>, // Kept alive for proper cleanup
    capture_fps_item: Retained<NSMenuItem>,
    send_fps_item: Retained<NSMenuItem>,
}

struct Ivars {
    capture_interval: Interval,
    display_send_interval: Interval,
    ui: RefCell<Option<UiElements>>,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject does not have any subclassing requirements.
    // - `AppDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);

            self.init(mtm);

            NSApplication::main(mtm);
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            println!("Will terminate!");
        }
    }

    impl AppDelegate {
        #[unsafe(method(onUpdateTimer))]
        fn update_timer(&self) {
            self.on_update_timer();
        }

        #[unsafe(method(quitApplication))]
        fn quit_application(&self) {
            let mtm = MainThreadMarker::from(self);
            let app = NSApplication::sharedApplication(mtm);
            unsafe { app.terminate(None) };
        }
    }
);

impl AppDelegate {
    fn new(
        capture_interval: Interval,
        display_send_interval: Interval,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let this = this.set_ivars(Ivars {
            capture_interval,
            display_send_interval,
            ui: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn init(&self, mtm: MainThreadMarker) {
        let status_bar = unsafe { NSStatusBar::systemStatusBar() };
        let status_bar_item =
            unsafe { status_bar.statusItemWithLength(NSVariableStatusItemLength) };
        if let Some(button) = unsafe { status_bar_item.button(mtm) } {
            let image = unsafe {
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str("apple.logo"),
                    None,
                )
            };
            unsafe { button.setImage(image.as_deref()) };
            
            // Create menu
            let menu = NSMenu::new(mtm);
            
            // Add FPS info items
            let capture_fps_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str("Capture FPS: --"),
                    None,
                    &NSString::from_str(""),
                )
            };
            unsafe { capture_fps_item.setEnabled(false) };
            menu.addItem(&capture_fps_item);
            
            let send_fps_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str("Send FPS: --"),
                    None,
                    &NSString::from_str(""),
                )
            };
            unsafe { send_fps_item.setEnabled(false) };
            menu.addItem(&send_fps_item);
            
            // Add separator
            let separator = NSMenuItem::separatorItem(mtm);
            menu.addItem(&separator);
            
            // Add quit item
            let quit_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str("Quit"),
                    Some(sel!(quitApplication)),
                    &NSString::from_str("q"),
                )
            };
            unsafe { quit_item.setTarget(Some(self)) };
            menu.addItem(&quit_item);
            unsafe { status_bar_item.setMenu(Some(&menu)) };
            
            let timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    1.0,
                    self,
                    sel!(onUpdateTimer),
                    None,
                    true,
                )
            };

            self.ivars().ui.replace(Some(UiElements {
                status_bar,
                status_bar_button: button,
                update_timer: timer,
                menu,
                capture_fps_item,
                send_fps_item,
            }));
        }
    }

    fn on_update_timer(&self) {
        let ui = self.ivars().ui.borrow();
        let Some(ui) = ui.as_ref() else {
            return;
        };

        let capture_interval = self.ivars().capture_interval.get();
        let capture_fps = 1.0 / capture_interval.as_secs_f64();
        let send_interval = self.ivars().display_send_interval.get();
        let send_fps = 1.0 / send_interval.as_secs_f64();

        unsafe {
            ui.capture_fps_item.setTitle(&NSString::from_str(&format!(
                "Capture FPS: {:.2}",
                capture_fps
            )));
            ui.send_fps_item.setTitle(&NSString::from_str(&format!(
                "Send FPS: {:.2}",
                send_fps
            )));
        };
    }
}

pub fn run(capture_interval: Interval, display_send_interval: Interval) {
    let mtm: MainThreadMarker = MainThreadMarker::new().unwrap();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // configure the application delegate
    let delegate = AppDelegate::new(capture_interval, display_send_interval, mtm);
    let object = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(object));

    // run the app
    app.run();
}
