//! JPEG decoding for scrub frames, thumbnails and JPEG streaming profiles (replaces the browser's <img> decode).

use anyhow::{Result, anyhow};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

#[derive(Clone)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RgbaImage({}x{})", self.width, self.height)
    }
}

pub fn decode_jpeg_rgba(data: &[u8]) -> Result<RgbaImage> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA).set_max_width(16384).set_max_height(16384);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), opts);
    let rgba = d.decode().map_err(|e| anyhow!("jpeg decode: {e:?}"))?;
    let info = d.info().ok_or_else(|| anyhow!("jpeg: no info"))?;
    Ok(RgbaImage { width: info.width as u32, height: info.height as u32, rgba })
}
