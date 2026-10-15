//! Copies the FFmpeg shared libraries next to built executables (target/<profile>/ and target/<profile>/deps/)
//! so binaries, tests and examples run without touching PATH.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let Ok(ffmpeg) = std::env::var("FFMPEG_DIR") else { return };
    let bin = PathBuf::from(ffmpeg).join("bin");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/<profile>/build/<pkg>-<hash>/out
    let Some(profile_dir) = out.ancestors().nth(3) else { return };
    let Ok(entries) = std::fs::read_dir(&bin) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")) {
            for dir in [profile_dir.to_path_buf(), profile_dir.join("deps"), profile_dir.join("examples")] {
                let _ = std::fs::create_dir_all(&dir);
                let dst = dir.join(p.file_name().unwrap());
                let stale = std::fs::metadata(&dst).map(|m| m.len() != e.metadata().map(|m| m.len()).unwrap_or(0)).unwrap_or(true);
                if stale {
                    let _ = std::fs::copy(&p, &dst);
                }
            }
        }
    }
}
