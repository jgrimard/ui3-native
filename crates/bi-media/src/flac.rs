//! FLAC audio blocks from Blue Iris (`&audio=2`, `wFormatTag` 0xF1AC).
//!
//! UI3 passes each block to `AudioContext.decodeAudioData` (ui3.js:33570), i.e. a block is a self-contained FLAC
//! chunk that may begin with the `fLaC` marker and metadata blocks. This decoder accepts both forms: blocks with
//! a header (STREAMINFO becomes the decoder extradata) and bare frame data (decoded with the last STREAMINFO).

use anyhow::{Result, bail};
use ffmpeg_sys_next as ff;
use std::ptr;

pub struct FlacDecoder {
    ctx: *mut ff::AVCodecContext,
    parser: *mut ff::AVCodecParserContext,
    pkt: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
    streaminfo: Vec<u8>,
    pub sample_rate: u32,
}

unsafe impl Send for FlacDecoder {}

impl Default for FlacDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FlacDecoder {
    pub fn new() -> Self {
        unsafe {
            FlacDecoder {
                ctx: ptr::null_mut(),
                parser: ff::av_parser_init(ff::AVCodecID::AV_CODEC_ID_FLAC as i32),
                pkt: ff::av_packet_alloc(),
                frame: ff::av_frame_alloc(),
                streaminfo: Vec::new(),
                sample_rate: 0,
            }
        }
    }

    /// Uses the STREAMINFO Blue Iris puts after the WAVEFORMATEX in the stream header (`cbSize` = 34); its audio
    /// blocks are then bare FLAC frames.
    pub fn set_streaminfo(&mut self, streaminfo: &[u8]) -> Result<()> {
        if streaminfo.len() >= 34 && (self.ctx.is_null() || !same_format(streaminfo, &self.streaminfo)) {
            self.open(&streaminfo[..34])?;
        }
        Ok(())
    }

    fn open(&mut self, streaminfo: &[u8]) -> Result<()> {
        unsafe {
            // The FLAC parser keeps a pointer to the codec context it was last used with: replace both together.
            if !self.parser.is_null() {
                ff::av_parser_close(self.parser);
            }
            self.parser = ff::av_parser_init(ff::AVCodecID::AV_CODEC_ID_FLAC as i32);
            if self.parser.is_null() {
                bail!("no FLAC parser");
            }
            if !self.ctx.is_null() {
                ff::avcodec_free_context(&mut self.ctx);
            }
            let dec = ff::avcodec_find_decoder(ff::AVCodecID::AV_CODEC_ID_FLAC);
            if dec.is_null() {
                bail!("no FLAC decoder");
            }
            self.ctx = ff::avcodec_alloc_context3(dec);
            if !streaminfo.is_empty() {
                (*self.ctx).extradata = ff::av_mallocz(streaminfo.len() + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize) as *mut u8;
                ptr::copy_nonoverlapping(streaminfo.as_ptr(), (*self.ctx).extradata, streaminfo.len());
                (*self.ctx).extradata_size = streaminfo.len() as i32;
            }
            let r = ff::avcodec_open2(self.ctx, dec, ptr::null_mut());
            if r < 0 {
                bail!("FLAC avcodec_open2: {}", crate::decoder::av_err(r));
            }
            self.streaminfo = streaminfo.to_vec();
        }
        Ok(())
    }

    /// Decodes one Blue Iris audio block to mono f32 samples (appended to `out`). Returns the sample rate.
    pub fn decode_block(&mut self, block: &[u8], out: &mut Vec<f32>) -> Result<u32> {
        let mut data = block;
        if data.starts_with(b"fLaC") {
            let mut i = 4;
            let mut si: Option<&[u8]> = None;
            loop {
                if i + 4 > data.len() {
                    bail!("truncated FLAC metadata");
                }
                let last = data[i] & 0x80 != 0;
                let kind = data[i] & 0x7F;
                let len = ((data[i + 1] as usize) << 16) | ((data[i + 2] as usize) << 8) | data[i + 3] as usize;
                if i + 4 + len > data.len() {
                    bail!("truncated FLAC metadata block");
                }
                if kind == 0 {
                    si = Some(&data[i + 4..i + 4 + len]);
                }
                i += 4 + len;
                if last {
                    break;
                }
            }
            // Blue Iris repeats STREAMINFO in every block (with changing frame sizes / sample counts); only a
            // different sample rate, channel count or bit depth needs a new decoder.
            if let Some(si) = si
                && (self.ctx.is_null() || !same_format(si, &self.streaminfo))
            {
                self.open(si)?;
            }
            data = &data[i..];
        }
        if self.ctx.is_null() {
            self.open(&[])?;
        }
        unsafe {
            let mut rest = data;
            // Flush the parser at the end of every block: blocks are self-contained.
            let mut flushing = false;
            loop {
                let mut out_data: *mut u8 = ptr::null_mut();
                let mut out_size: i32 = 0;
                let (inp, inlen) = if flushing { (ptr::null(), 0) } else { (rest.as_ptr(), rest.len() as i32) };
                let used = ff::av_parser_parse2(self.parser, self.ctx, &mut out_data, &mut out_size, inp, inlen, ff::AV_NOPTS_VALUE, ff::AV_NOPTS_VALUE, 0);
                if used < 0 {
                    bail!("FLAC parse error");
                }
                if !flushing {
                    rest = &rest[used as usize..];
                }
                if out_size > 0 {
                    ff::av_new_packet(self.pkt, out_size);
                    ptr::copy_nonoverlapping(out_data, (*self.pkt).data, out_size as usize);
                    if ff::avcodec_send_packet(self.ctx, self.pkt) >= 0 {
                        while ff::avcodec_receive_frame(self.ctx, self.frame) >= 0 {
                            self.append_mono(out);
                        }
                    }
                    ff::av_packet_unref(self.pkt);
                }
                if flushing && out_size == 0 {
                    break;
                }
                if rest.is_empty() {
                    flushing = true;
                }
            }
            self.sample_rate = (*self.ctx).sample_rate.max(0) as u32;
            // A flushed FLAC parser cannot be fed again (it crashes inside FFmpeg): start a fresh one per block.
            ff::av_parser_close(self.parser);
            self.parser = ff::av_parser_init(ff::AVCodecID::AV_CODEC_ID_FLAC as i32);
            if self.parser.is_null() {
                bail!("no FLAC parser");
            }
        }
        Ok(self.sample_rate)
    }

    #[allow(clippy::missing_safety_doc)]
    unsafe fn append_mono(&mut self, out: &mut Vec<f32>) {
        unsafe {
            let f = self.frame;
            let n = (*f).nb_samples as usize;
            let ch = (*f).ch_layout.nb_channels.max(1) as usize;
            let fmt = (*f).format;
            let planar = ff::av_sample_fmt_is_planar(std::mem::transmute::<i32, ff::AVSampleFormat>(fmt)) != 0;
            let sample = |c: usize, i: usize| -> f32 {
                let (plane, idx) = if planar { (c, i) } else { (0, i * ch + c) };
                let p = *(*f).extended_data.add(plane);
                match std::mem::transmute::<i32, ff::AVSampleFormat>(fmt) {
                    ff::AVSampleFormat::AV_SAMPLE_FMT_S16 | ff::AVSampleFormat::AV_SAMPLE_FMT_S16P => *(p as *const i16).add(idx) as f32 / 32768.0,
                    ff::AVSampleFormat::AV_SAMPLE_FMT_S32 | ff::AVSampleFormat::AV_SAMPLE_FMT_S32P => *(p as *const i32).add(idx) as f32 / 2147483648.0,
                    ff::AVSampleFormat::AV_SAMPLE_FMT_FLT | ff::AVSampleFormat::AV_SAMPLE_FMT_FLTP => *(p as *const f32).add(idx),
                    _ => 0.0,
                }
            };
            for i in 0..n {
                let mut acc = 0.0;
                for c in 0..ch {
                    acc += sample(c, i);
                }
                out.push(acc / ch as f32);
            }
        }
    }
}

/// STREAMINFO bytes 10..13 hold sample rate (20 bits), channels (3) and bits per sample (5).
fn same_format(a: &[u8], b: &[u8]) -> bool {
    a.len() >= 13 && b.len() >= 13 && a[10..12] == b[10..12] && (a[12] & 0xF0) == (b[12] & 0xF0)
}

impl Drop for FlacDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.ctx.is_null() {
                ff::avcodec_free_context(&mut self.ctx);
            }
            if !self.parser.is_null() {
                ff::av_parser_close(self.parser);
            }
            ff::av_packet_free(&mut self.pkt);
            ff::av_frame_free(&mut self.frame);
        }
    }
}
