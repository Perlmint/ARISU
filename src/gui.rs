#![deny(unsafe_op_in_unsafe_fn)]
use std::cell::{Cell, RefCell};

use crate::counter::Interval;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSImage, NSMenu,
    NSStatusBar, NSStatusBarButton, NSVariableStatusItemLength,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSString, NSTimeInterval, NSTimer,
};

struct Ivars {
    capture_interval: Interval,
    display_send_interval: Interval,
    status_bar: Cell<Option<Retained<NSStatusBar>>>,
    status_bar_button: RefCell<Option<Retained<NSStatusBarButton>>>,
    update_timer: Cell<Option<Retained<NSTimer>>>,
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

        #[unsafe(method(onUpdateTimer))]
        fn update_timer(&self) {
            self.on_update_timer();
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
            status_bar: Cell::new(None),
            status_bar_button: RefCell::new(None),
            update_timer: Cell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn init(&self, mtm: MainThreadMarker) {
        let status_bar = unsafe { NSStatusBar::new() };
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
            self.ivars().status_bar_button.replace(Some(button));
        }

        self.ivars().status_bar.replace(Some(status_bar));

        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0,
                self,
                sel!(onUpdateTimer),
                None,
                true,
            )
        };
        self.ivars().update_timer.set(Some(timer));
    }

    fn on_update_timer(&self) {
        let bar_button = self.ivars().status_bar_button.borrow();
        let Some(bar_button) = bar_button.as_ref() else {
            if let Some(timer) = self.ivars().update_timer.take() {
                unsafe { timer.invalidate() };
            }
            return;
        };

        let capture_interval = self.ivars().capture_interval.get();
        let capture_fps = 1.0 / capture_interval.as_secs_f64();
        let send_interval = self.ivars().display_send_interval.get();
        let send_fps = 1.0 / send_interval.as_secs_f64();

        unsafe {
            bar_button.setTitle(&NSString::from_str(&format!(
                "{:.2}/{:.2}FPS",
                capture_fps, send_fps
            )))
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
