//! Writer for the Blue Iris stream container (inverse of [`crate::stream_parser`]).
//! Used to synthesize test streams and by the offline mock server.

use crate::stream_parser::{StatusBlock, WAVE_FORMAT_MULAW};

pub struct StreamWriter {
    pub out: Vec<u8>,
}

impl StreamWriter {
    /// Writes the `blue` stream header. `audio_rate` adds a mu-law WAVEFORMATEX.
    pub fn new(fourcc: &[u8; 4], width: i32, height: i32, audio_rate: Option<u32>) -> Self {
        let mut bih = Vec::with_capacity(40);
        bih.extend_from_slice(&40u32.to_le_bytes());
        bih.extend_from_slice(&width.to_le_bytes());
        bih.extend_from_slice(&height.to_le_bytes());
        bih.extend_from_slice(&1u16.to_le_bytes());
        bih.extend_from_slice(&24u16.to_le_bytes());
        bih.extend_from_slice(fourcc);
        bih.extend_from_slice(&[0u8; 20]);
        let mut wfx = Vec::new();
        if let Some(rate) = audio_rate {
            wfx.extend_from_slice(&WAVE_FORMAT_MULAW.to_le_bytes());
            wfx.extend_from_slice(&1u16.to_le_bytes());
            wfx.extend_from_slice(&rate.to_le_bytes());
            wfx.extend_from_slice(&rate.to_le_bytes());
            wfx.extend_from_slice(&1u16.to_le_bytes());
            wfx.extend_from_slice(&8u16.to_le_bytes());
            wfx.extend_from_slice(&0u16.to_le_bytes());
        }
        let mut out = b"blue".to_vec();
        out.push(if audio_rate.is_some() { 2 } else { 1 });
        out.push((bih.len() + wfx.len()) as u8);
        out.extend(bih);
        out.extend(wfx);
        StreamWriter { out }
    }

    pub fn video(&mut self, pos: u16, time_ms: u32, utc_ms: u64, data: &[u8]) {
        self.out.extend_from_slice(b"Blue\x00");
        self.out.extend_from_slice(&pos.to_be_bytes());
        self.out.extend_from_slice(&time_ms.to_be_bytes());
        self.out.extend_from_slice(&utc_ms.to_le_bytes());
        self.out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        self.out.extend_from_slice(data);
    }

    pub fn audio(&mut self, data: &[u8]) {
        self.out.extend_from_slice(b"Blue\x01");
        self.out.extend_from_slice(&(data.len() as i32).to_be_bytes());
        self.out.extend_from_slice(data);
    }

    pub fn status(&mut self, s: &StatusBlock) {
        self.out.extend_from_slice(b"Blue\x02");
        self.out.push(6 + 22);
        for b in [s.rec, s.motion, s.check_fps, s.triggered, s.signal_lost, s.push_error, s.flash_error, s.force_movie] {
            self.out.push(b as u8);
        }
        self.out.push(s.other0);
        self.out.push(s.other1);
        self.out.extend_from_slice(&s.fps_x100.to_be_bytes());
        self.out.extend_from_slice(&s.apeak.to_be_bytes());
        self.out.extend_from_slice(&s.tpause.to_be_bytes());
    }

    pub fn end(&mut self) {
        self.out.extend_from_slice(b"Blue\x04");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_parser::{StreamEvent, StreamParser};

    #[test]
    fn roundtrip() {
        let mut w = StreamWriter::new(b"H264", 640, 360, Some(8000));
        w.video(0, 100, 5, &[0, 0, 1, 0x65, 1]);
        w.audio(&[0xFF; 160]);
        w.status(&StatusBlock { motion: true, fps_x100: 1000, ..Default::default() });
        w.end();
        let mut p = StreamParser::default();
        p.push(&w.out);
        let mut n = 0;
        while let Some(e) = p.next_event().unwrap() {
            n += 1;
            if let StreamEvent::Status(s) = e {
                assert!(s.motion);
                assert_eq!(s.fps_x100, 1000);
            }
        }
        assert_eq!(n, 5);
    }
}
