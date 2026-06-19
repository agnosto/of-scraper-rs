use std::{fs, path::Path, sync::Arc, time::SystemTime, iter::from_fn, process};
use of_client::{OFClient, media::{Feed, DRM, Media}, widevine::Cdm};
use anyhow::{anyhow, bail};
use reqwest::Url;
use tokio::fs as tfs;
use filetime::{set_file_mtime, FileTime};
use httpdate::{fmt_http_date, parse_http_date};
use reqwest::header::{self, HeaderValue, IF_MODIFIED_SINCE};
use ffmpeg_sidecar::{command::FfmpegCommand, event::{FfmpegEvent, LogLevel}, log_parser::FfmpegLogParser};

pub struct Downloader {
    client: OFClient,
    device: Option<Cdm>,
    state: Option<Arc<std::sync::Mutex<crate::AppState>>>,
}

impl Downloader {
    pub fn new(client: OFClient, device: Option<Cdm>, state: Option<Arc<std::sync::Mutex<crate::AppState>>>) -> Self {
        Self { client, device, state }
    }

    pub async fn download_media(&self, media: &Feed, path: &Path) -> anyhow::Result<()> {
        if let Some(url_str) = media.source() {
            let url = Url::parse(url_str)?;
            let filename = url.path_segments()
                .and_then(|s| s.last())
                .ok_or_else(|| anyhow!("Filename unknown"))?
                .to_string();

            let target_path = path.join(&filename);
            self.fetch_file(media.id, url, &target_path, &filename).await?;
        }
        Ok(())
    }

    pub async fn download_media_drm(&self, media: &DRM, license_url: &str, path: &Path, media_id: u64) -> anyhow::Result<()> {
        if self.device.is_none() {
            bail!("DRM device not initialized");
        }

        let mpd_data = self.client.get_mpd_data(media).await?;
        let target_path = path.join(&mpd_data.base_url);

        if let Some(remote_modified) = mpd_data.last_modified {
            if let Ok(local_modified) = target_path.metadata().and_then(|m| m.modified()) {
                if local_modified >= remote_modified {
                    return Ok(());
                }
            }
        }

        if let Some(state) = &self.state {
            let mut s = state.lock().unwrap();
            s.active_downloads.insert(media_id, crate::ActiveDownload {
                filename: mpd_data.base_url.clone(),
                bytes_downloaded: 0,
                total_bytes: None,
            });
        }

        let res = self.handle_download(&target_path, mpd_data.last_modified, || async {
            let key = self.client
                .get_decryption_key(self.device.as_ref().unwrap(), license_url, mpd_data.pssh)
                .await?;
            
            let manifest = &media.manifest.dash;

            let mut command = FfmpegCommand::new();
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

        if let Some(state) = &self.state {
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

    async fn fetch_file(&self, media_id: u64, url: Url, path: &Path, filename: &str) -> anyhow::Result<()> {
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

        if let Some(state) = &self.state {
            let mut s = state.lock().unwrap();
            s.active_downloads.insert(media_id, crate::ActiveDownload {
                filename: filename.to_string(),
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
                
                if let Some(state) = &self.state {
                    if let Some(dl) = state.lock().unwrap().active_downloads.get_mut(&media_id) {
                        dl.bytes_downloaded = downloaded;
                    }
                }
            }

            fs::rename(&temp_path, path)?;
            Ok(())
        }).await;

        if let Some(state) = &self.state {
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
