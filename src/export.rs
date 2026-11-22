//! Writing captured swells to WAV files and showing them in the file browser.

use std::path::{Path, PathBuf};

/// `~/Music/Reverse Swell` (created if needed).
pub fn output_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join("Music").join("Reverse Swell")
}

/// Label for the swell length used in file names, e.g. "1-2 bar" or "800 ms".
pub fn length_label(note_index: i32, len_samples: usize, sample_rate: f32) -> String {
    match note_index {
        0 => "1-16".into(),
        1 => "1-8".into(),
        2 => "1-4".into(),
        3 => "1-2".into(),
        4 => "1 bar".into(),
        5 => "2 bars".into(),
        _ => format!("{:.0} ms", len_samples as f32 * 1000.0 / sample_rate),
    }
}

/// Writes a stereo 32-bit float WAV with a fresh numbered name and returns its path.
pub fn write_swell(
    left: &[f32],
    right: &[f32],
    sample_rate: f32,
    label: &str,
    bpm: f32,
) -> Result<PathBuf, String> {
    let dir = output_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;

    let mut path = dir.join("Swell.wav");
    for n in 1..10_000 {
        path = dir.join(format!("Swell {n:03} - {label} @ {bpm:.0} BPM.wav"));
        if !path.exists() {
            break;
        }
    }

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sample_rate.round() as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer =
        hound::WavWriter::create(&path, spec).map_err(|e| format!("couldn't write file: {e}"))?;
    for (l, r) in left.iter().zip(right.iter()) {
        writer.write_sample(*l).map_err(|e| e.to_string())?;
        writer.write_sample(*r).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(path)
}

/// Shows the file in Finder / Explorer / the file manager.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Starts an OS-level file drag. Only implemented on macOS.
pub fn start_drag(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::drag_mac::start_file_drag(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("dragging out is only available on Mac".to_string())
    }
}
