use anyhow::Context;
use ironrdp::server::{KeyboardEvent, MouseEvent, RdpServerInputHandler};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGMouseButton, CGScrollEventUnit,
};
use tokio::sync::watch;

use crate::screen::ScreenSize;

pub struct InputHandler {
    last_mouse_point: CGPoint,
    down_mouse_button: Option<CGMouseButton>,
    modifier_state: Modifiers,
    client_screen_size: watch::Receiver<ScreenSize>,
}

#[derive(Default, Debug)]
struct Modifiers {
    shift: bool,
    command: bool,
    option: bool,
    control: bool,
}

impl InputHandler {
    pub fn new(client_screen_size: watch::Receiver<ScreenSize>) -> Self {
        Self {
            last_mouse_point: CGPoint { x: 0.0, y: 0.0 },
            down_mouse_button: None,
            modifier_state: Default::default(),
            client_screen_size,
        }
    }

    fn apply_modifier_to_event(&self, event: CFRetained<CGEvent>) -> CFRetained<CGEvent> {
        let mut flags = CGEventFlags(0);
        if self.modifier_state.command {
            flags |= CGEventFlags::MaskCommand;
        }
        if self.modifier_state.control {
            flags |= CGEventFlags::MaskControl;
        }
        if self.modifier_state.option {
            flags |= CGEventFlags::MaskAlternate;
        }
        if self.modifier_state.shift {
            flags |= CGEventFlags::MaskShift;
        }
        if flags.0 != 0 {
            unsafe { CGEvent::set_flags(Some(event.as_ref()), flags) };
        }
        event
    }

    fn convert_keyboard_event(
        &mut self,
        event: KeyboardEvent,
    ) -> anyhow::Result<CFRetained<CGEvent>> {
        fn convert_non_unicode_key(
            code: u8,
            extended: bool,
            pressed: bool,
            modifier: &mut Modifiers,
        ) -> Option<u16> {
            tracing::info!(?code, ?extended, ?pressed, ?modifier);
            Some(match (code, extended) {
                // Delete
                (14, false) => 0x33,
                // Command
                (91, true) => {
                    modifier.command = pressed;
                    0x37
                }
                // Ctrl
                (29, false) => {
                    modifier.control = pressed;
                    0x3B
                }
                // Left shift
                (42, false) => {
                    modifier.shift = pressed;
                    0x38
                }
                // Left alt/option
                (56, false) => {
                    modifier.option = pressed;
                    0x3A
                }
                // Return
                (28, false) => 0x24,
                // qwertyuiop
                (16, false) => 0x0C,
                (17, false) => 0x0D,
                (18, false) => 0x0E,
                (19, false) => 0x0F,
                (20, false) => 0x11,
                (21, false) => 0x10,
                (22, false) => 0x20,
                (23, false) => 0x22,
                (24, false) => 0x1F,
                (25, false) => 0x23,
                // asdfghjkl;
                (30, false) => 0x00,
                (31, false) => 0x01,
                (32, false) => 0x02,
                (33, false) => 0x03,
                (34, false) => 0x05,
                (35, false) => 0x04,
                (36, false) => 0x26,
                (37, false) => 0x28,
                (38, false) => 0x25,
                (39, false) => 0x29,
                // zxcvbnm
                (44, false) => 0x06,
                (45, false) => 0x07,
                (46, false) => 0x08,
                (47, false) => 0x09,
                (48, false) => 0x0B,
                (49, false) => 0x2D,
                (50, false) => 0x2E,
                // F1..F12
                (59, false) => 0x7A,
                (60, false) => 0x78,
                (61, false) => 0x63,
                (62, false) => 0x76,
                (63, false) => 0x60,
                (64, false) => 0x61,
                (65, false) => 0x62,
                (66, false) => 0x64,
                (67, false) => 0x65,
                (68, false) => 0x6D,
                (87, false) => 0x67,
                (88, false) => 0x6F,
                // Tab
                (15, false) => 0x30,
                // Arrow(left, up, down, right)
                (75, true) => 0x7B,
                (72, true) => 0x7E,
                (80, true) => 0x7D,
                (77, true) => 0x7C,
                // Del
                (83, true) => 0x75,
                // Home, End, PgUp, PgDn
                (71, true) => 0x73,
                (79, true) => 0x77,
                (73, true) => 0x74,
                (81, true) => 0x79,
                // ESC
                (1, false) => 0x35,
                // PrintScr, ScrollLock, Break
                (55, true) => return None,
                (70, false) => return None,
                (69, false) => return None,
                // 1..0
                (2, false) => 0x12,
                (3, false) => 0x13,
                (4, false) => 0x14,
                (5, false) => 0x15,
                (6, false) => 0x16,
                (7, false) => 0x17,
                (8, false) => 0x18,
                (9, false) => 0x19,
                (10, false) => 0x1A,
                (11, false) => 0x1B,
                _ => {
                    tracing::info!(?code, ?extended);
                    code as _
                }
            })
        }

        match event {
            KeyboardEvent::Pressed { code, extended } => {
                let code = convert_non_unicode_key(code, extended, true, &mut self.modifier_state)
                    .with_context(|| format!("Unknown code - {code}, {extended}"))?;
                unsafe { CGEvent::new_keyboard_event(None, code, true) }
                    .map(|event| self.apply_modifier_to_event(event))
            }
            .ok_or_else(|| anyhow::anyhow!("Failed to convert keyboard pressed event")),
            KeyboardEvent::Released { code, extended } => {
                let code = convert_non_unicode_key(code, extended, false, &mut self.modifier_state)
                    .with_context(|| format!("Unknown code - {code}, {extended}"))?;
                (unsafe { CGEvent::new_keyboard_event(None, code, false) })
                    .map(|event| self.apply_modifier_to_event(event))
                    .ok_or_else(|| anyhow::anyhow!("Failed to convert keyboard pressed event"))
            }
            KeyboardEvent::UnicodePressed(code) => {
                let event =
                    unsafe { CGEvent::new_keyboard_event(None, 0, true) }.ok_or_else(|| {
                        anyhow::anyhow!("Failed to convert keyboard event - {event:?}")
                    })?;
                unsafe { CGEvent::keyboard_set_unicode_string(Some(event.as_ref()), 1, &code) };
                Ok(event)
            }
            KeyboardEvent::UnicodeReleased(code) => {
                let event =
                    unsafe { CGEvent::new_keyboard_event(None, 0, false) }.ok_or_else(|| {
                        anyhow::anyhow!("Failed to convert keyboard event - {event:?}")
                    })?;
                unsafe { CGEvent::keyboard_set_unicode_string(Some(event.as_ref()), 1, &code) };
                Ok(event)
            }
            _ => Err(anyhow::anyhow!("Unhandled event - {event:?}")),
        }
    }
}

impl RdpServerInputHandler for InputHandler {
    fn keyboard(&mut self, event: KeyboardEvent) {
        let Ok(event) = self
            .convert_keyboard_event(event)
            .map_err(|e| tracing::error!(?e))
        else {
            return;
        };
        unsafe { CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event)) };
    }

    fn mouse(&mut self, event: MouseEvent) {
        use objc2_core_graphics::{CGDisplayMoveCursorToPoint, CGEventType};
        let event = match event {
            MouseEvent::LeftPressed => {
                self.down_mouse_button = Some(CGMouseButton::Left);
                unsafe {
                    CGEvent::new_mouse_event(
                        None,
                        CGEventType::LeftMouseDown,
                        self.last_mouse_point,
                        CGMouseButton::Left,
                    )
                }
            }
            MouseEvent::LeftReleased => {
                self.down_mouse_button = None;
                unsafe {
                    CGEvent::new_mouse_event(
                        None,
                        CGEventType::LeftMouseUp,
                        self.last_mouse_point,
                        CGMouseButton::Left,
                    )
                }
            }
            MouseEvent::RightPressed => {
                self.down_mouse_button = Some(CGMouseButton::Right);
                unsafe {
                    CGEvent::new_mouse_event(
                        None,
                        CGEventType::RightMouseDown,
                        self.last_mouse_point,
                        CGMouseButton::Right,
                    )
                }
            }
            MouseEvent::RightReleased => {
                self.down_mouse_button = None;
                unsafe {
                    CGEvent::new_mouse_event(
                        None,
                        CGEventType::RightMouseUp,
                        self.last_mouse_point,
                        CGMouseButton::Right,
                    )
                }
            }
            MouseEvent::Move { x, y } => {
                let screen_size = *self.client_screen_size.borrow_and_update();
                self.last_mouse_point.x =
                    (x as u32 * screen_size.server.0 as u32) as f64 / screen_size.client.0 as f64;
                self.last_mouse_point.y =
                    (y as u32 * screen_size.server.1 as u32) as f64 / screen_size.client.1 as f64;

                if let Some(down_button) = &self.down_mouse_button {
                    let down_button = *down_button;
                    unsafe {
                        CGEvent::new_mouse_event(
                            None,
                            match down_button {
                                CGMouseButton::Left => CGEventType::LeftMouseDragged,
                                CGMouseButton::Center => CGEventType::OtherMouseDragged,
                                CGMouseButton::Right => CGEventType::RightMouseDragged,
                                _ => {
                                    unreachable!("Unavailable case");
                                }
                            },
                            self.last_mouse_point,
                            down_button,
                        )
                    }
                } else {
                    let err = unsafe { CGDisplayMoveCursorToPoint(0, self.last_mouse_point) };
                    if err.0 != 0 {
                        tracing::error!("[CGDisplayMoveCursorToPoint] error - {}", err.0);
                    }
                    return;
                }
            }
            MouseEvent::VerticalScroll { value } => unsafe {
                CGEvent::new_scroll_wheel_event2(
                    None,
                    CGScrollEventUnit::Pixel,
                    1,
                    value as _,
                    0,
                    0,
                )
            },
            _ => {
                tracing::info!("Unknown mouse event {event:?}");
                return;
            }
        };
        let Some(event) = event else {
            tracing::error!("Failed to create mouse event");
            return;
        };
        unsafe { CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event)) };
    }
}
