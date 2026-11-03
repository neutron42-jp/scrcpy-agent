//! H.264 decoding and the latest-frame store shared by the UI and the tools.

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use arc_swap::ArcSwapOption;
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;

#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub captured: Instant,
}

impl Frame {
    pub fn age_ms(&self) -> u64 {
        self.captured.elapsed().as_millis() as u64
    }
}

/// Holds only the newest decoded frame: the UI and screenshots never queue up.
pub struct FrameStore {
    current: ArcSwapOption<Frame>,
    seq: AtomicU64,
}

impl Default for FrameStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameStore {
    pub fn new() -> Self {
        Self {
            current: ArcSwapOption::empty(),
            seq: AtomicU64::new(0),
        }
    }

    pub fn store(&self, frame: Frame) {
        self.current.store(Some(Arc::new(frame)));
        self.seq.fetch_add(1, Ordering::Relaxed);
    }

    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.current.load_full()
    }

    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    pub fn wait_for_change(&self, after: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.seq.load(Ordering::Relaxed) > after {
                return true;
            }
            thread::sleep(Duration::from_millis(4));
        }
        false
    }
}

static FFMPEG_INIT: OnceLock<Result<(), String>> = OnceLock::new();

fn init_ffmpeg() -> Result<()> {
    let result = FFMPEG_INIT
        .get_or_init(|| ffmpeg::init().map_err(|err| format!("ffmpeg init failed: {err}")));
    match result {
        Ok(()) => Ok(()),
        Err(err) => bail!("{err}"),
    }
}

pub struct Decoder {
    opened: ffmpeg::decoder::Video,
    scaler: Option<scaling::Context>,
    scaled: Option<ffmpeg::frame::Video>,
    source: Option<(Pixel, u32, u32)>,
}

impl Decoder {
    pub fn new() -> Result<Self> {
        init_ffmpeg()?;
        let codec = ffmpeg::decoder::find(ffmpeg::codec::Id::H264)
            .context("the H.264 decoder is not available in the linked FFmpeg")?;
        let context = ffmpeg::codec::Context::new_with_codec(codec);
        let opened = context
            .decoder()
            .open_as(codec)
            .context("could not open the H.264 decoder")?
            .video()
            .context("the decoder is not a video decoder")?;
        Ok(Self {
            opened,
            scaler: None,
            scaled: None,
            source: None,
        })
    }

    /// Feeds one Annex-B packet and returns the last frame it produced.
    pub fn push(&mut self, data: &[u8]) -> Result<Option<Frame>> {
        if data.is_empty() {
            return Ok(None);
        }
        let packet = ffmpeg::Packet::copy(data);
        self.opened
            .send_packet(&packet)
            .context("send H.264 packet to decoder")?;

        let mut last = None;
        let mut frame = ffmpeg::frame::Video::empty();
        while self.opened.receive_frame(&mut frame).is_ok() {
            if let Some(converted) = self.convert(&frame)? {
                last = Some(converted);
            }
        }
        Ok(last)
    }

    fn convert(&mut self, frame: &ffmpeg::frame::Video) -> Result<Option<Frame>> {
        let width = frame.width();
        let height = frame.height();
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let format = frame.format();
        let needs_scaler = self
            .source
            .map(|(f, w, h)| f != format || w != width || h != height)
            .unwrap_or(true);
        if needs_scaler || self.scaler.is_none() {
            let scaler = scaling::Context::get(
                format,
                width,
                height,
                Pixel::RGBA,
                width,
                height,
                scaling::Flags::BILINEAR,
            )
            .context("create RGBA scaler")?;
            self.scaler = Some(scaler);
            self.scaled = Some(ffmpeg::frame::Video::new(Pixel::RGBA, width, height));
            self.source = Some((format, width, height));
        }

        let scaled = self.scaled.as_mut().expect("scaler output frame");
        self.scaler
            .as_mut()
            .expect("scaler")
            .run(frame, scaled)
            .context("convert decoded frame to RGBA")?;

        let stride = scaled.stride(0);
        let data = scaled.data(0);
        let row_bytes = width as usize * 4;
        let mut rgba = Vec::with_capacity(row_bytes * height as usize);
        for row in 0..height as usize {
            let start = row * stride;
            let end = start + row_bytes;
            if end > data.len() {
                break;
            }
            rgba.extend_from_slice(&data[start..end]);
        }
        if rgba.len() != row_bytes * height as usize {
            bail!(
                "short RGBA frame: {} bytes for {}x{}",
                rgba.len(),
                width,
                height
            );
        }

        Ok(Some(Frame {
            width,
            height,
            rgba,
            captured: Instant::now(),
        }))
    }
}

pub fn encode_png(frame: &Frame) -> Result<Vec<u8>> {
    let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
        .context("RGBA buffer does not match frame dimensions")?;
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .context("encode PNG")?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_keeps_latest_and_bumps_seq() {
        let store = FrameStore::new();
        assert!(store.latest().is_none());
        let frame = Frame {
            width: 2,
            height: 2,
            rgba: vec![0; 16],
            captured: Instant::now(),
        };
        store.store(frame);
        assert_eq!(store.seq(), 1);
        assert_eq!(store.latest().unwrap().width, 2);

        let before = store.seq();
        store.wait_for_change(before, Duration::from_millis(10));
        assert_eq!(store.seq(), before);

        store.store(Frame {
            width: 2,
            height: 2,
            rgba: vec![0; 16],
            captured: Instant::now(),
        });
        assert!(store.wait_for_change(before, Duration::from_millis(200)));
    }

    #[test]
    fn decodes_h264_fixture() {
        let data = std::fs::read("tests/fixtures/sample.h264").unwrap();
        let mut decoder = Decoder::new().unwrap();
        let frame = decoder.push(&data).unwrap();
        assert!(frame.is_some(), "decoder produced no frame");
        let frame = frame.unwrap();
        assert_eq!((frame.width, frame.height), (160, 120));
    }

    #[test]
    fn png_roundtrip() {
        let frame = Frame {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            captured: Instant::now(),
        };
        let png = encode_png(&frame).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }
}
