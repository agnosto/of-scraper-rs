use std::{fs, path::{Path, PathBuf}, sync::Arc, time::{Duration, SystemTime}, iter::from_fn, process};
use of_client::{OFClient, media::{Feed, DRM, Media}, widevine::Cdm};
use anyhow::{anyhow, bail};
use reqwest::Url;
use tokio::fs as tfs;
use filetime::{set_file_mtime, FileTime};
use httpdate::{fmt_http_date, parse_http_date};
use reqwest::header::{self, HeaderValue, IF_MODIFIED_SINCE};
use ffmpeg_sidecar::{command::FfmpegCommand, event::{FfmpegEvent, LogLevel}, log_parser::FfmpegLogParser};

use crate::tui::app::{ActiveDownload, ScrapeState};

pub struct Downloader {
    client: OFClient,
    device: Option<Cdm>,
    ffmpeg_path: Option<PathBuf>,
}

impl Downloader {
    pub fn new(client: OFClient, device: Option<Cdm>) -> Self {
        Self::new_with_ffmpeg(client, device, None)
    }

    pub fn new_with_ffmpeg(client: OFClient, device: Option<Cdm>, ffmpeg_path: Option<PathBuf>) -> Self {
        Self { client, device, ffmpeg_path }
    }

    pub async fn download_media(&self, media: &Feed, path: &Path) -> anyhow::Result<()> {
        if let Some(url_str) = media.source() {
            let url = Url::parse(url_str)?;
            let filename = url.path_segments()
                .and_then(|mut s| s.next_back())
                .ok_or_else(|| anyhow!("Filename unknown"))?
                .to_string();

            let target_path = path.join(&filename);
            self.fetch_file(media.id, url, &target_path, &filename, None, String::new()).await?;
        }
        Ok(())
    }

    /// Same as `download_media` but also reports progress into a
    /// per-scrape `ScrapeState`, if one was supplied for this run.
    /// `filename` is the exact name to save as (custom filename formats
    /// are resolved by the caller).
    pub async fn download_media_tracked(
        &self,
        media: &Feed,
        path: &Path,
        filename: &str,
        state: Option<Arc<std::sync::Mutex<ScrapeState>>>,
        creator: String,
    ) -> anyhow::Result<()> {
        if let Some(url_str) = media.source() {
            let url = Url::parse(url_str)?;
            let target_path = path.join(filename);
            self.fetch_file(media.id, url, &target_path, filename, state, creator).await?;
        }
        Ok(())
    }

    pub async fn download_media_drm(&self, media: &DRM, license_url: &str, path: &Path, media_id: u64, filename: &str) -> anyhow::Result<()> {
        self.download_media_drm_tracked(media, license_url, path, media_id, filename, None, String::new()).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn download_media_drm_tracked(
        &self,
        media: &DRM,
        license_url: &str,
        path: &Path,
        media_id: u64,
        filename: &str,
        state: Option<Arc<std::sync::Mutex<ScrapeState>>>,
        creator: String,
    ) -> anyhow::Result<()> {
        if self.device.is_none() {
            bail!("DRM device not initialized");
        }

        let mpd_data = self.client.get_mpd_data(media).await?;
        let target_path = path.join(filename);

        if let Some(remote_modified) = mpd_data.last_modified
            && let Ok(local_modified) = target_path.metadata().and_then(|m| m.modified())
            && local_modified >= remote_modified
        {
            return Ok(());
        }

        if let Some(state) = &state {
            let mut s = state.lock().unwrap();
            s.active_downloads.insert(media_id, ActiveDownload {
                filename: filename.to_string(),
                creator,
                bytes_downloaded: 0,
                total_bytes: None,
            });
        }

        let res = self.handle_download(&target_path, mpd_data.last_modified, || async {
            let key = self.client
                .get_decryption_key(self.device.as_ref().unwrap(), license_url, mpd_data.pssh)
                .await?;

            let manifest = &media.manifest.dash;

            let mut command = if let Some(path) = &self.ffmpeg_path {
                FfmpegCommand::new_with_path(path)
            } else {
                FfmpegCommand::new()
            };
            command
                .hide_banner()
                .args(["-cenc_decryption_key", &base16::encode_lower(&key.key)])
                .args(["-headers", &self.client.mpd_header(manifest)])
                .overwrite()
                .input(manifest)
                .args(["-c", "copy"])
                .as_inner_mut()
                .arg(&target_path);

            let output = tokio::process::Command::from(process::Command::from(command))
                .spawn()?
                .wait_with_output()
                .await?;

            let mut log_parser = FfmpegLogParser::new(output.stderr.as_slice());
            let first_error = from_fn(|| match log_parser.parse_next_event() {
                Ok(entry) if !matches!(entry, FfmpegEvent::LogEOF) => Some(entry),
                _ => None,
            })
            .find(|entry| matches!(entry, FfmpegEvent::Log(LogLevel::Error, _)));

            if let Some(FfmpegEvent::Log(_, error)) = first_error {
                bail!(error)
            }

            Ok(())
        }).await;

        if let Some(state) = &state {
            let mut s = state.lock().unwrap();
            s.active_downloads.remove(&media_id);
            if res.is_ok() {
                s.files_downloaded += 1;
            } else {
                s.files_failed += 1;
            }
        }

        res
    }

    async fn fetch_file(
        &self,
        media_id: u64,
        url: Url,
        path: &Path,
        filename: &str,
        state: Option<Arc<std::sync::Mutex<ScrapeState>>>,
        creator: String,
    ) -> anyhow::Result<()> {
        let response = match path.metadata().and_then(|m| m.modified()) {
            Ok(date) => {
                let res = self.client.get(url.clone())
                    .header(IF_MODIFIED_SINCE, HeaderValue::from_str(&fmt_http_date(date)).unwrap())
                    .send()
                    .await?;

                if res.status() == reqwest::StatusCode::NOT_MODIFIED { return Ok(()) }
                res
            },
            Err(_) => self.client.get(url).send().await?
        };

        let modified = response
            .headers()
            .get(header::LAST_MODIFIED)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| parse_http_date(s).ok());

        let total_bytes = response.content_length();

        let rate_bps: Option<f64> = {
            let perf = &crate::config::config().performance;
            if perf.limit_download_rate && perf.download_limit_mb_per_sec > 0.0 {
                Some(perf.download_limit_mb_per_sec * 1024.0 * 1024.0)
            } else {
                None
            }
        };

        if let Some(state) = &state {
            let mut s = state.lock().unwrap();
            s.active_downloads.insert(media_id, ActiveDownload {
                filename: filename.to_string(),
                creator,
                bytes_downloaded: 0,
                total_bytes,
            });
        }

        let res = self.handle_download(path, modified, || async {
            let temp_path = path.with_extension("temp");
            let mut file = tfs::File::from_std(fs::File::create(&temp_path)?);

            use tokio::io::AsyncWriteExt;
            use futures::StreamExt;
            let mut downloaded = 0u64;
            let mut stream = response.bytes_stream();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result?;
                file.write_all(&chunk).await?;
                downloaded += chunk.len() as u64;

                if let Some(bps) = rate_bps {
                    // Throttle to `DownloadLimitInMbPerSec` by sleeping for
                    // the time it should have taken to receive this chunk.
                    tokio::time::sleep(Duration::from_secs_f64(chunk.len() as f64 / bps)).await;
                }

                if let Some(state) = &state
                    && let Some(dl) = state.lock().unwrap().active_downloads.get_mut(&media_id)
                {
                    dl.bytes_downloaded = downloaded;
                }
            }

            fs::rename(&temp_path, path)?;
            Ok(())
        }).await;

        if let Some(state) = &state {
            let mut s = state.lock().unwrap();
            s.active_downloads.remove(&media_id);
            if res.is_ok() {
                s.files_downloaded += 1;
            } else {
                s.files_failed += 1;
            }
        }

        res
    }

    async fn handle_download<F, Fut>(&self, path: &Path, modified: Option<SystemTime>, fetch_fn: F) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fetch_fn().await?;

        if let Some(date) = modified {
            let _ = set_file_mtime(path, FileTime::from_system_time(date));
        }

        Ok(())
    }
}
