//! Wire formats shared by scrcpy-server 5.x and this client.
//!
//! All integers are big-endian. The video socket starts with a 1-byte dummy
//! byte, a 64-byte device name, a 4-byte codec id, and then a sequence of
//! 12-byte-headered packets. The control socket carries control messages from
//! us and device messages back.

use anyhow::{bail, Result};

pub const HEADER_SIZE: usize = 12;

pub const CODEC_H264: u32 = 0x6832_3634; // "h264"
pub const CODEC_H265: u32 = 0x6832_3635; // "h265"
pub const CODEC_AV1: u32 = 0x0061_7631;
pub const CODEC_VP8: u32 = 0x0076_7038;
pub const CODEC_VP9: u32 = 0x0076_7039;

/// Bits 62 and 61 of the PTS field are packet flags.
const FLAG_CONFIG: u64 = 1 << 62;
const FLAG_KEY_FRAME: u64 = 1 << 61;
const PTS_MASK: u64 = FLAG_KEY_FRAME - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPacket {
    pub pts: u64,
    pub config: bool,
    pub key_frame: bool,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSession {
    pub width: u32,
    pub height: u32,
    pub client_resized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoHeader {
    /// Codec/size session packet (first byte has the MSB set).
    Session(StreamSession),
    Media(VideoPacket),
}

pub fn codec_name(codec_id: u32) -> &'static str {
    match codec_id {
        CODEC_H264 => "h264",
        CODEC_H265 => "h265",
        CODEC_AV1 => "av1",
        CODEC_VP8 => "vp8",
        CODEC_VP9 => "vp9",
        _ => "unknown",
    }
}

pub fn parse_header(header: &[u8; HEADER_SIZE]) -> VideoHeader {
    if header[0] & 0x80 != 0 {
        VideoHeader::Session(StreamSession {
            width: u32::from_be_bytes([header[4], header[5], header[6], header[7]]),
            height: u32::from_be_bytes([header[8], header[9], header[10], header[11]]),
            client_resized: header[3] & 1 != 0,
        })
    } else {
        let flags = u64::from_be_bytes(header[0..8].try_into().expect("8 bytes"));
        VideoHeader::Media(VideoPacket {
            pts: flags & PTS_MASK,
            config: flags & FLAG_CONFIG != 0,
            key_frame: flags & FLAG_KEY_FRAME != 0,
            size: u32::from_be_bytes([header[8], header[9], header[10], header[11]]),
        })
    }
}

/// A message sent by the device on the control socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceMessage {
    Clipboard(String),
    ClipboardAck { sequence: u64 },
}

pub const DEVICE_MSG_CLIPBOARD: u8 = 0;
pub const DEVICE_MSG_ACK_CLIPBOARD: u8 = 1;
pub const DEVICE_MSG_UHID_OUTPUT: u8 = 2;

/// Reads one device message payload (after the 1-byte type) from a byte slice.
/// Returns the message and the number of bytes consumed, or None if the buffer
/// is too short.
pub fn parse_device_message(kind: u8, buf: &[u8]) -> Result<Option<(DeviceMessage, usize)>> {
    match kind {
        DEVICE_MSG_CLIPBOARD => {
            if buf.len() < 12 {
                return Ok(None);
            }
            let len = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
            if buf.len() < 12 + len {
                return Ok(None);
            }
            let text = String::from_utf8_lossy(&buf[12..12 + len]).into_owned();
            Ok(Some((DeviceMessage::Clipboard(text), 12 + len)))
        }
        DEVICE_MSG_ACK_CLIPBOARD => {
            if buf.len() < 8 {
                return Ok(None);
            }
            let sequence = u64::from_be_bytes(buf[0..8].try_into().expect("8 bytes"));
            Ok(Some((DeviceMessage::ClipboardAck { sequence }, 8)))
        }
        DEVICE_MSG_UHID_OUTPUT => {
            if buf.len() < 2 {
                return Ok(None);
            }
            let size = u16::from_be_bytes([buf[0], buf[1]]) as usize;
            if buf.len() < 2 + size {
                return Ok(None);
            }
            Ok(Some((
                DeviceMessage::ClipboardAck { sequence: 0 },
                2 + size,
            )))
        }
        other => bail!("unknown device message type {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_packet() {
        let mut h = [0u8; HEADER_SIZE];
        h[0] = 0x80;
        h[4..8].copy_from_slice(&1080u32.to_be_bytes());
        h[8..12].copy_from_slice(&2400u32.to_be_bytes());
        assert_eq!(
            parse_header(&h),
            VideoHeader::Session(StreamSession {
                width: 1080,
                height: 2400,
                client_resized: false
            })
        );
    }

    #[test]
    fn parses_media_packet_flags_and_pts() {
        let mut h = [0u8; HEADER_SIZE];
        let flags = FLAG_KEY_FRAME | 123_456;
        h[0..8].copy_from_slice(&flags.to_be_bytes());
        h[8..12].copy_from_slice(&4242u32.to_be_bytes());
        assert_eq!(
            parse_header(&h),
            VideoHeader::Media(VideoPacket {
                pts: 123_456,
                config: false,
                key_frame: true,
                size: 4242
            })
        );
    }

    #[test]
    fn parses_config_flag() {
        let mut h = [0u8; HEADER_SIZE];
        h[0..8].copy_from_slice(&FLAG_CONFIG.to_be_bytes());
        h[8..12].copy_from_slice(&10u32.to_be_bytes());
        match parse_header(&h) {
            VideoHeader::Media(p) => assert!(p.config && !p.key_frame && p.size == 10),
            _ => panic!("expected media packet"),
        }
    }

    #[test]
    fn parses_clipboard_device_message() {
        let mut buf = vec![0u8; 12];
        buf[8..12].copy_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(b"hello");
        let (msg, used) = parse_device_message(DEVICE_MSG_CLIPBOARD, &buf)
            .unwrap()
            .unwrap();
        assert_eq!(used, 17);
        assert_eq!(msg, DeviceMessage::Clipboard("hello".into()));
    }

    #[test]
    fn needs_full_clipboard_payload() {
        let mut buf = vec![0u8; 12];
        buf[8..12].copy_from_slice(&5u32.to_be_bytes());
        assert!(parse_device_message(DEVICE_MSG_CLIPBOARD, &buf)
            .unwrap()
            .is_none());
    }
}
