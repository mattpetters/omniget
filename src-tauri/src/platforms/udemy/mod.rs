//! First-class Udemy downloader: real Widevine (CBCS) DRM decryption for
//! protected courses, plus direct MP4/HLS for non-DRM lectures. Registered
//! ahead of the generic yt-dlp fallback so Udemy lecture URLs route here.
//!
//! The DRM pipeline (N_m3u8DL-RE → PSSH → pywidevine CDM → mp4decrypt → ffmpeg)
//! lives in `omniget_core::core::udemy`; this module wires it to the platform
//! trait, cookies, tool resolution, and settings.

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::path::Path;
use tokio::sync::mpsc;

use crate::core::hls_downloader::{HlsDownloader, ProtectedHlsPolicy};
use crate::models::media::{DownloadOptions, DownloadResult, MediaInfo, MediaType, VideoQuality};
use crate::platforms::traits::PlatformDownloader;
use omniget_core::core::udemy::{self, api, drm};
use omniget_core::core::{cookie_parser, dependencies, direct_downloader, http_client};
use omniget_core::models::progress::ProgressUpdate;

pub struct UdemyDownloader;

impl Default for UdemyDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl UdemyDownloader {
    pub fn new() -> Self {
        Self
    }
}

/// Normalize an omniget quality string (`"720p"`, `"1080"`, `"best"`) to the
/// bare form the DRM/MP4 selectors expect.
fn normalize_quality(q: Option<&str>) -> String {
    let q = q.unwrap_or("best").trim();
    if q.eq_ignore_ascii_case("best") || q.eq_ignore_ascii_case("highest") || q.is_empty() {
        return "best".to_string();
    }
    q.trim_end_matches('p').trim_end_matches('P').to_string()
}

/// Resolve the python interpreter for the CDM helper.
async fn ensure_python() -> Result<std::path::PathBuf> {
    for cand in ["python3", "python"] {
        if let Some(p) = dependencies::find_tool(cand).await {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "python3 not found. Udemy DRM decryption needs python3 with pywidevine installed: `pip install pywidevine`"
    ))
}

impl UdemyDownloader {
    /// Resolve course + lecture metadata for a lecture URL.
    async fn fetch_detail(url: &str) -> Result<(api::UdemyClient, String, api::LectureDetail)> {
        let jar = cookie_parser::load_extension_cookies_for_url(url);
        let client = api::UdemyClient::from_lecture_url(url, jar)?;
        let slug = api::parse_slug(url);
        let lecture_id = api::parse_lecture_id(url)
            .ok_or_else(|| anyhow!("Could not parse lecture id from URL: {url}"))?;
        let course = api::resolve_course(&client, &slug).await.with_context(|| {
            "Failed to resolve Udemy course — sign in to Udemy in your browser and capture cookies via the omniget browser extension"
        })?;
        let detail = api::get_lecture_detail(&client, course.id, lecture_id)
            .await
            .context("Failed to fetch Udemy lecture detail")?;
        Ok((client, slug, detail))
    }
}

#[async_trait]
impl PlatformDownloader for UdemyDownloader {
    fn name(&self) -> &str {
        "udemy"
    }

    fn can_handle(&self, url: &str) -> bool {
        udemy::is_udemy_lecture_url(url)
    }

    async fn get_media_info(&self, url: &str) -> Result<MediaInfo> {
        let (_client, slug, detail) = Self::fetch_detail(url).await?;
        let title = if detail.title.is_empty() {
            slug
        } else {
            detail.title
        };

        // Carry the lecture URL through MediaInfo so download() can recover it.
        let quality = VideoQuality {
            label: if detail.is_drmed {
                "DRM".into()
            } else {
                "best".into()
            },
            width: 0,
            height: 0,
            url: url.to_string(),
            format: "udemy".into(),
        };

        Ok(MediaInfo {
            title,
            author: String::new(),
            platform: "udemy".into(),
            duration_seconds: None,
            thumbnail_url: None,
            available_qualities: vec![quality],
            media_type: MediaType::Video,
            file_size_bytes: None,
        })
    }

    async fn download(
        &self,
        info: &MediaInfo,
        opts: &DownloadOptions,
        progress: mpsc::Sender<ProgressUpdate>,
    ) -> Result<DownloadResult> {
        // Recover the lecture URL (page_url first, then the embedded quality).
        let lecture_url = opts
            .page_url
            .clone()
            .filter(|u| udemy::is_udemy_lecture_url(u))
            .or_else(|| info.available_qualities.first().map(|q| q.url.clone()))
            .filter(|u| udemy::is_udemy_lecture_url(u))
            .ok_or_else(|| anyhow!("Udemy lecture URL unavailable for download"))?;

        let _ = progress.send(ProgressUpdate::percent(-1.0)).await; // Preparing
        let (client, slug, detail) = Self::fetch_detail(&lecture_url).await?;

        let raw_name = if detail.title.is_empty() {
            &slug
        } else {
            &detail.title
        };
        let base_name = sanitize_filename::sanitize(raw_name);
        std::fs::create_dir_all(&opts.output_dir)?;
        let quality = normalize_quality(opts.quality.as_deref());

        if detail.is_drmed {
            return Self::download_drm_lecture(
                &client, &detail, &base_name, &quality, opts, &progress,
            )
            .await;
        }

        // --- Non-DRM: prefer a direct MP4, else fall back to plain HLS. ---
        if let Some((mp4_url, _label)) = api::select_best_mp4(&detail.media_sources, &quality) {
            let output = opts.output_dir.join(format!("{base_name}.mp4"));
            let jar = cookie_parser::load_extension_cookies_for_url(&mp4_url);
            let mut builder = http_client::apply_global_proxy(reqwest::Client::builder())
                .user_agent(udemy::UDEMY_UA);
            if let Some(j) = jar {
                builder = builder.cookie_provider(j);
            }
            let http = builder.build()?;
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::REFERER,
                format!("https://{}/", client.host()).parse()?,
            );
            let size = direct_downloader::download_direct_with_headers(
                &http,
                &mp4_url,
                &output,
                progress.clone(),
                Some(headers),
                Some(&opts.cancel_token),
            )
            .await?;
            let _ = progress.send(ProgressUpdate::percent(100.0)).await;
            return Ok(DownloadResult {
                file_path: output,
                file_size_bytes: size,
                duration_seconds: 0.0,
                torrent_id: None,
                protected_media: None,
                protection_sidecar_path: None,
            });
        }

        if let Some(hls_url) = api::select_hls(&detail.media_sources) {
            let output = opts.output_dir.join(format!("{base_name}.mp4"));
            let jar = cookie_parser::load_extension_cookies_for_url(&hls_url);
            let mut builder = http_client::apply_global_proxy(reqwest::Client::builder())
                .user_agent(udemy::UDEMY_UA);
            if let Some(j) = jar {
                builder = builder.cookie_provider(j);
            }
            let http = builder.build()?;
            let referer = format!("https://{}/", client.host());
            let result = HlsDownloader::with_client(http)
                .with_user_agent_override(Some(udemy::UDEMY_UA.to_string()))
                .with_protected_hls_policy(ProtectedHlsPolicy::Fail)
                .download(
                    &hls_url,
                    &output.to_string_lossy(),
                    &referer,
                    None,
                    opts.cancel_token.clone(),
                    20,
                    3,
                )
                .await?;
            let _ = progress.send(ProgressUpdate::percent(100.0)).await;
            return Ok(DownloadResult {
                file_path: result.path,
                file_size_bytes: result.file_size,
                duration_seconds: 0.0,
                torrent_id: None,
                protected_media: result.protected_media,
                protection_sidecar_path: result.protection_sidecar_path,
            });
        }

        bail!("No downloadable media source found for this Udemy lecture")
    }
}

impl UdemyDownloader {
    async fn download_drm_lecture(
        client: &api::UdemyClient,
        detail: &api::LectureDetail,
        base_name: &str,
        quality: &str,
        opts: &DownloadOptions,
        progress: &mpsc::Sender<ProgressUpdate>,
    ) -> Result<DownloadResult> {
        let hls_url = api::select_hls(&detail.media_sources)
            .ok_or_else(|| anyhow!("DRM Udemy lecture has no HLS source"))?;
        let token = detail.media_license_token.clone().ok_or_else(|| {
            anyhow!("DRM lecture has no media_license_token — re-capture Udemy cookies and retry")
        })?;

        // Resolve the toolchain (auto-downloads N_m3u8DL-RE / mp4decrypt).
        let n_m3u8dl = dependencies::ensure_n_m3u8dl_re()
            .await
            .context("N_m3u8DL-RE is required to download DRM-protected Udemy video")?;
        let mp4decrypt = dependencies::ensure_mp4decrypt()
            .await
            .context("mp4decrypt (Bento4) is required to decrypt DRM-protected Udemy video")?;
        let ffmpeg = dependencies::ensure_ffmpeg()
            .await
            .context("ffmpeg is required to mux decrypted Udemy video")?;
        let python = ensure_python().await?;
        let cdm_script = drm::materialize_cdm_script().await?;

        let tools = drm::DrmTools {
            n_m3u8dl,
            mp4decrypt,
            ffmpeg,
            python,
            cdm_script,
        };

        let wvd = opts.widevine_device_path.as_deref().map(Path::new);
        let license_url = client.license_url();

        let path = drm::download_drm(
            &hls_url,
            &token,
            &license_url,
            &opts.output_dir,
            base_name,
            quality,
            wvd,
            &tools,
            &opts.cancel_token,
            progress,
        )
        .await
        .context("Udemy Widevine decryption pipeline failed")?;

        let size = std::fs::metadata(&path)?.len();
        if size == 0 {
            bail!(
                "Decrypted Udemy lecture is empty (0 bytes): {}",
                path.display()
            );
        }
        Ok(DownloadResult {
            file_path: path,
            file_size_bytes: size,
            duration_seconds: 0.0,
            torrent_id: None,
            protected_media: None,
            protection_sidecar_path: None,
        })
    }
}
