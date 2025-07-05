use crate::config::Config;
use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSModalResponseOK, NSOpenPanel, NSPanel,
    NSSecureTextField, NSTextField, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::path::PathBuf;
use tracing::{debug, error};

struct WindowFields {
    auth_id_field: Retained<NSTextField>,
    auth_password_field: Retained<NSSecureTextField>,
    certificate_field: Retained<NSTextField>,
    key_field: Retained<NSTextField>,
    result: Option<Config>,
    #[allow(dead_code)]
    panel: Retained<NSPanel>,
}

struct WindowIvars {
    fields: RefCell<Option<WindowFields>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = WindowIvars]
    struct SettingsWindowController;

    unsafe impl NSObjectProtocol for SettingsWindowController {}

    impl SettingsWindowController {
        #[unsafe(method(browseCertificate))]
        fn browse_certificate(&self) {
            let mtm = MainThreadMarker::from(self);
            if let Some(path) = show_file_picker(mtm, "Select Certificate File") {
                if let Some(fields) = self.ivars().fields.borrow().as_ref() {
                    unsafe { fields.certificate_field.setStringValue(&NSString::from_str(&path.display().to_string())) };
                }
            }
        }

        #[unsafe(method(browseKey))]
        fn browse_key(&self) {
            let mtm = MainThreadMarker::from(self);
            if let Some(path) = show_file_picker(mtm, "Select Key File") {
                if let Some(fields) = self.ivars().fields.borrow().as_ref() {
                    unsafe { fields.key_field.setStringValue(&NSString::from_str(&path.display().to_string())) };
                }
            }
        }

        #[unsafe(method(saveSettings))]
        fn save_settings(&self) {
            let mut fields = self.ivars().fields.borrow_mut();
            if let Some(fields) = fields.as_mut() {
                let auth_id = unsafe { fields.auth_id_field.stringValue().to_string() };
                let auth_password = unsafe { fields.auth_password_field.stringValue().to_string() };
                let certificate_path = unsafe { fields.certificate_field.stringValue().to_string() };
                let key_path = unsafe { fields.key_field.stringValue().to_string() };

                if auth_id.is_empty() || auth_password.is_empty() || certificate_path.is_empty() || key_path.is_empty() {
                    error!("All fields are required");
                    return;
                }

                let config = Config {
                    auth_id,
                    auth_password,
                    certificate: PathBuf::from(certificate_path),
                    key: PathBuf::from(key_path),
                };

                if let Err(e) = config.save() {
                    error!("Failed to save config: {}", e);
                    return;
                }

                debug!("Config saved successfully");
                fields.result = Some(config);

                // End the modal loop properly
                let mtm = MainThreadMarker::from(self);
                let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
                unsafe { app.stopModal() };
            }
        }

        #[unsafe(method(cancel))]
        fn cancel(&self) {
            // End the modal loop properly
            let mtm = MainThreadMarker::from(self);
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            unsafe { app.stopModal() };
        }
    }
);

impl SettingsWindowController {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let this = this.set_ivars(WindowIvars {
            fields: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }
}

fn show_file_picker(mtm: MainThreadMarker, title: &str) -> Option<PathBuf> {
    let panel = unsafe { NSOpenPanel::new(mtm) };
    unsafe { panel.setTitle(Some(&NSString::from_str(title))) };
    unsafe { panel.setCanChooseFiles(true) };
    unsafe { panel.setCanChooseDirectories(false) };
    unsafe { panel.setAllowsMultipleSelection(false) };

    let result = unsafe { panel.runModal() };
    if result == NSModalResponseOK {
        let urls = unsafe { panel.URLs() };
        if !urls.is_empty() {
            let url = urls.objectAtIndex(0);
            if let Some(path) = unsafe { url.path() } {
                return Some(PathBuf::from(path.to_string()));
            }
        }
    }
    None
}

pub fn show_settings_dialog(
    mtm: MainThreadMarker,
    current_config: Option<&Config>,
) -> Option<Config> {
    let panel_rect = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(500.0, 250.0));
    let style_mask = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;

    let panel = unsafe {
        NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            panel_rect,
            style_mask,
            NSBackingStoreType(2),
            false,
        )
    };

    panel.setTitle(&NSString::from_str("Settings"));

    let controller = SettingsWindowController::new(mtm);

    // Create content view
    let content_view = panel.contentView().unwrap();
    let content_frame = content_view.frame();

    // Auth ID field with label
    let auth_id_label_frame = NSRect::new(
        NSPoint::new(20.0, content_frame.size.height - 50.0),
        NSSize::new(80.0, 20.0),
    );
    let auth_id_label =
        unsafe { NSTextField::initWithFrame(NSTextField::alloc(mtm), auth_id_label_frame) };
    unsafe { auth_id_label.setStringValue(&NSString::from_str("Username:")) };
    unsafe { auth_id_label.setBezeled(false) };
    unsafe { auth_id_label.setDrawsBackground(false) };
    unsafe { auth_id_label.setEditable(false) };
    unsafe { auth_id_label.setSelectable(false) };
    unsafe { content_view.addSubview(&auth_id_label) };

    let auth_id_field_frame = NSRect::new(
        NSPoint::new(110.0, content_frame.size.height - 50.0),
        NSSize::new(250.0, 20.0),
    );
    let auth_id_field =
        unsafe { NSTextField::initWithFrame(NSTextField::alloc(mtm), auth_id_field_frame) };
    if let Some(config) = current_config {
        unsafe { auth_id_field.setStringValue(&NSString::from_str(&config.auth_id)) };
    }
    unsafe { auth_id_field.setPlaceholderString(Some(&NSString::from_str("Username"))) };
    unsafe { content_view.addSubview(&auth_id_field) };

    // Auth Password field with label
    let auth_password_label_frame = NSRect::new(
        NSPoint::new(20.0, content_frame.size.height - 80.0),
        NSSize::new(80.0, 20.0),
    );
    let auth_password_label =
        unsafe { NSTextField::initWithFrame(NSTextField::alloc(mtm), auth_password_label_frame) };
    unsafe { auth_password_label.setStringValue(&NSString::from_str("Password:")) };
    unsafe { auth_password_label.setBezeled(false) };
    unsafe { auth_password_label.setDrawsBackground(false) };
    unsafe { auth_password_label.setEditable(false) };
    unsafe { auth_password_label.setSelectable(false) };
    unsafe { content_view.addSubview(&auth_password_label) };

    let auth_password_field_frame = NSRect::new(
        NSPoint::new(110.0, content_frame.size.height - 80.0),
        NSSize::new(250.0, 20.0),
    );
    let auth_password_field = unsafe {
        NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), auth_password_field_frame)
    };
    if let Some(config) = current_config {
        unsafe { auth_password_field.setStringValue(&NSString::from_str(&config.auth_password)) };
    }
    unsafe { auth_password_field.setPlaceholderString(Some(&NSString::from_str("Password"))) };
    unsafe { content_view.addSubview(&auth_password_field) };

    // Certificate field with label and browse button
    let certificate_label_frame = NSRect::new(
        NSPoint::new(20.0, content_frame.size.height - 110.0),
        NSSize::new(80.0, 20.0),
    );
    let certificate_label =
        unsafe { NSTextField::initWithFrame(NSTextField::alloc(mtm), certificate_label_frame) };
    unsafe { certificate_label.setStringValue(&NSString::from_str("Certificate:")) };
    unsafe { certificate_label.setBezeled(false) };
    unsafe { certificate_label.setDrawsBackground(false) };
    unsafe { certificate_label.setEditable(false) };
    unsafe { certificate_label.setSelectable(false) };
    unsafe { content_view.addSubview(&certificate_label) };

    let certificate_field_frame = NSRect::new(
        NSPoint::new(110.0, content_frame.size.height - 110.0),
        NSSize::new(250.0, 20.0),
    );
    let certificate_field =
        unsafe { NSTextField::initWithFrame(NSTextField::alloc(mtm), certificate_field_frame) };
    if let Some(config) = current_config {
        unsafe {
            certificate_field.setStringValue(&NSString::from_str(
                &config.certificate.display().to_string(),
            ))
        };
    }
    unsafe {
        certificate_field.setPlaceholderString(Some(&NSString::from_str("Certificate path")))
    };
    unsafe { content_view.addSubview(&certificate_field) };

    let certificate_browse_frame = NSRect::new(
        NSPoint::new(370.0, content_frame.size.height - 110.0),
        NSSize::new(80.0, 20.0),
    );
    let certificate_browse_button =
        unsafe { NSButton::initWithFrame(NSButton::alloc(mtm), certificate_browse_frame) };
    unsafe { certificate_browse_button.setTitle(&NSString::from_str("Browse...")) };
    unsafe { certificate_browse_button.setTarget(Some(&controller)) };
    unsafe { certificate_browse_button.setAction(Some(sel!(browseCertificate))) };
    unsafe { content_view.addSubview(&certificate_browse_button) };

    // Key field with label and browse button
    let key_label_frame = NSRect::new(
        NSPoint::new(20.0, content_frame.size.height - 140.0),
        NSSize::new(80.0, 20.0),
    );
    let key_label = unsafe { NSTextField::initWithFrame(NSTextField::alloc(mtm), key_label_frame) };
    unsafe { key_label.setStringValue(&NSString::from_str("Key:")) };
    unsafe { key_label.setBezeled(false) };
    unsafe { key_label.setDrawsBackground(false) };
    unsafe { key_label.setEditable(false) };
    unsafe { key_label.setSelectable(false) };
    unsafe { content_view.addSubview(&key_label) };

    let key_field_frame = NSRect::new(
        NSPoint::new(110.0, content_frame.size.height - 140.0),
        NSSize::new(250.0, 20.0),
    );
    let key_field = unsafe { NSTextField::initWithFrame(NSTextField::alloc(mtm), key_field_frame) };
    if let Some(config) = current_config {
        unsafe { key_field.setStringValue(&NSString::from_str(&config.key.display().to_string())) };
    }
    unsafe { key_field.setPlaceholderString(Some(&NSString::from_str("Key path"))) };
    unsafe { content_view.addSubview(&key_field) };

    let key_browse_frame = NSRect::new(
        NSPoint::new(370.0, content_frame.size.height - 140.0),
        NSSize::new(80.0, 20.0),
    );
    let key_browse_button =
        unsafe { NSButton::initWithFrame(NSButton::alloc(mtm), key_browse_frame) };
    unsafe { key_browse_button.setTitle(&NSString::from_str("Browse...")) };
    unsafe { key_browse_button.setTarget(Some(&controller)) };
    unsafe { key_browse_button.setAction(Some(sel!(browseKey))) };
    unsafe { content_view.addSubview(&key_browse_button) };

    // Save and Cancel buttons
    let save_button_frame = NSRect::new(
        NSPoint::new(content_frame.size.width - 180.0, 20.0),
        NSSize::new(80.0, 30.0),
    );
    let save_button = unsafe { NSButton::initWithFrame(NSButton::alloc(mtm), save_button_frame) };
    unsafe { save_button.setTitle(&NSString::from_str("Save")) };
    unsafe { save_button.setTarget(Some(&controller)) };
    unsafe { save_button.setAction(Some(sel!(saveSettings))) };
    unsafe { content_view.addSubview(&save_button) };

    let cancel_button_frame = NSRect::new(
        NSPoint::new(content_frame.size.width - 90.0, 20.0),
        NSSize::new(80.0, 30.0),
    );
    let cancel_button =
        unsafe { NSButton::initWithFrame(NSButton::alloc(mtm), cancel_button_frame) };
    unsafe { cancel_button.setTitle(&NSString::from_str("Cancel")) };
    unsafe { cancel_button.setTarget(Some(&controller)) };
    unsafe { cancel_button.setAction(Some(sel!(cancel))) };
    unsafe { content_view.addSubview(&cancel_button) };

    // Store all fields in the controller
    controller.ivars().fields.replace(Some(WindowFields {
        auth_id_field,
        auth_password_field,
        certificate_field,
        key_field,
        result: None,
        panel: panel.clone(),
    }));

    panel.center();
    panel.makeKeyAndOrderFront(None);

    debug!("Showing settings modal dialog");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    unsafe { app.runModalForWindow(&panel) };

    panel.orderOut(None);

    // Return the saved config if user clicked Save
    if let Some(fields) = controller.ivars().fields.borrow().as_ref() {
        return fields.result.clone();
    }

    None
}
