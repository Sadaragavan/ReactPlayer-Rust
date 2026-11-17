//! Demo clips, generated on the machine that runs the app.
//!
//! The demo exists so the player can be tried without hunting for footage. Two
//! synthetic clips are generated with ffmpeg:
//!
//! * `source.mp4` - `testsrc2` at 30 fps with a white flash at the very start,
//! * `reaction.mp4` - a flat background with a white flash 1500 ms later.
//!
//! The flash is a precise, easy-to-see event, which is exactly what the
//! Alignment Studio needs: aligning the demo puts the reaction offset at
//! `+1_500_000` us. `scripts/make-demo-clips.sh` runs the same commands so the
//! clips can be built ahead of time, and the app runs them from Rust when the
//! button is pressed.
//!
//! Nothing here needs network access: ffmpeg's `lavfi` sources are built in.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::geom::NormRect;
use crate::model::presets::{layout_rects, layout_z, LayoutType};
use crate::model::session::{MediaSource, Role, Session};

/// Where the demo clips live under the app's data directory.
pub const DEMO_DIR: &str = "demo";

/// The flash in `reaction.mp4`, in microseconds. This is also the offset the
/// demo session ships with.
pub const DEMO_FLASH_US: i64 = 1_500_000;

/// Length of both clips.
pub const DEMO_DURATION_US: i64 = 8_000_000;

/// The two files the demo needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DemoClips {
    pub source: PathBuf,
    pub reaction: PathBuf,
}

impl DemoClips {
    pub fn dir(&self) -> Option<&Path> {
        self.source.parent()
    }

    pub fn exist(&self) -> bool {
        is_clip(&self.source) && is_clip(&self.reaction)
    }
}

/// Paths of the demo clips for a data directory, without touching the disk.
pub fn clips(data_dir: &Path) -> DemoClips {
    DemoClips {
        source: data_dir.join(DEMO_DIR).join("source.mp4"),
        reaction: data_dir.join(DEMO_DIR).join("reaction.mp4"),
    }
}

fn is_clip(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

/// True when both clips are already on disk.
/// The program the demo shells out to. One name, in one place: the home screen
/// checks for it, and `ensure` runs it.
pub const FFMPEG: &str = "ffmpeg";

pub fn is_ready(data_dir: &Path) -> bool {
    clips(data_dir).exist()
}

/// Is there an `ffmpeg` on `PATH`?
///
/// The demo is the only feature that shells out, and it is the first thing a
/// new offline install tries, so the home screen asks once at startup instead
/// of letting the click fail.
pub fn ffmpeg_on_path() -> bool {
    Command::new(FFMPEG)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Make sure both clips exist, generating them with ffmpeg if needed.
pub fn ensure(data_dir: &Path) -> Result<DemoClips, String> {
    let clips = clips(data_dir);
    if clips.exist() {
        return Ok(clips);
    }
    let dir = data_dir.join(DEMO_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    warn_if_ffmpeg_missing()?;
    run_ffmpeg(&source_args(&clips.source))?;
    run_ffmpeg(&reaction_args(&clips.reaction))?;
    if !clips.exist() {
        return Err("ffmpeg finished but the demo clips are missing".to_owned());
    }
    Ok(clips)
}

/// The ffmpeg arguments for the reference clip, shared by the app and the
/// script so the two can never drift apart.
pub fn source_args(output: &Path) -> Vec<String> {
    vec![
        "-y".into(),
        "-nostdin".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("testsrc2=size=1280x720:rate=30:duration={}", DEMO_DURATION_US / 1_000_000),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "sine=frequency=440:sample_rate=48000:duration=8".into(),
        "-vf".into(),
        // A white flash on the first 200 ms: the alignment landmark.
        "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='lt(t,0.2)'".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        "-shortest".into(),
        "-movflags".into(),
        "+faststart".into(),
        output.display().to_string(),
    ]
}

/// The ffmpeg arguments for the reaction clip: same length, flash 1500 ms in.
pub fn reaction_args(output: &Path) -> Vec<String> {
    vec![
        "-y".into(),
        "-nostdin".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("color=c=0x0A0A0A:size=1280x720:rate=30:duration={}", DEMO_DURATION_US / 1_000_000),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "sine=frequency=880:sample_rate=48000:duration=8".into(),
        "-vf".into(),
        format!(
            "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='between(t,{}, {})'",
            DEMO_FLASH_US as f64 / 1_000_000.0,
            DEMO_FLASH_US as f64 / 1_000_000.0 + 0.2
        ),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        "-shortest".into(),
        "-movflags".into(),
        "+faststart".into(),
        output.display().to_string(),
    ]
}

fn warn_if_ffmpeg_missing() -> Result<(), String> {
    match Command::new(FFMPEG).arg("-version").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) => Err(
            "ffmpeg is installed but would not run; install the `ffmpeg` package and try again"
                .to_owned(),
        ),
        Err(error) => Err(format!(
            "the demo needs ffmpeg on PATH to generate its clips ({error}). \
             Install it with `sudo pacman -S ffmpeg` on Arch, `brew install ffmpeg` on macOS, \
             or `choco install ffmpeg` on Windows, then press Demo again."
        )),
    }
}

fn run_ffmpeg(args: &[String]) -> Result<(), String> {
    let output = Command::new(FFMPEG)
        .args(args)
        .output()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: String = stderr
        .lines()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ");
    Err(format!("ffmpeg failed: {tail}"))
}

/// Build the demo session: the source full-frame underneath, the reaction as a
/// picture-in-picture inset on top, pre-offset by the flash.
pub fn build_session(clips: &DemoClips) -> Session {
    let mut session = Session::new("Demo - flash alignment");
    // The reaction sits in the bottom-right corner, on top of the source, which
    // is the arrangement the demo is meant to show off.
    let rects = layout_rects(LayoutType::PipReactionOverSource, 2);
    let z = layout_z(LayoutType::PipReactionOverSource, 2);
    let mut source = crate::model::session::Layer::new(
        "Source",
        Role::Source,
        MediaSource::file(clips.source.clone()),
        rects.first().copied().unwrap_or(NormRect::FULL),
        z.first().copied().unwrap_or(0),
    );
    source.duration_us = DEMO_DURATION_US;
    source.fps_num = 30;
    source.fps_den = 1;
    source.has_audio = true;
    source.width = 1280;
    source.height = 720;
    source.audio_rate = 48_000;
    source.audio_channels = 2;

    let mut reaction = crate::model::session::Layer::new(
        "Reaction 1",
        Role::Reaction,
        MediaSource::file(clips.reaction.clone()),
        rects.get(1).copied().unwrap_or(NormRect::FULL),
        z.get(1).copied().unwrap_or(1),
    );
    reaction.duration_us = DEMO_DURATION_US;
    reaction.fps_num = 30;
    reaction.fps_den = 1;
    reaction.has_audio = true;
    reaction.width = 1280;
    reaction.height = 720;
    reaction.audio_rate = 48_000;
    reaction.audio_channels = 2;
    // The flash is 1500 ms later in the reaction clip; the offset lines the two
    // flashes up on the union timeline.
    reaction.offset_us = DEMO_FLASH_US;

    session.master_audio = Some(source.id.clone());
    session.layers.push(source);
    session.layers.push(reaction);
    session.normalize_z();
    session
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_under_the_demo_directory() {
        let clips = clips(Path::new("/tmp/rp"));
        assert_eq!(clips.source, Path::new("/tmp/rp/demo/source.mp4"));
        assert_eq!(clips.reaction, Path::new("/tmp/rp/demo/reaction.mp4"));
        assert_eq!(clips.dir(), Some(Path::new("/tmp/rp/demo")));
        assert!(!clips.exist());
    }

    #[test]
    fn ffmpeg_commands_carry_the_landmark() {
        let source = source_args(Path::new("/tmp/source.mp4"));
        assert!(source.iter().any(|arg| arg.contains("testsrc2")));
        assert!(source
            .iter()
            .any(|arg| arg.contains("enable='lt(t,0.2)'")));
        assert_eq!(source.last().map(String::as_str), Some("/tmp/source.mp4"));

        let reaction = reaction_args(Path::new("/tmp/reaction.mp4"));
        assert!(reaction.iter().any(|arg| arg == "color=c=0x0A0A0A:size=1280x720:rate=30:duration=8"));
        assert!(reaction
            .iter()
            .any(|arg| arg.contains("between(t,1.5, 1.7)")));
    }

    #[test]
    fn demo_session_offsets_the_reaction_by_the_flash() {
        let clips = clips(Path::new("/tmp/rp"));
        let session = build_session(&clips);
        assert_eq!(session.layers.len(), 2);
        assert_eq!(session.layers[0].role, Role::Source);
        assert_eq!(session.layers[1].role, Role::Reaction);
        assert_eq!(session.layers[1].offset_us, DEMO_FLASH_US);
        assert_eq!(session.layers[0].offset_us, 0);
        // Union starts where the reaction's footage starts, i.e. -1.5 s.
        let union = session.union();
        assert_eq!(union.start_us, -DEMO_FLASH_US);
        assert_eq!(union.end_us, DEMO_DURATION_US);
        // The source is the master, so its sound carries the demo.
        assert_eq!(
            session.master_audio.as_deref(),
            Some(session.layers[0].id.as_str())
        );
        // The inset does not cover the whole canvas.
        assert!(session.layers[1].rect.w < 1.0);
        assert_eq!(session.z_of(&session.layers[1].id), Some(1));
    }

    #[test]
    fn missing_clips_are_not_ready() {
        assert!(!is_ready(Path::new("/definitely/not/here")));
    }

    #[test]
    fn a_zero_length_file_is_not_a_clip() {
        let dir = std::env::temp_dir().join("reaction-player-demo-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("source.mp4");
        std::fs::write(&path, b"").expect("write");
        assert!(!is_clip(&path));
        std::fs::write(&path, b"x").expect("write");
        assert!(is_clip(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
