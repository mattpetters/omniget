use anyhow::{anyhow, Context};
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::core::ytdlp;
use crate::models::media::{
    DownloadOptions, DownloadResult, MediaInfo, MediaType, VideoQuality as MediaVideoQuality,
};
use crate::models::progress::ProgressUpdate;
use crate::platforms::cookie_provider;
use crate::platforms::generic_ytdlp::GenericYtdlpDownloader;
use crate::platforms::traits::PlatformDownloader;

const PATREON_REFERER: &str = "https://www.patreon.com/";

/// Downloader for individual Patreon posts.
///
/// Patreon already has a mature yt-dlp extractor for its authenticated API,
/// direct attachments, native audio/video, inline media, and external embeds.
/// Keeping that extractor as the transport is important: it checks
/// `current_user_can_view` before exposing protected media URLs. This wrapper
/// adds strict URL matching, managed-cookie support through the shared yt-dlp
/// runtime, multi-attachment handling, and errors that tell the user how to
/// restore access.
pub struct PatreonDownloader {
    fallback: GenericYtdlpDownloader,
}

impl Default for PatreonDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl PatreonDownloader {
    pub fn new() -> Self {
        Self {
            fallback: GenericYtdlpDownloader::new(),
        }
    }

    pub fn parse_video_info(
        json: &serde_json::Value,
        source_url: &str,
    ) -> anyhow::Result<MediaInfo> {
        if let Some(entries) = json.get("entries").and_then(serde_json::Value::as_array) {
            let qualities: Vec<MediaVideoQuality> = entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry_has_downloadable_media(entry))
                .map(|(index, entry)| MediaVideoQuality {
                    label: format!("{}. {}", index + 1, entry_title(entry, index)),
                    width: 0,
                    height: entry
                        .get("height")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as u32,
                    // Deliberately retain the Patreon post URL. Downloads are
                    // re-extracted with --playlist-items so entitlement checks,
                    // expiring CDN URLs, cookies, and embed headers stay intact.
                    url: source_url.to_string(),
                    format: format!("patreon_entry:{}", index + 1),
                })
                .collect();

            if qualities.is_empty() {
                return Err(anyhow!(
                    "Patreon post is accessible, but it has no supported downloadable attachment or embedded video/audio"
                ));
            }

            let title = json
                .get("title")
                .and_then(serde_json::Value::as_str)
                .filter(|title| !title.trim().is_empty())
                .unwrap_or("Patreon post")
                .to_string();
            let author = json
                .get("uploader")
                .or_else(|| json.get("channel"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Patreon")
                .to_string();
            let file_size_bytes = entries
                .iter()
                .filter(|entry| entry_has_downloadable_media(entry))
                .try_fold(0_u64, |total, entry| {
                    total.checked_add(entry_file_size(entry)?)
                });

            return Ok(MediaInfo {
                title,
                author,
                platform: "patreon".to_string(),
                duration_seconds: None,
                thumbnail_url: json
                    .get("thumbnail")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                available_qualities: qualities,
                media_type: MediaType::Playlist,
                file_size_bytes,
            });
        }

        let mut info = GenericYtdlpDownloader::parse_video_info(json)?;
        info.platform = "patreon".to_string();
        info.media_type = media_type_from_json(json);
        info.file_size_bytes = entry_file_size(json);
        for quality in &mut info.available_qualities {
            // A resolved embed may point at Vimeo, YouTube, or a short-lived
            // Patreon CDN URL. Keep the stable post URL so download-time
            // extraction can refresh it with the selected Patreon account.
            quality.url = source_url.to_string();
            quality.format = "ytdlp".to_string();
        }
        Ok(info)
    }

    async fn download_playlist(
        &self,
        info: &MediaInfo,
        opts: &DownloadOptions,
        progress: mpsc::Sender<ProgressUpdate>,
    ) -> anyhow::Result<DownloadResult> {
        let source_url = info
            .available_qualities
            .first()
            .map(|entry| entry.url.as_str())
            .ok_or_else(|| anyhow!("Patreon post has no downloadable media"))?;
        let ytdlp_path = match opts.ytdlp_path.as_ref() {
            Some(path) => path.clone(),
            None => ytdlp::ensure_ytdlp()
                .await
                .context("Patreon requires yt-dlp")?,
        };
        let output_dir = opts
            .output_dir
            .join(sanitize_filename::sanitize(&info.title));
        tokio::fs::create_dir_all(&output_dir).await?;

        let total = info.available_qualities.len();
        let mut total_bytes = 0_u64;
        let mut success_count = 0_usize;
        let mut last_path = output_dir.clone();
        let mut failures = Vec::new();
        let mut last_protected_media = None;
        let mut last_sidecar = None;

        for (position, entry) in info.available_qualities.iter().enumerate() {
            if opts.cancel_token.is_cancelled() {
                return Err(anyhow!("Download cancelled"));
            }

            let entry_index = entry
                .format
                .strip_prefix("patreon_entry:")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(position + 1);
            let extra_flags = patreon_entry_flags(
                opts.custom_ytdlp_args.clone().unwrap_or_default(),
                entry_index,
            );

            let (entry_tx, mut entry_rx) = mpsc::channel::<ProgressUpdate>(16);
            let aggregate_tx = progress.clone();
            let entry_position = position;
            let forwarder = tokio::spawn(async move {
                while let Some(update) = entry_rx.recv().await {
                    if update.percent < 0.0 {
                        if entry_position == 0 {
                            let _ = aggregate_tx.send(update).await;
                        }
                        continue;
                    }
                    let overall = (entry_position as f64 / total as f64) * 100.0
                        + (update.percent.clamp(0.0, 100.0) / total as f64);
                    let _ = aggregate_tx
                        .send(ProgressUpdate::rich(
                            overall,
                            None,
                            None,
                            update.speed_bps,
                            update.eta_seconds,
                        ))
                        .await;
                }
            });

            let result = ytdlp::download_video(
                &ytdlp_path,
                source_url,
                &output_dir,
                extract_quality_height(opts.quality.as_deref()),
                entry_tx,
                opts.download_mode.as_deref(),
                opts.format_id.as_deref(),
                opts.filename_template.as_deref(),
                opts.referer.as_deref().or(Some(PATREON_REFERER)),
                opts.cancel_token.clone(),
                None,
                opts.concurrent_fragments,
                opts.download_subtitles,
                &extra_flags,
                opts.audio_format.as_deref(),
                opts.save_encrypted_hls,
            )
            .await;
            let _ = forwarder.await;

            match result {
                Ok(result) => {
                    success_count += 1;
                    total_bytes = total_bytes.saturating_add(result.file_size_bytes);
                    last_path = result.file_path;
                    last_protected_media = result.protected_media;
                    last_sidecar = result.protection_sidecar_path;
                }
                Err(error) => {
                    tracing::warn!(
                        "[patreon] attachment {}/{} failed: {}",
                        position + 1,
                        total,
                        error
                    );
                    failures.push(format!(
                        "{}: {}",
                        entry.label,
                        actionable_error_message(
                            &format!("{error:#}"),
                            has_saved_patreon_cookies(),
                        )
                    ));
                }
            }
        }

        if success_count == 0 {
            return Err(anyhow!(failures.last().cloned().unwrap_or_else(|| {
                "Patreon post download finished without producing any files".to_string()
            })));
        }
        if !failures.is_empty() {
            return Err(anyhow!(partial_failure_message(
                success_count,
                total,
                &output_dir,
                &failures,
            )));
        }

        let _ = progress.send(ProgressUpdate::percent(100.0)).await;
        Ok(DownloadResult {
            file_path: if success_count > 1 {
                output_dir
            } else {
                last_path
            },
            file_size_bytes: total_bytes,
            duration_seconds: 0.0,
            torrent_id: None,
            protected_media: last_protected_media,
            protection_sidecar_path: last_sidecar,
        })
    }
}

#[async_trait]
impl PlatformDownloader for PatreonDownloader {
    fn name(&self) -> &str {
        "patreon"
    }

    fn can_handle(&self, url: &str) -> bool {
        patreon_post_id(url).is_some()
    }

    async fn get_media_info(&self, url: &str) -> anyhow::Result<MediaInfo> {
        let ytdlp_path = ytdlp::ensure_ytdlp()
            .await
            .context("Patreon requires yt-dlp")?;
        let json = ytdlp::get_video_info(&ytdlp_path, url, &[])
            .await
            .map_err(|error| actionable_error(error, has_saved_patreon_cookies()))?;
        Self::parse_video_info(&json, url)
            .map_err(|error| actionable_error(error, has_saved_patreon_cookies()))
    }

    async fn download(
        &self,
        info: &MediaInfo,
        opts: &DownloadOptions,
        progress: mpsc::Sender<ProgressUpdate>,
    ) -> anyhow::Result<DownloadResult> {
        if info.media_type == MediaType::Playlist {
            return self.download_playlist(info, opts, progress).await;
        }

        let mut patreon_opts = opts.clone();
        if patreon_opts.referer.is_none() {
            patreon_opts.referer = Some(PATREON_REFERER.to_string());
        }
        if matches!(info.media_type, MediaType::File | MediaType::Photo) {
            disable_media_postprocessors(
                patreon_opts
                    .custom_ytdlp_args
                    .get_or_insert_with(Vec::new),
            );
        }
        self.fallback
            .download(info, &patreon_opts, progress)
            .await
            .map_err(|error| actionable_error(error, has_saved_patreon_cookies()))
    }
}

pub fn is_patreon_post_url(url: &str) -> bool {
    patreon_post_id(url).is_some()
}

pub fn patreon_post_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host != "patreon.com" {
        return None;
    }

    if parsed.path().trim_end_matches('/') == "/creation" {
        return parsed
            .query_pairs()
            .find(|(key, value)| key == "hid" && value.chars().all(|c| c.is_ascii_digit()))
            .map(|(_, value)| value.into_owned());
    }

    let segments: Vec<&str> = parsed
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect();
    let post_segment = segments
        .windows(2)
        .find(|pair| pair[0].eq_ignore_ascii_case("posts"))?
        .get(1)?;
    let id = post_segment.rsplit('-').next()?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| id.to_string())
}

fn entry_has_downloadable_media(entry: &serde_json::Value) -> bool {
    entry
        .get("url")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"))
        || entry
            .get("formats")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|formats| !formats.is_empty())
}

fn entry_title(entry: &serde_json::Value, index: usize) -> String {
    entry
        .get("alt_title")
        .or_else(|| entry.get("title"))
        .or_else(|| entry.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("Attachment {}", index + 1))
}

fn entry_file_size(entry: &serde_json::Value) -> Option<u64> {
    entry
        .get("filesize")
        .or_else(|| entry.get("filesize_approx"))
        .and_then(serde_json::Value::as_u64)
}

fn media_type_from_json(json: &serde_json::Value) -> MediaType {
    if json
        .get("vcodec")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|codec| codec != "none")
        || json
            .get("formats")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|formats| {
                formats.iter().any(|format| {
                    format
                        .get("vcodec")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|codec| codec != "none")
                })
            })
        || json.get("media_type").and_then(serde_json::Value::as_str) == Some("video")
    {
        return MediaType::Video;
    }

    let extension = json
        .get("ext")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp3" | "m4a" | "aac" | "flac" | "ogg" | "opus" | "wav" => MediaType::Audio,
        "mp4" | "m4v" | "mkv" | "mov" | "webm" | "avi" | "m3u8" => MediaType::Video,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "heic" => MediaType::Photo,
        "" if json
            .get("acodec")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|codec| codec != "none") =>
        {
            MediaType::Audio
        }
        _ => MediaType::File,
    }
}

fn extract_quality_height(quality: Option<&str>) -> Option<u32> {
    let quality = quality?.trim().to_ascii_lowercase();
    if matches!(quality.as_str(), "best" | "highest") {
        return None;
    }
    quality.trim_end_matches('p').parse().ok()
}

fn patreon_entry_flags(mut flags: Vec<String>, entry_index: usize) -> Vec<String> {
    // These flags come after the shared runtime's --no-playlist and select one
    // freshly extracted attachment. Patreon posts may include PDFs, archives,
    // and project files; media-only metadata/thumbnail post-processors cause
    // yt-dlp to download those files successfully and then exit with an ffmpeg
    // error. Explicit opt-outs are honored by the shared yt-dlp runtime.
    flags.extend([
        "--yes-playlist".to_string(),
        "--playlist-items".to_string(),
        entry_index.to_string(),
    ]);
    disable_media_postprocessors(&mut flags);
    flags
}

fn disable_media_postprocessors(flags: &mut Vec<String>) {
    for flag in ["--no-embed-metadata", "--no-embed-thumbnail"] {
        if !flags.iter().any(|existing| existing == flag) {
            flags.push(flag.to_string());
        }
    }
}

fn partial_failure_message(
    success_count: usize,
    total: usize,
    output_dir: &std::path::Path,
    failures: &[String],
) -> String {
    format!(
        "Patreon post was only partially downloaded ({success_count}/{total} attachments saved to {}). Retry to fetch the missing attachment(s). Failures: {}",
        output_dir.display(),
        failures.join(" | ")
    )
}

fn has_saved_patreon_cookies() -> bool {
    cookie_provider::cookie_path_for("patreon.com").is_some_and(|path| {
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false)
    })
}

fn actionable_error(error: anyhow::Error, has_saved_cookies: bool) -> anyhow::Error {
    anyhow!(actionable_error_message(
        &error.to_string(),
        has_saved_cookies
    ))
}

fn actionable_error_message(message: &str, has_saved_cookies: bool) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("you do not have access to this post")
        || lower.contains("current_user_can_view")
        || lower.contains("patron-only")
    {
        return if has_saved_cookies {
            "Patreon denied access to this post. The saved Patreon session may be expired, or its account is not entitled to this post. Refresh patreon.com cookies from a signed-in browser account with access, then retry."
                .to_string()
        } else {
            "Patreon denied access to this post, and no saved Patreon cookies were found. Sign in with an entitled account, save cookies for patreon.com, then retry."
                .to_string()
        };
    }
    if lower.contains("no supported media found")
        || lower.contains("no supported downloadable attachment")
    {
        return "This Patreon post is accessible, but it has no supported downloadable attachment or embedded video/audio."
            .to_string();
    }
    if lower.contains("login required")
        || lower.contains("authentication required")
        || lower.contains("http error 401")
        || lower.contains("http error 403")
    {
        return "Patreon authentication failed. Refresh patreon.com cookies from the signed-in browser account that can view this post, then retry."
            .to_string();
    }
    if lower.contains("not found") || lower.contains("http error 404") {
        return "Patreon could not find this post. Check that the link is correct and the post has not been deleted."
            .to_string();
    }
    format!("Patreon download failed: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_supported_post_urls_and_extracts_ids() {
        assert_eq!(
            patreon_post_id("https://www.patreon.com/FanuFatGyver/posts/mixing-drums-up-163067112")
                .as_deref(),
            Some("163067112")
        );
        assert_eq!(
            patreon_post_id("https://www.patreon.com/posts/episode-166-of-743933").as_deref(),
            Some("743933")
        );
        assert_eq!(
            patreon_post_id("https://patreon.com/creation?hid=743933").as_deref(),
            Some("743933")
        );
    }

    #[test]
    fn rejects_profiles_lists_and_lookalike_hosts() {
        assert!(patreon_post_id("https://www.patreon.com/FanuFatGyver").is_none());
        assert!(patreon_post_id("https://www.patreon.com/FanuFatGyver/posts").is_none());
        assert!(patreon_post_id("https://patreon.com.evil.example/posts/test-123").is_none());
    }

    #[test]
    fn parses_single_audio_attachment() {
        let json = serde_json::json!({
            "id": "743933",
            "title": "Episode 166",
            "uploader": "Cognitive Dissonance Podcast",
            "extractor_key": "Patreon",
            "url": "https://c10.patreonusercontent.com/audio.mp3?token=short-lived",
            "webpage_url": "https://www.patreon.com/posts/episode-166-of-743933",
            "ext": "mp3",
            "filesize": 93742539
        });
        let info = PatreonDownloader::parse_video_info(
            &json,
            "https://www.patreon.com/posts/episode-166-of-743933",
        )
        .unwrap();

        assert_eq!(info.platform, "patreon");
        assert_eq!(info.media_type, MediaType::Audio);
        assert_eq!(info.file_size_bytes, Some(93742539));
        assert_eq!(
            info.available_qualities[0].url,
            "https://www.patreon.com/posts/episode-166-of-743933"
        );
    }

    #[test]
    fn parses_multiple_attachments_without_caching_signed_urls() {
        let source_url = "https://www.patreon.com/creator/posts/lesson-123";
        let json = serde_json::json!({
            "id": "123",
            "title": "Lesson files",
            "uploader": "Creator",
            "entries": [
                {"id": "123-1", "alt_title": "lesson.mp4", "url": "https://cdn.example/video.mp4", "filesize": 10},
                {"id": "123-2", "alt_title": "notes.pdf", "url": "https://cdn.example/notes.pdf", "filesize": 20}
            ]
        });
        let info = PatreonDownloader::parse_video_info(&json, source_url).unwrap();

        assert_eq!(info.media_type, MediaType::Playlist);
        assert_eq!(info.file_size_bytes, Some(30));
        assert_eq!(info.available_qualities.len(), 2);
        assert!(info
            .available_qualities
            .iter()
            .all(|entry| entry.url == source_url));
        assert_eq!(info.available_qualities[1].format, "patreon_entry:2");
    }

    #[test]
    fn denied_access_error_explains_missing_or_stale_cookies() {
        let raw = "ERROR: [patreon] 163067112: You do not have access to this post";
        assert!(actionable_error_message(raw, false).contains("no saved Patreon cookies"));
        assert!(actionable_error_message(raw, true).contains("may be expired"));
    }

    #[test]
    fn accessible_post_without_media_has_specific_error() {
        let raw = "ERROR: [patreon] 1: No supported media found in this post";
        let message = actionable_error_message(raw, true);
        assert!(message.contains("accessible"));
        assert!(message.contains("no supported downloadable"));
    }

    #[test]
    fn attachment_flags_disable_media_only_post_processors() {
        let flags = patreon_entry_flags(vec!["--no-overwrites".to_string()], 3);
        assert!(flags.windows(2).any(|pair| pair == ["--playlist-items", "3"]));
        assert!(flags.iter().any(|flag| flag == "--no-embed-metadata"));
        assert!(flags.iter().any(|flag| flag == "--no-embed-thumbnail"));
    }

    #[test]
    fn single_file_flags_disable_media_only_post_processors_without_duplicates() {
        let mut flags = vec!["--no-embed-metadata".to_string()];
        disable_media_postprocessors(&mut flags);
        assert_eq!(
            flags
                .iter()
                .filter(|flag| flag.as_str() == "--no-embed-metadata")
                .count(),
            1
        );
        assert!(flags.iter().any(|flag| flag == "--no-embed-thumbnail"));
    }

    #[test]
    fn partial_downloads_are_not_reported_as_success() {
        let message = partial_failure_message(
            1,
            3,
            std::path::Path::new("/tmp/post"),
            &["notes.pdf: post-processing failed".to_string()],
        );
        assert!(message.contains("1/3"));
        assert!(message.contains("partially downloaded"));
        assert!(message.contains("notes.pdf"));
    }
}
