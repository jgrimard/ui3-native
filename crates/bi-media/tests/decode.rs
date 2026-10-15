//! Decodes synthetic H.264 / H.265 streams through the full path: elementary -> .bistream -> parser -> decoder.
//! Test media is generated into temp/testmedia (see README); tests are skipped if it is missing.

use bi_media::decoder::{HwAccel, VideoDecoder};
use bi_media::elementary::{read_elementary, synth_bistream};
use bi_proto::stream_parser::{StreamEvent, StreamParser};
use std::path::PathBuf;
use std::time::Instant;

fn media(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../temp/testmedia").join(name);
    if p.exists() { Some(p) } else { eprintln!("skipping: {} missing", p.display()); None }
}

fn run(name: &str, accel: HwAccel, expect: (u32, u32)) {
    let Some(p) = media(name) else { return };
    bi_media::quiet_ffmpeg_logs();
    let e = read_elementary(&p).unwrap();
    assert!(e.units[0].keyframe, "first AU should be a keyframe");
    let stream = synth_bistream(&e, 1_700_000_000_000, None);
    let mut parser = StreamParser::default();
    // Feed in awkward chunk sizes to exercise incremental parsing.
    let mut dec: Option<VideoDecoder> = None;
    let mut frames = Vec::new();
    let t0 = Instant::now();
    for chunk in stream.chunks(7919) {
        parser.push(chunk);
        while let Some(ev) = parser.next_event().unwrap() {
            if let StreamEvent::Video(f) = ev {
                let d = dec.get_or_insert_with(|| VideoDecoder::new(f.codec, accel, None).unwrap());
                d.decode(&f.data, f.time_ms, &mut frames).unwrap();
            }
        }
    }
    let d = dec.unwrap();
    let mut tail = Vec::new();
    let desc = d.description();
    let mut d = d;
    d.flush(&mut tail).unwrap();
    frames.extend(tail);
    let el = t0.elapsed();
    eprintln!("{name}: {} AUs -> {} frames via {desc} in {el:?} ({:.0} fps)", e.units.len(), frames.len(), frames.len() as f64 / el.as_secs_f64());
    assert_eq!(frames.len(), e.units.len());
    let f = frames[frames.len() / 2].cpu().unwrap();
    assert_eq!((f.width, f.height), expect);
    assert_eq!(f.y.len(), (expect.0 * expect.1) as usize);
    // testsrc2 is colorful: luma must not be flat.
    let min = *f.y.iter().min().unwrap();
    let max = *f.y.iter().max().unwrap();
    assert!(max - min > 100, "luma range {min}..{max}");
    // pts pass through in order
    assert!(frames.windows(2).all(|w| w[1].pts() > w[0].pts()));
    if accel == HwAccel::D3D11Va {
        assert!(frames.iter().all(|f| f.is_hw()));
    }
}

#[test]
fn h264_software() { run("test1080.h264", HwAccel::Software, (1920, 1080)); }
#[test]
fn h264_d3d11va() { run("test1080.h264", HwAccel::D3D11Va, (1920, 1080)); }
#[test]
fn h265_software() { run("test1440.hevc", HwAccel::Software, (2560, 1440)); }
#[test]
fn h265_d3d11va() { run("test1440.hevc", HwAccel::D3D11Va, (2560, 1440)); }
#[test]
fn h265_10bit_d3d11va() { run("test720_10bit.hevc", HwAccel::D3D11Va, (1280, 720)); }
#[test]
fn h265_10bit_software() { run("test720_10bit.hevc", HwAccel::Software, (1280, 720)); }

/// Muxes the synthetic H.265 stream into MP4 without re-encoding, then demuxes it and decodes it again.
#[test]
fn h265_mp4_remux_roundtrip() {
    let Some(p) = media("test1440.hevc") else { return };
    let e = read_elementary(&p).unwrap();
    let out = std::env::temp_dir().join("ui3native_remux_test.mp4");
    let mut m = bi_media::mux::Mp4Muxer::create(&out, e.codec, e.width, e.height).unwrap();
    for (i, au) in e.units.iter().enumerate() {
        m.write(&au.data, (i as f64 * 1000.0 / e.fps) as i64, au.keyframe, e.codec).unwrap();
    }
    m.finish().unwrap();
    let back = read_elementary(&out).unwrap();
    assert_eq!(back.codec, bi_proto::VideoCodec::H265);
    assert_eq!(back.units.len(), e.units.len());
    assert_eq!((back.width, back.height), (2560, 1440));
    let mut d = VideoDecoder::new(back.codec, HwAccel::Software, None).unwrap();
    let mut frames = Vec::new();
    for (i, au) in back.units.iter().enumerate() {
        d.decode(&au.data, i as i64, &mut frames).unwrap();
    }
    d.flush(&mut frames).unwrap();
    assert_eq!(frames.len(), e.units.len());
    let _ = std::fs::remove_file(out);
}

/// A complete FLAC file (as UI3 receives per audio block) decodes to the expected sample count.
#[test]
fn flac_block_decode() {
    let Some(p) = media("tone.flac") else { return };
    let bytes = std::fs::read(p).unwrap();
    let mut d = bi_media::flac::FlacDecoder::new();
    let mut out = Vec::new();
    let rate = d.decode_block(&bytes, &mut out).unwrap();
    assert_eq!(rate, 8000);
    assert!((3900..=4100).contains(&out.len()), "{} samples", out.len());
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.1, "peak {peak}");
}

/// Zero-copy: decoding on a shared D3D11 device yields GPU frames in shareable textures.
#[test]
fn h265_zero_copy_gpu_frames() {
    let Some(p) = media("test1440.hevc") else { return };
    let luid = unsafe {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let f: IDXGIFactory1 = CreateDXGIFactory1().unwrap();
        f.EnumAdapters1(0).unwrap().GetDesc1().unwrap().AdapterLuid
    };
    let dev = bi_media::d3d::SharedD3D11::new(luid).unwrap();
    let e = read_elementary(&p).unwrap();
    let mut d = VideoDecoder::new_with_device(e.codec, HwAccel::D3D11Va, None, Some(dev)).unwrap();
    let mut frames = Vec::new();
    let t0 = Instant::now();
    for (i, au) in e.units.iter().enumerate() {
        d.decode(&au.data, i as i64, &mut frames).unwrap();
        // Release frames like the UI would, so ring slots recycle.
        if frames.len() > 3 {
            frames.drain(..frames.len() - 3);
        }
    }
    eprintln!("zero-copy: {} via {} in {:?}", e.units.len(), d.description(), t0.elapsed());
    assert!(d.description().contains("zero-copy"), "{}", d.description());
    let last = frames.last().unwrap();
    assert!(matches!(last, bi_media::DecodedFrame::Gpu(_)));
    assert_eq!(last.size(), (2560, 1440));
}

/// Decodes every real-server capture in temp/captures (made by `bi-capture`) on the GPU. Skipped if none.
#[test]
fn real_captures_d3d11va() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../temp/captures");
    let Ok(rd) = std::fs::read_dir(&dir) else { return eprintln!("skipping: no captures") };
    bi_media::quiet_ffmpeg_logs();
    for p in rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bistream")) {
        let data = std::fs::read(&p).unwrap();
        let mut parser = StreamParser::default();
        parser.push(&data);
        let (mut dec, mut frames, mut aus) = (None::<VideoDecoder>, Vec::new(), 0);
        while let Some(ev) = parser.next_event().unwrap() {
            if let StreamEvent::Video(f) = ev {
                // Direct-to-wire streams start mid-GOP; count from the first keyframe.
                if aus == 0 && !f.keyframe {
                    continue;
                }
                aus += 1;
                let d = dec.get_or_insert_with(|| VideoDecoder::new(f.codec, HwAccel::D3D11Va, None).unwrap());
                d.decode(&f.data, f.time_ms, &mut frames).unwrap();
            }
        }
        let Some(mut d) = dec else { continue };
        d.flush(&mut frames).unwrap();
        let (w, h) = frames.last().map(|f| f.size()).unwrap_or_default();
        eprintln!("{}: {aus} AUs -> {} frames {w}x{h} via {}", p.file_name().unwrap().to_string_lossy(), frames.len(), d.description());
        assert_eq!(frames.len(), aus, "lost frames");
    }
}

/// Decodes the audio of every real-server capture (μ-law or FLAC) in temp/captures. Skipped if none.
#[test]
fn real_captures_audio() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../temp/captures");
    let Ok(rd) = std::fs::read_dir(&dir) else { return eprintln!("skipping: no captures") };
    bi_media::quiet_ffmpeg_logs();
    for p in rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bistream")) {
        let data = std::fs::read(&p).unwrap();
        let mut parser = StreamParser::default();
        parser.push(&data);
        let (mut flac_fmt, mut blocks, mut samples, mut rate) = (false, 0, 0usize, 0u32);
        let mut flac = bi_media::flac::FlacDecoder::new();
        let mut pcm = Vec::new();
        while let Some(ev) = parser.next_event().unwrap() {
            match ev {
                StreamEvent::Header { audio: Some(w), .. } => {
                    flac_fmt = w.is_flac();
                    rate = w.samples_per_sec;
                    if flac_fmt && w.raw.len() >= 18 + 34 {
                        flac.set_streaminfo(&w.raw[18..]).unwrap();
                    }
                }
                StreamEvent::Audio(a) => {
                    blocks += 1;
                    if flac_fmt {
                        pcm.clear();
                        rate = flac.decode_block(&a.data, &mut pcm).unwrap();
                        samples += pcm.len();
                    } else {
                        samples += a.data.len();
                    }
                }
                _ => {}
            }
        }
        if blocks == 0 {
            continue;
        }
        eprintln!("{}: {blocks} audio blocks ({}) -> {samples} samples @ {rate} Hz", p.file_name().unwrap().to_string_lossy(), if flac_fmt { "FLAC" } else { "μ-law" });
        assert!(samples > 0);
    }
}

/// Zero-copy over every real capture: all must stay on the GPU (no fallback).
#[test]
fn real_captures_zero_copy() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../temp/captures");
    let Ok(rd) = std::fs::read_dir(&dir) else { return eprintln!("skipping: no captures") };
    bi_media::quiet_ffmpeg_logs();
    let luid = unsafe {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let f: IDXGIFactory1 = CreateDXGIFactory1().unwrap();
        f.EnumAdapters1(0).unwrap().GetDesc1().unwrap().AdapterLuid
    };
    let dev = bi_media::d3d::SharedD3D11::new(luid).unwrap();
    for p in rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bistream")) {
        let data = std::fs::read(&p).unwrap();
        let mut parser = StreamParser::default();
        parser.push(&data);
        let (mut dec, mut frames) = (None::<VideoDecoder>, Vec::new());
        while let Some(ev) = parser.next_event().unwrap() {
            if let StreamEvent::Video(f) = ev {
                let d = dec.get_or_insert_with(|| VideoDecoder::new_with_device(f.codec, HwAccel::D3D11Va, None, Some(dev.clone())).unwrap());
                d.decode(&f.data, f.time_ms, &mut frames).unwrap();
                if frames.len() > 3 {
                    frames.drain(..frames.len() - 3);
                }
            }
        }
        let Some(d) = dec else { continue };
        eprintln!("{}: {} {:?}", p.file_name().unwrap().to_string_lossy(), d.description(), frames.last().map(|f| f.size()));
    }
}

/// Like the client-side grid: several decoders on one shared device, decoding concurrently. None may fall back.
#[test]
fn real_captures_zero_copy_concurrent() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../temp/captures");
    let Ok(rd) = std::fs::read_dir(&dir) else { return eprintln!("skipping: no captures") };
    bi_media::quiet_ffmpeg_logs();
    let luid = unsafe {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let f: IDXGIFactory1 = CreateDXGIFactory1().unwrap();
        f.EnumAdapters1(0).unwrap().GetDesc1().unwrap().AdapterLuid
    };
    let dev = bi_media::d3d::SharedD3D11::new(luid).unwrap();
    let files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bistream")).collect();
    let threads: Vec<_> = files
        .into_iter()
        .flat_map(|p| [p.clone(), p])
        .map(|p| {
            let dev = dev.clone();
            std::thread::spawn(move || {
                let data = std::fs::read(&p).unwrap();
                let mut parser = StreamParser::default();
                parser.push(&data);
                let (mut dec, mut frames) = (None::<VideoDecoder>, Vec::new());
                while let Some(ev) = parser.next_event().unwrap() {
                    if let StreamEvent::Video(f) = ev {
                        let d = dec.get_or_insert_with(|| VideoDecoder::new_with_device(f.codec, HwAccel::D3D11Va, None, Some(dev.clone())).unwrap());
                        d.decode(&f.data, f.time_ms, &mut frames).unwrap();
                        if frames.len() > 3 {
                            frames.drain(..frames.len() - 3);
                        }
                    }
                }
                dec.map(|d| d.description()).unwrap_or_default()
            })
        })
        .collect();
    for t in threads {
        let desc = t.join().unwrap();
        assert!(desc.is_empty() || desc.contains("zero-copy"), "fell back: {desc}");
    }
}
