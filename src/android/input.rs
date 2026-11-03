//! Gestures and text/keyboard injection built on the control socket.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::control::{ControlMessage, ACTION_DOWN, ACTION_MOVE, ACTION_UP};
use super::keys;
use super::session::{Dims, Session};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionReport {
    pub changed: bool,
    pub elapsed_ms: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMode {
    Auto,
    Key,
    Clipboard,
}

impl Session {
    pub(crate) fn require_dims(&self) -> Result<Dims> {
        self.dims().ok_or_else(|| {
            anyhow::anyhow!("the video stream has not reported a size yet; try again in a moment")
        })
    }

    fn validate(&self, x: u32, y: u32, dims: Dims) -> Result<()> {
        if x >= dims.width || y >= dims.height {
            bail!(
                "point ({x}, {y}) is outside the {w}x{h} screen",
                w = dims.width,
                h = dims.height
            );
        }
        Ok(())
    }

    fn touch(&self, action: u8, x: u32, y: u32, dims: Dims) -> Result<()> {
        self.send(&ControlMessage::InjectTouch {
            action,
            pointer_id: 1,
            x,
            y,
            width: dims.width as u16,
            height: dims.height as u16,
            pressure: 0xFFFF,
            action_button: 0,
            buttons: 0,
        })
    }

    fn report(&self, before: u64, started: Instant, settle_ms: u32, dims: Dims) -> ActionReport {
        let changed = self
            .frames()
            .wait_for_change(before, Duration::from_millis(settle_ms as u64));
        ActionReport {
            changed,
            elapsed_ms: started.elapsed().as_millis() as u64,
            width: dims.width,
            height: dims.height,
        }
    }

    pub fn tap(&self, x: u32, y: u32, settle_ms: u32) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        self.validate(x, y, dims)?;
        let before = self.frames().seq();
        let started = Instant::now();
        self.touch(ACTION_DOWN, x, y, dims)?;
        thread::sleep(Duration::from_millis(30));
        self.touch(ACTION_UP, x, y, dims)?;
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn double_tap(
        &self,
        x: u32,
        y: u32,
        interval_ms: u32,
        settle_ms: u32,
    ) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        self.validate(x, y, dims)?;
        let before = self.frames().seq();
        let started = Instant::now();
        for tap in 0..2 {
            if tap > 0 {
                thread::sleep(Duration::from_millis(interval_ms as u64));
            }
            self.touch(ACTION_DOWN, x, y, dims)?;
            thread::sleep(Duration::from_millis(25));
            self.touch(ACTION_UP, x, y, dims)?;
        }
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn long_press(
        &self,
        x: u32,
        y: u32,
        duration_ms: u32,
        settle_ms: u32,
    ) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        self.validate(x, y, dims)?;
        let before = self.frames().seq();
        let started = Instant::now();
        self.touch(ACTION_DOWN, x, y, dims)?;
        thread::sleep(Duration::from_millis(duration_ms as u64));
        self.touch(ACTION_UP, x, y, dims)?;
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn swipe(
        &self,
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        duration_ms: u32,
        settle_ms: u32,
    ) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        self.validate(x1, y1, dims)?;
        self.validate(x2, y2, dims)?;
        let before = self.frames().seq();
        let started = Instant::now();

        let steps = (duration_ms / 12).clamp(2, 80);
        let step_delay = Duration::from_millis((duration_ms / steps).max(1) as u64);

        self.touch(ACTION_DOWN, x1, y1, dims)?;
        thread::sleep(step_delay);
        for step in 1..steps {
            let t = step as f64 / steps as f64;
            let x = lerp(x1, x2, t);
            let y = lerp(y1, y2, t);
            self.touch(ACTION_MOVE, x, y, dims)?;
            thread::sleep(step_delay);
        }
        self.touch(ACTION_UP, x2, y2, dims)?;
        Ok(self.report(before, started, settle_ms, dims))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn drag(
        &self,
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        hold_ms: u32,
        duration_ms: u32,
        settle_ms: u32,
    ) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        self.validate(x1, y1, dims)?;
        self.validate(x2, y2, dims)?;
        let before = self.frames().seq();
        let started = Instant::now();

        self.touch(ACTION_DOWN, x1, y1, dims)?;
        thread::sleep(Duration::from_millis(hold_ms as u64));

        let steps = (duration_ms / 12).clamp(2, 80);
        let step_delay = Duration::from_millis((duration_ms / steps).max(1) as u64);
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            self.touch(ACTION_MOVE, lerp(x1, x2, t), lerp(y1, y2, t), dims)?;
            thread::sleep(step_delay);
        }
        self.touch(ACTION_UP, x2, y2, dims)?;
        Ok(self.report(before, started, settle_ms, dims))
    }

    /// Wheel-style scrolling. Positive dy scrolls down (like Playwright's wheel),
    /// in units of one wheel notch.
    pub fn scroll(&self, x: u32, y: u32, dx: f32, dy: f32, settle_ms: u32) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        self.validate(x, y, dims)?;
        let before = self.frames().seq();
        let started = Instant::now();
        self.send(&ControlMessage::InjectScroll {
            x,
            y,
            width: dims.width as u16,
            height: dims.height as u16,
            hscroll: dx,
            vscroll: -dy,
            buttons: 0,
        })?;
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn press_key(
        &self,
        key: &str,
        modifiers: &[String],
        repeat: u32,
        settle_ms: u32,
    ) -> Result<ActionReport> {
        let keycode = keys::keycode(key)
            .ok_or_else(|| anyhow::anyhow!("unknown key {key:?}; pass an Android keycode number or a name such as ENTER, BACK, HOME, TAB, ESC, DEL, VOLUME_UP"))?;
        let meta = keys::meta_state(modifiers);
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        let before = self.frames().seq();
        let started = Instant::now();
        for i in 0..repeat.max(1) {
            self.send(&ControlMessage::InjectKeycode {
                action: ACTION_DOWN,
                keycode,
                repeat: i,
                meta_state: meta,
            })?;
            self.send(&ControlMessage::InjectKeycode {
                action: ACTION_UP,
                keycode,
                repeat: i,
                meta_state: meta,
            })?;
            thread::sleep(Duration::from_millis(20));
        }
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn back(&self, settle_ms: u32) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        let before = self.frames().seq();
        let started = Instant::now();
        self.send(&ControlMessage::BackOrScreenOn {
            action: ACTION_DOWN,
        })?;
        self.send(&ControlMessage::BackOrScreenOn { action: ACTION_UP })?;
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn type_text(&self, text: &str, mode: TextMode, settle_ms: u32) -> Result<ActionReport> {
        if text.is_empty() {
            bail!("text must not be empty");
        }
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        let before = self.frames().seq();
        let started = Instant::now();

        // Android IMEs compose key events, so a kana IME turns injected ASCII
        // into garbage. Paste through the clipboard by default; mode=key stays
        // available for raw key events.
        let clipboard_mode = match mode {
            TextMode::Clipboard => true,
            TextMode::Key => false,
            TextMode::Auto => true,
        };

        if clipboard_mode {
            // Non-ASCII text (CJK, emoji, ...) cannot be composed from
            // keycodes, so push it through the device clipboard and paste.
            let sequence = self.next_sequence();
            self.send(&ControlMessage::SetClipboard {
                sequence,
                paste: true,
                text: text.to_string(),
            })?;
            thread::sleep(Duration::from_millis(120));
        } else {
            let mut segment = String::new();
            let flush = |segment: &mut String| -> Result<()> {
                if segment.is_empty() {
                    return Ok(());
                }
                self.send(&ControlMessage::InjectText(std::mem::take(segment)))?;
                thread::sleep(Duration::from_millis(12));
                Ok(())
            };
            for ch in text.chars() {
                match ch {
                    '\n' => {
                        flush(&mut segment)?;
                        self.press_key_inner(66, 0)?;
                    }
                    '\t' => {
                        flush(&mut segment)?;
                        self.press_key_inner(61, 0)?;
                    }
                    _ => {
                        let mut buf = [0u8; 4];
                        let encoded = ch.encode_utf8(&mut buf);
                        if segment.len() + encoded.len() > super::control::INJECT_TEXT_MAX_BYTES {
                            flush(&mut segment)?;
                        }
                        segment.push_str(encoded);
                    }
                }
            }
            flush(&mut segment)?;
        }

        Ok(self.report(before, started, settle_ms, dims))
    }

    fn press_key_inner(&self, keycode: u32, meta: u32) -> Result<()> {
        self.send(&ControlMessage::InjectKeycode {
            action: ACTION_DOWN,
            keycode,
            repeat: 0,
            meta_state: meta,
        })?;
        self.send(&ControlMessage::InjectKeycode {
            action: ACTION_UP,
            keycode,
            repeat: 0,
            meta_state: meta,
        })?;
        thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    pub fn set_clipboard(&self, text: &str) -> Result<()> {
        let sequence = self.next_sequence();
        self.send(&ControlMessage::SetClipboard {
            sequence,
            paste: false,
            text: text.to_string(),
        })
    }

    pub fn clipboard_text(&self, timeout: Duration) -> Result<String> {
        let receiver = self.begin_clipboard_wait();
        if let Err(err) = self.send(&ControlMessage::GetClipboard { copy_key: 0 }) {
            self.cancel_clipboard_wait();
            return Err(err);
        }
        match receiver.recv_timeout(timeout) {
            Ok(text) => Ok(text),
            Err(_) => {
                self.cancel_clipboard_wait();
                bail!(
                    "the device did not return its clipboard within {} ms (Android < 7 does not support this)",
                    timeout.as_millis()
                )
            }
        }
    }

    pub fn rotate(&self, settle_ms: u32) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        let before = self.frames().seq();
        let started = Instant::now();
        self.send(&ControlMessage::RotateDevice)?;
        thread::sleep(Duration::from_millis(100));
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn notifications(&self, open: bool, settle_ms: u32) -> Result<ActionReport> {
        let _guard = self.lock_actions();
        let dims = self.require_dims()?;
        let before = self.frames().seq();
        let started = Instant::now();
        if open {
            self.send(&ControlMessage::ExpandNotificationPanel)?;
        } else {
            self.send(&ControlMessage::CollapsePanels)?;
        }
        thread::sleep(Duration::from_millis(80));
        Ok(self.report(before, started, settle_ms, dims))
    }

    pub fn wait_first_frame(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.frames().latest().is_some() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

fn lerp(from: u32, to: u32, t: f64) -> u32 {
    let value = from as f64 + (to as f64 - from as f64) * t;
    value.round().clamp(0.0, u32::MAX as f64) as u32
}

#[allow(dead_code)]
fn _assert_error_context(err: anyhow::Error) -> String {
    format!("{err:#}")
}

pub fn clamp_coordinate(value: f64, max: u32) -> u32 {
    value.round().clamp(0.0, max.saturating_sub(1) as f64) as u32
}

pub fn _context<T>(result: Result<T>) -> Result<T> {
    result.context("input")
}
