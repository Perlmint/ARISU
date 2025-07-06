#![deny(unsafe_op_in_unsafe_fn)]
use std::cell::RefCell;

use crate::{
    config::Config,
    counter::Interval,
    server::{ServerController, ServerStatus},
};

mod settings;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusBarButton, NSVariableStatusItemLength,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString, NSTimer};
use tracing::{debug, error};

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
    start_stop_item: Retained<NSMenuItem>,
    #[allow(dead_code)]
    settings_item: Retained<NSMenuItem>,
}

struct Ivars {
    capture_interval: Interval,
    display_send_interval: Interval,
    ui: RefCell<Option<UiElements>>,
    config: RefCell<Option<Config>>,
    server_controller: ServerController,
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
            debug!("Application will terminate");
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

        #[unsafe(method(openSettings))]
        fn open_settings(&self) {
            self.show_settings_dialog();
        }

        #[unsafe(method(toggleServer))]
        fn toggle_server(&self) {
            self.toggle_server_state();
        }
    }
);

impl AppDelegate {
    fn new(
        capture_interval: Interval,
        display_send_interval: Interval,
        server_controller: ServerController,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let this = this.set_ivars(Ivars {
            capture_interval,
            display_send_interval,
            ui: RefCell::new(None),
            config: RefCell::new(None),
            server_controller,
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

            // Add start/stop server item
            let start_stop_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str("Start"),
                    Some(sel!(toggleServer)),
                    &NSString::from_str("s"),
                )
            };
            unsafe { start_stop_item.setTarget(Some(self)) };
            menu.addItem(&start_stop_item);

            // Add settings item
            let settings_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str("Settings..."),
                    Some(sel!(openSettings)),
                    &NSString::from_str(","),
                )
            };
            unsafe { settings_item.setTarget(Some(self)) };
            menu.addItem(&settings_item);

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
                start_stop_item,
                settings_item,
            }));

            // Load config
            match Config::load() {
                Ok(config) => {
                    self.ivars().config.replace(config);
                }
                Err(e) => {
                    error!("Failed to load config: {}", e);
                }
            }
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

        // Update server status
        let server_status = self.ivars().server_controller.get_status_sync();
        let (status_text, enabled) = match server_status {
            ServerStatus::Stopped => ("Start", true),
            ServerStatus::Starting => ("Starting...", false),
            ServerStatus::Running => ("Stop", true),
            ServerStatus::Stopping => ("Stopping...", false),
            ServerStatus::Error => ("Start (Error)", true),
        };

        // Only show FPS when server is running
        let show_fps = server_status == ServerStatus::Running;

        unsafe {
            if show_fps {
                ui.capture_fps_item.setTitle(&NSString::from_str(&format!(
                    "Capture FPS: {:.2}",
                    capture_fps
                )));
                ui.send_fps_item
                    .setTitle(&NSString::from_str(&format!("Send FPS: {:.2}", send_fps)));
            } else {
                ui.capture_fps_item
                    .setTitle(&NSString::from_str("Capture FPS: --"));
                ui.send_fps_item
                    .setTitle(&NSString::from_str("Send FPS: --"));
            }
            ui.start_stop_item
                .setTitle(&NSString::from_str(status_text));
            ui.start_stop_item.setEnabled(enabled);
        };
    }

    fn show_settings_dialog(&self) {
        debug!("Opening settings dialog");

        let mtm = MainThreadMarker::from(self);
        let current_config = self.ivars().config.borrow();
        let current_config = current_config.as_ref();

        if let Some(new_config) = settings::show_settings_dialog(mtm, current_config) {
            self.ivars().config.replace(Some(new_config));
        }
    }

    fn toggle_server_state(&self) {
        let server_status = self.ivars().server_controller.get_status_sync();
        let config = self.ivars().config.borrow();

        match server_status {
            ServerStatus::Stopped | ServerStatus::Error => {
                if let Some(config) = config.as_ref() {
                    if let Err(e) = self.ivars().server_controller.start_server(config.clone()) {
                        error!("Failed to start server: {}", e);
                    }
                } else {
                    error!("No configuration available. Please configure settings first.");
                }
            }
            ServerStatus::Running => {
                if let Err(e) = self.ivars().server_controller.stop_server() {
                    error!("Failed to stop server: {}", e);
                }
            }
            ServerStatus::Starting | ServerStatus::Stopping => {
                // Do nothing while transitioning
            }
        }
    }
}

pub fn run(
    capture_interval: Interval,
    display_send_interval: Interval,
    server_controller: ServerController,
) {
    let mtm: MainThreadMarker = MainThreadMarker::new().unwrap();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // configure the application delegate
    let delegate = AppDelegate::new(
        capture_interval,
        display_send_interval,
        server_controller,
        mtm,
    );
    let object = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(object));

    // run the app
    app.run();
}
