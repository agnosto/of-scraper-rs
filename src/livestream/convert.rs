//! Converts a finished recording to the configured container with FFmpeg.
//! Streams are copied whenever the target container can hold them; otherwise
//! the video is encoded as H.264 and the audio as AAC. The original is removed
//! only after a successful conversion, so a failure always leaves the
//! recording as it was written. Ported from SubScraper's `LiveRecordingConverter`.

use std::path::{Path, PathBuf};

use log::{info, warn};

use super::OutputFormat;

/// The file a recording ended up in, and whether converting it failed.
#[derive(Debug)]
pub struct Conversion {
	pub path: PathBuf,
	pub converted: bool,
	pub error: Option<String>,
}

pub async fn convert(source_path: &Path, format: OutputFormat, ffmpeg_path: Option<&Path>) -> Conversion {
	let source_extension = source_path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase());
	let target_extension = match format {
		OutputFormat::Mkv if source_extension.as_deref() != Some("mkv") => Some(".mkv"),
		OutputFormat::Mp4 => Some(".mp4"),
		_ => None,
	};
	let Some(target_extension) = target_extension else {
		return Conversion { path: source_path.to_path_buf(), converted: false, error: None };
	};

	let target_path = unique_path(&source_path.with_extension(target_extension.trim_start_matches('.')));
	let mut arguments = vec![
		"-hide_banner".to_string(),
		"-loglevel".to_string(),
		"error".to_string(),
		"-nostdin".to_string(),
		"-y".to_string(),
		"-i".to_string(),
		source_path.to_string_lossy().to_string(),
		"-map".to_string(),
		"0".to_string(),
	];
	if target_extension == ".mkv" {
		arguments.extend(["-c".to_string(), "copy".to_string()]);
	} else {
		// WebM recordings carry VP8, which MP4 players do not support; Matroska ones carry H.264.
		match source_extension.as_deref() {
			Some("mkv") => arguments.extend(["-c:v".to_string(), "copy".to_string()]),
			_ => arguments.extend([
				"-c:v".to_string(),
				"libx264".to_string(),
				"-preset".to_string(),
				"veryfast".to_string(),
				"-crf".to_string(),
				"20".to_string(),
				"-pix_fmt".to_string(),
				"yuv420p".to_string(),
			]),
		}
		arguments.extend(["-c:a".to_string(), "aac".to_string(), "-b:a".to_string(), "192k".to_string(), "-movflags".to_string(), "+faststart".to_string()]);
	}
	arguments.push(target_path.to_string_lossy().to_string());

	let ffmpeg = ffmpeg_path
		.map(|p| p.to_path_buf())
		.filter(|p| p.exists())
		.unwrap_or_else(|| PathBuf::from("ffmpeg"));

	let result = tokio::process::Command::new(&ffmpeg)
		.args(&arguments)
		.output()
		.await;
	match result {
		Ok(output) if output.status.success() && target_path.exists() && target_path.metadata().map(|m| m.len() > 0).unwrap_or(false) => {
			let _ = std::fs::remove_file(source_path);
			info!("Converted livestream recording {} to {}", source_path.display(), target_path.display());
			Conversion { path: target_path, converted: true, error: None }
		}
		Ok(output) => {
			let stderr = String::from_utf8_lossy(&output.stderr);
			let last_line = stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("no output");
			let error = format!("FFmpeg exited with code {}: {last_line}", output.status.code().unwrap_or(-1));
			try_delete(&target_path);
			warn!("Keeping livestream recording {} as recorded: {error}", source_path.display());
			Conversion { path: source_path.to_path_buf(), converted: false, error: Some(error) }
		}
		Err(e) => {
			let error = format!("FFmpeg could not be run ({e}). Set the FFmpeg path in the configuration.");
			try_delete(&target_path);
			warn!("Keeping livestream recording {} as recorded: {error}", source_path.display());
			Conversion { path: source_path.to_path_buf(), converted: false, error: Some(error) }
		}
	}
}

fn unique_path(path: &Path) -> PathBuf {
	let mut candidate = path.to_path_buf();
	let mut suffix = 2;
	while candidate.exists() {
		let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("recording");
		let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
		candidate = path.with_file_name(format!("{stem}_{suffix}.{ext}"));
		suffix += 1;
	}
	candidate
}

fn try_delete(path: &Path) {
	match std::fs::remove_file(path) {
		Err(e) if path.exists() => {
			log::debug!("Could not remove the incomplete conversion {}: {e}", path.display());
		}
		_ => {}
	}
}
