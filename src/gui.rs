#![deny(unsafe_op_in_unsafe_fn)]
use std::cell::Cell;

use crate::counter::Interval;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSImage, NSMenu,
    NSStatusBar, NSVariableStatusItemLength,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString};

struct Ivars {
    capture_interval: Interval,
    display_send_interval: Interval,
    status_bar: Cell<Option<Retained<NSStatusBar>>>,
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

            self.init_status_item(mtm);

            NSApplication::main(mtm);
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            println!("Will terminate!");
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
        });
        unsafe { msg_send![super(this), init] }
    }

    fn init_status_item(&self, mtm: MainThreadMarker) {
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
        }

        self.ivars().status_bar.set(Some(status_bar))
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
