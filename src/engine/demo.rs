//! Demo media generation.
//!
//! The demo session needs two synthetic clips that are visually and audibly
//! easy to align:
//!
//! * a **source** clip: `testsrc2` video plus a 1 kHz sine beep that starts at
//!   a known point, and
//! * a **reaction** clip: a flat colour flash 1500 ms later than the source's
//!   flash, which is exactly the offset the demo session encodes
//!   (`offset_us = +1_500_000`).
//!
//! `scripts/make-demo-clips.sh` produces byte-identical arguments from the
//! shell. The app runs the same `ffmpeg` command line itself when the button
//! is pressed and the clips are missing, so a user without the script still
//! gets a working demo. If `ffmpeg` is not on `PATH` the failure is reported
//! verbatim, never hidden behind a stub.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{AppError, AppResult};

/// File name of the demo source clip.
pub const DEMO_SOURCE_FILE: &str = "source-testsrc2.mp4";
/// File name of the demo reaction clip.
pub const DEMO_REACTION_FILE: &str = "reaction-flash.mp4";
/// Offset the demo session applies to the reaction layer, in microseconds.
///
/// Positive means "show the media later": the reaction flash happens 1500 ms
/// after the source flash, so shifting the layer forward by 1500 ms makes the
/// two flashes line up.
pub const DEMO_REACTION_OFFSET_US: i64 = 1_500_000;
/// Length of both clips, in seconds.
pub const DEMO_DURATION_S: u32 = 10;
/// Video frame rate of both clips.
pub const DEMO_FPS: u32 = 30;
/// Where the reaction clip's colour flash starts, in milliseconds.
pub const DEMO_REACTION_FLASH_MS: u32 = 3_000;
/// Where the source clip's flash starts, in milliseconds (1500 ms earlier).
pub const DEMO_SOURCE_FLASH_MS: u32 = DEMO_REACTION_FLASH_MS - 1_500;
/// Beep frequency used by both clips.
pub const DEMO_BEEP_HZ: u32 = 1_000;

/// A resolved pair of demo clips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoMedia {
    /// Absolute path of the source clip.
    pub source: PathBuf,
    /// Absolute path of the reaction clip.
    pub reaction: PathBuf,
}

impl DemoMedia {
    /// Paths inside a data directory, whether or not the files exist.
    pub fn paths_in(dir: &Path) -> Self {
        Self {
            source: dir.join(DEMO_SOURCE_FILE),
            reaction: dir.join(DEMO_REACTION_FILE),
        }
    }

    /// True when both files are present and non-empty.
    pub fn exists(&self) -> bool {
        is_nonempty(&self.source) && is_nonempty(&self.reaction)
    }

    /// The duration of the clips, in microseconds.
    pub fn duration_us() -> i64 {
        DEMO_DURATION_S as i64 * 1_000_000
    }
}

fn is_nonempty(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// The exact `ffmpeg` arguments for the source clip.
///
/// Kept separate from [`ensure_demo_media`] so the shell script and the tests
/// can assert on the same list.
pub fn source_args(path: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!(
            "testsrc2=size=1280x720:rate={DEMO_FPS}:duration={DEMO_DURATION_S}"
        ),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!(
            "sine=frequency={DEMO_BEEP_HZ}:duration={DEMO_DURATION_S}:sample_rate=48000"
        ),
        // Flash a white frame for 200 ms at the known mark, so alignment can
        // be checked by eye as well as by the offset field.
        "-vf".into(),
        format!(
            "drawbox=x=0:y=0:w=iw:h=ih:color=white@1.0:t=fill:enable='between(t,{:.3},{:.3})',format=yuv420p",
            (DEMO_SOURCE_FLASH_MS as f32) / 1000.0,
            (DEMO_SOURCE_FLASH_MS as f32 + 200.0) / 1000.0
        ),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "20".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        "-shortest".into(),
        "-movflags".into(),
        "+faststart".into(),
        path.to_string_lossy().to_string(),
    ]
}

/// The exact `ffmpeg` arguments for the reaction clip.
pub fn reaction_args(path: &Path) -> Vec<String> {
    let flash_start = (DEMO_REACTION_FLASH_MS as f32) / 1000.0;
    let flash_end = (DEMO_REACTION_FLASH_MS as f32 + 200.0) / 1000.0;
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("color=c=0x101014:size=960x540:rate={DEMO_FPS}:duration={DEMO_DURATION_S}"),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!(
            "sine=frequency={DEMO_BEEP_HZ}:duration={DEMO_DURATION_S}:sample_rate=48000"
        ),
        // Accent-coloured flash exactly 1500 ms after the source's flash.
        "-vf".into(),
        format!(
            "drawbox=x=0:y=0:w=iw:h=ih:color=0xFACC15@1.0:t=fill:enable='between(t,{flash_start:.3},{flash_end:.3})',format=yuv420p"
        ),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "20".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        "-shortest".into(),
        "-movflags".into(),
        "+faststart".into(),
        path.to_string_lossy().to_string(),
    ]
}

/// True when `ffmpeg` can be executed from `PATH`.
pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create both demo clips in `dir` if they are missing.
///
/// Returns the resolved paths. Errors are actionable: they name `ffmpeg`, say
/// how to install it, and fall back to describing the shell script.
pub fn ensure_demo_media(dir: &Path) -> AppResult<DemoMedia> {
    let media = DemoMedia::paths_in(dir);
    if media.exists() {
        return Ok(media);
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::new(format!("Could not create {}: {e}", dir.display())))?;

    if !ffmpeg_available() {
        return Err(AppError::new(format!(
            "ffmpeg is required to generate the demo clips but was not found on PATH. \
             Install it (Arch: `sudo pacman -S ffmpeg`), or run \
             `scripts/make-demo-clips.sh` yourself and place the files in {}.",
            dir.display()
        )));
    }

    if !is_nonempty(&media.source) {
        run_ffmpeg(&source_args(&media.source), "source clip")?;
    }
    if !is_nonempty(&media.reaction) {
        run_ffmpeg(&reaction_args(&media.reaction), "reaction clip")?;
    }
    Ok(media)
}

fn run_ffmpeg(args: &[String], what: &str) -> AppResult<()> {
    let output = Command::new("ffmpeg").args(args).output().map_err(|e| {
        AppError::new(format!("Could not run ffmpeg for the demo {what}: {e}"))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(AppError::new(format!(
            "ffmpeg failed while generating the demo {what} (exit {:?}):\n{tail}",
            output.status.code()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_matches_the_flash_difference() {
        // The demo relies on these two numbers being 1500 ms apart; if one
        // changes without the other, the offsets stop describing the media.
        assert_eq!(
            DEMO_REACTION_FLASH_MS as i64 - DEMO_SOURCE_FLASH_MS as i64,
            DEMO_REACTION_OFFSET_US / 1000
        );
        assert_eq!(DEMO_REACTION_OFFSET_US, 1_500_000);
    }

    #[test]
    fn paths_live_in_the_given_dir() {
        let media = DemoMedia::paths_in(Path::new("/tmp/demo"));
        assert_eq!(media.source, PathBuf::from("/tmp/demo/source-testsrc2.mp4"));
        assert_eq!(media.reaction, PathBuf::from("/tmp/demo/reaction-flash.mp4"));
        assert!(!media.exists());
    }

    #[test]
    fn duration_is_ten_seconds() {
        assert_eq!(DemoMedia::duration_us(), 10_000_000);
    }

    #[test]
    fn source_args_describe_a_playable_clip() {
        let args = source_args(Path::new("/tmp/source.mp4"));
        let joined = args.join(" ");
        assert!(joined.contains("testsrc2=size=1280x720"));
        assert!(joined.contains("sine=frequency=1000"));
        assert!(joined.contains("between(t,1.500,1.700)"));
        assert_eq!(args.last().unwrap(), "/tmp/source.mp4");
        assert!(joined.contains("-pix_fmt yuv420p"));
    }

    #[test]
    fn reaction_args_flash_later() {
        let args = reaction_args(Path::new("/tmp/reaction.mp4"));
        let joined = args.join(" ");
        assert!(joined.contains("color=c=0x101014"));
        assert!(joined.contains("between(t,3.000,3.200)"));
        assert!(joined.contains("0xFACC15"));
        assert_eq!(args.last().unwrap(), "/tmp/reaction.mp4");
    }

    #[test]
    fn demo_clips_are_not_reported_when_absent() {
        let dir = std::env::temp_dir().join("reaction-player-demo-test-absent");
        let _ = std::fs::remove_dir_all(&dir);
        let media = DemoMedia::paths_in(&dir);
        assert!(!media.exists());
    }

    #[test]
    fn existing_files_short_circuit_generation() {
        let dir = std::env::temp_dir().join("reaction-player-demo-test-present");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let media = DemoMedia::paths_in(&dir);
        std::fs::write(&media.source, b"x").unwrap();
        std::fs::write(&media.reaction, b"y").unwrap();
        let resolved = ensure_demo_media(&dir).unwrap();
        assert_eq!(resolved, media);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_ffmpeg_produces_an_actionable_error() {
        if ffmpeg_available() {
            // On machines that do have ffmpeg we cannot exercise this path.
            return;
        }
        let dir = std::env::temp_dir().join("reaction-player-demo-test-noffmpeg");
        let _ = std::fs::remove_dir_all(&dir);
        let err = ensure_demo_media(&dir).unwrap_err().to_string();
        assert!(err.contains("ffmpeg"));
        assert!(err.contains("make-demo-clips.sh"));
    }
}
