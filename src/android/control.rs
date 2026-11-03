//! Control-message encoding for scrcpy-server 5.x.
//!
//! The layouts below mirror `app/src/control_msg.c` byte for byte.

use anyhow::{bail, Result};

pub const INJECT_TEXT_MAX_BYTES: usize = 300;
pub const SET_CLIPBOARD_MAX_BYTES: usize = 1 << 18;

pub const ACTION_DOWN: u8 = 0;
pub const ACTION_UP: u8 = 1;
pub const ACTION_MOVE: u8 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    InjectKeycode {
        action: u8,
        keycode: u32,
        repeat: u32,
        meta_state: u32,
    },
    InjectText(String),
    InjectTouch {
        action: u8,
        pointer_id: u64,
        x: u32,
        y: u32,
        width: u16,
        height: u16,
        pressure: u16,
        action_button: u32,
        buttons: u32,
    },
    InjectScroll {
        x: u32,
        y: u32,
        width: u16,
        height: u16,
        /// Scroll amount in scrcpy's [-16, 16] wheel units.
        hscroll: f32,
        vscroll: f32,
        buttons: u32,
    },
    BackOrScreenOn {
        action: u8,
    },
    ExpandNotificationPanel,
    ExpandSettingsPanel,
    CollapsePanels,
    GetClipboard {
        copy_key: u8,
    },
    SetClipboard {
        sequence: u64,
        paste: bool,
        text: String,
    },
    SetDisplayPower {
        on: bool,
    },
    RotateDevice,
    StartApp(String),
    ResetVideo,
    OpenHardKeyboardSettings,
}

impl ControlMessage {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(32);
        match self {
            ControlMessage::InjectKeycode {
                action,
                keycode,
                repeat,
                meta_state,
            } => {
                buf.push(0);
                buf.push(*action);
                buf.extend_from_slice(&keycode.to_be_bytes());
                buf.extend_from_slice(&repeat.to_be_bytes());
                buf.extend_from_slice(&meta_state.to_be_bytes());
            }
            ControlMessage::InjectText(text) => {
                let text = text.as_str();
                if text.len() > INJECT_TEXT_MAX_BYTES {
                    bail!(
                        "text chunk of {} bytes exceeds the scrcpy limit of {}",
                        text.len(),
                        INJECT_TEXT_MAX_BYTES
                    );
                }
                buf.push(1);
                write_string(&mut buf, text);
            }
            ControlMessage::InjectTouch {
                action,
                pointer_id,
                x,
                y,
                width,
                height,
                pressure,
                action_button,
                buttons,
            } => {
                buf.push(2);
                buf.push(*action);
                buf.extend_from_slice(&pointer_id.to_be_bytes());
                write_position(&mut buf, *x, *y, *width, *height);
                buf.extend_from_slice(&pressure.to_be_bytes());
                buf.extend_from_slice(&action_button.to_be_bytes());
                buf.extend_from_slice(&buttons.to_be_bytes());
                debug_assert_eq!(buf.len(), 32);
            }
            ControlMessage::InjectScroll {
                x,
                y,
                width,
                height,
                hscroll,
                vscroll,
                buttons,
            } => {
                buf.push(3);
                write_position(&mut buf, *x, *y, *width, *height);
                // scrcpy accepts [-16, 16] wheel units and normalizes to [-1, 1]
                // before converting to 16-bit fixed point.
                let h = float_to_i16_fixed(*hscroll / 16.0);
                let v = float_to_i16_fixed(*vscroll / 16.0);
                buf.extend_from_slice(&h.to_be_bytes());
                buf.extend_from_slice(&v.to_be_bytes());
                buf.extend_from_slice(&buttons.to_be_bytes());
                debug_assert_eq!(buf.len(), 21);
            }
            ControlMessage::BackOrScreenOn { action } => {
                buf.push(4);
                buf.push(*action);
            }
            ControlMessage::ExpandNotificationPanel => buf.push(5),
            ControlMessage::ExpandSettingsPanel => buf.push(6),
            ControlMessage::CollapsePanels => buf.push(7),
            ControlMessage::GetClipboard { copy_key } => {
                buf.push(8);
                buf.push(*copy_key);
            }
            ControlMessage::SetClipboard {
                sequence,
                paste,
                text,
            } => {
                if text.len() > SET_CLIPBOARD_MAX_BYTES {
                    bail!(
                        "clipboard text of {} bytes exceeds the scrcpy limit of {}",
                        text.len(),
                        SET_CLIPBOARD_MAX_BYTES
                    );
                }
                buf.push(9);
                buf.extend_from_slice(&sequence.to_be_bytes());
                buf.push(u8::from(*paste));
                write_string(&mut buf, text);
            }
            ControlMessage::SetDisplayPower { on } => {
                buf.push(10);
                buf.push(u8::from(*on));
            }
            ControlMessage::RotateDevice => buf.push(11),
            ControlMessage::StartApp(name) => {
                if name.len() > 255 {
                    bail!("app name too long for scrcpy");
                }
                buf.push(16);
                buf.push(name.len() as u8);
                buf.extend_from_slice(name.as_bytes());
            }
            ControlMessage::ResetVideo => buf.push(17),
            ControlMessage::OpenHardKeyboardSettings => buf.push(15),
        }
        Ok(buf)
    }
}

fn write_string(buf: &mut Vec<u8>, text: &str) {
    buf.extend_from_slice(&(text.len() as u32).to_be_bytes());
    buf.extend_from_slice(text.as_bytes());
}

fn write_position(buf: &mut Vec<u8>, x: u32, y: u32, width: u16, height: u16) {
    buf.extend_from_slice(&x.to_be_bytes());
    buf.extend_from_slice(&y.to_be_bytes());
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
}

fn float_to_i16_fixed(value: f32) -> i16 {
    let clamped = value.clamp(-1.0, 1.0);
    let raw = (clamped * 32768.0) as i32;
    raw.clamp(-32768, 32767) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycode_layout() {
        let bytes = ControlMessage::InjectKeycode {
            action: ACTION_DOWN,
            keycode: 4,
            repeat: 0,
            meta_state: 0,
        }
        .encode()
        .unwrap();
        assert_eq!(bytes, vec![0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn text_layout() {
        let bytes = ControlMessage::InjectText("hi".into()).encode().unwrap();
        assert_eq!(bytes, vec![1, 0, 0, 0, 2, b'h', b'i']);
    }

    #[test]
    fn touch_layout() {
        let bytes = ControlMessage::InjectTouch {
            action: ACTION_DOWN,
            pointer_id: 1,
            x: 2,
            y: 3,
            width: 1080,
            height: 2400,
            pressure: 0xFFFF,
            action_button: 0,
            buttons: 0,
        }
        .encode()
        .unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(&bytes[..2], &[2, ACTION_DOWN]);
        assert_eq!(&bytes[2..10], &1u64.to_be_bytes());
        assert_eq!(&bytes[10..14], &2u32.to_be_bytes());
        assert_eq!(&bytes[14..18], &3u32.to_be_bytes());
        assert_eq!(&bytes[18..20], &1080u16.to_be_bytes());
        assert_eq!(&bytes[20..22], &2400u16.to_be_bytes());
        assert_eq!(&bytes[22..24], &0xFFFFu16.to_be_bytes());
    }

    #[test]
    fn scroll_layout_and_clamping() {
        let bytes = ControlMessage::InjectScroll {
            x: 1,
            y: 2,
            width: 100,
            height: 200,
            hscroll: 0.0,
            vscroll: 16.0,
            buttons: 0,
        }
        .encode()
        .unwrap();
        assert_eq!(bytes.len(), 21);
        assert_eq!(&bytes[15..17], &32767u16.to_be_bytes());

        let bytes = ControlMessage::InjectScroll {
            x: 1,
            y: 2,
            width: 100,
            height: 200,
            hscroll: -16.0,
            vscroll: 0.0,
            buttons: 0,
        }
        .encode()
        .unwrap();
        assert_eq!(&bytes[13..15], &(-32768i16).to_be_bytes());
    }

    #[test]
    fn clipboard_layout() {
        let bytes = ControlMessage::SetClipboard {
            sequence: 7,
            paste: true,
            text: "a".into(),
        }
        .encode()
        .unwrap();
        assert_eq!(bytes[0], 9);
        assert_eq!(&bytes[1..9], &7u64.to_be_bytes());
        assert_eq!(bytes[9], 1);
        assert_eq!(&bytes[10..14], &1u32.to_be_bytes());
        assert_eq!(bytes[14], b'a');
    }

    #[test]
    fn rejects_oversized_text() {
        let msg = ControlMessage::InjectText("x".repeat(INJECT_TEXT_MAX_BYTES + 1));
        assert!(msg.encode().is_err());
    }
}
