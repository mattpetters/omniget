use anyhow::{anyhow, Context};
use async_trait::async_trait;
use regex::Regex;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, USER_AGENT};
use std::collections::HashSet;
use tokio::sync::mpsc;

use crate::core::ytdlp;
use crate::models::media::{DownloadOptions, DownloadResult, MediaInfo, MediaType, VideoQuality};
use crate::models::progress::ProgressUpdate;
use crate::platforms::cookie_provider;
use crate::platforms::traits::PlatformDownloader;

const MWTM_DOMAIN: &str = "mixwiththemasters.com";
const MWTM_ORIGIN: &str = "https://mixwiththemasters.com/";
const MWTM_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Debug, Clone, PartialEq, Eq)]
struct MwtmVideoUrl {
    slug: String,
    part: Option<u32>,
}

#[derive(Debug, Clone)]
struct PartLink {
    number: u32,
    label: String,
    url: String,
}

#[derive(Debug, Clone)]
struct ParsedPage {
    course_title: String,
    author: String,
    thumbnail_url: Option<String>,
    playlist_url: Option<String>,
    parts: Vec<PartLink>,
}

/// Authenticated Mix With The Masters video/course downloader.
///
/// MWTM keeps the public course URL stable, but emits a short-lived HLS URL
/// only for an entitled browser session. The extension stores that session in
/// OmniGet's managed cookie jar. We therefore retain stable part URLs in
/// `MediaInfo` and resolve each signed playlist immediately before downloading.
pub struct MixWithTheMastersDownloader;

impl Default for MixWithTheMastersDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl MixWithTheMastersDownloader {
    pub fn new() -> Self {
        Self
    }

    async fn fetch_page(url: &str, user_agent: Option<&str>) -> anyhow::Result<String> {
        let mut builder = crate::core::http_client::apply_global_proxy(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(8))
                .timeout(std::time::Duration::from_secs(45))
                .default_headers({
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert(
                        ACCEPT,
                        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
                            .parse()
                            .expect("static accept header"),
                    );
                    headers.insert(
                        ACCEPT_LANGUAGE,
                        "en-US,en;q=0.9".parse().expect("static language header"),
                    );
                    headers.insert(
                        USER_AGENT,
                        user_agent
                            .unwrap_or(MWTM_USER_AGENT)
                            .parse()
                            .unwrap_or_else(|_| {
                                MWTM_USER_AGENT.parse().expect("static user agent")
                            }),
                    );
                    headers
                }),
        );

        if let Some(jar) = crate::core::cookie_parser::load_extension_cookies_for_url(url) {
            builder = builder.cookie_provider(jar);
        }

        let response = builder
            .build()
            .context("could not create the MWTM HTTP client")?
            .get(url)
            .send()
            .await
            .context("could not open the MWTM video page")?;
        let status = response.status();
        let final_url = response.url().clone();
        let body = response
            .text()
            .await
            .context("could not read the MWTM video page")?;

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(anyhow!(authentication_error()));
        }
        if !status.is_success() {
            return Err(anyhow!(
                "MWTM returned HTTP {} for this video. Check the URL and retry.",
                status.as_u16()
            ));
        }
        if final_url.path().contains("login") || final_url.path().contains("sign-in") {
            return Err(anyhow!(authentication_error()));
        }

        Ok(body)
    }

    async fn media_info(url: &str) -> anyhow::Result<MediaInfo> {
        let parsed_url = parse_mwtm_video_url(url)
            .ok_or_else(|| anyhow!("Unsupported Mix With The Masters URL"))?;
        let html = Self::fetch_page(url, None).await?;
        let page = parse_page(&html, url)?;

        if parsed_url.part.is_none() {
            if page.parts.is_empty() {
                return Err(anyhow!(
                    "MWTM course access was recognized, but no downloadable parts were found. {}",
                    refresh_cookie_hint()
                ));
            }
            let qualities = page
                .parts
                .iter()
                .map(|part| VideoQuality {
                    label: part.label.clone(),
                    width: 0,
                    height: 0,
                    url: part.url.clone(),
                    format: format!("mwtm_part:{}", part.number),
                })
                .collect();
            return Ok(MediaInfo {
                title: page.course_title,
                author: page.author,
                platform: "mixwiththemasters".to_string(),
                duration_seconds: None,
                thumbnail_url: page.thumbnail_url,
                available_qualities: qualities,
                media_type: MediaType::Playlist,
                file_size_bytes: None,
            });
        }

        if page.playlist_url.is_none() {
            return Err(anyhow!(authentication_error()));
        }
        let part_number = parsed_url.part.unwrap_or(1);
        let label = part_label(part_number);
        Ok(MediaInfo {
            title: format!("{} - {}", page.course_title, label),
            author: page.author,
            platform: "mixwiththemasters".to_string(),
            duration_seconds: None,
            thumbnail_url: page.thumbnail_url,
            available_qualities: vec![VideoQuality {
                label,
                width: 0,
                height: 0,
                url: url.to_string(),
                format: format!("mwtm_part:{part_number}"),
            }],
            media_type: MediaType::Video,
            file_size_bytes: None,
        })
    }

    async fn download_entries(
        info: &MediaInfo,
        opts: &DownloadOptions,
        progress: mpsc::Sender<ProgressUpdate>,
    ) -> anyhow::Result<DownloadResult> {
        let entries = &info.available_qualities;
        if entries.is_empty() {
            return Err(anyhow!("MWTM video has no downloadable parts"));
        }

        let ytdlp_path = match opts.ytdlp_path.as_ref() {
            Some(path) => path.clone(),
            None => ytdlp::ensure_ytdlp()
                .await
                .context("MWTM downloads require yt-dlp")?,
        };
        let multiple = entries.len() > 1 || info.media_type == MediaType::Playlist;
        let output_dir = if multiple {
            opts.output_dir
                .join(sanitize_filename::sanitize(&info.title))
        } else {
            opts.output_dir.clone()
        };
        tokio::fs::create_dir_all(&output_dir)
            .await
            .context("could not create the MWTM output folder")?;

        let cookie_path = cookie_provider::cookie_path_for(MWTM_DOMAIN);
        let total = entries.len();
        let mut total_bytes = 0_u64;
        let mut completed = 0_usize;
        let mut last_path = output_dir.clone();
        let mut failures = Vec::new();

        for (position, entry) in entries.iter().enumerate() {
            if opts.cancel_token.is_cancelled() {
                return Err(anyhow!("Download cancelled"));
            }

            let html = match Self::fetch_page(&entry.url, opts.user_agent.as_deref()).await {
                Ok(html) => html,
                Err(error) => {
                    failures.push(format!("{}: {error:#}", entry.label));
                    continue;
                }
            };
            let page = match parse_page(&html, &entry.url) {
                Ok(page) => page,
                Err(error) => {
                    failures.push(format!("{}: {error:#}", entry.label));
                    continue;
                }
            };
            let Some(playlist_url) = page.playlist_url else {
                failures.push(format!("{}: {}", entry.label, authentication_error()));
                continue;
            };

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
                        + update.percent.clamp(0.0, 100.0) / total as f64;
                    let _ = aggregate_tx
                        .send(ProgressUpdate::rich(
                            overall,
                            update.downloaded_bytes,
                            update.total_bytes,
                            update.speed_bps,
                            update.eta_seconds,
                        ))
                        .await;
                }
            });

            let literal_name = if multiple {
                format!("{:02} - {}", position + 1, entry.label)
            } else {
                info.title.clone()
            };
            let filename_template = format!(
                "{}.%(ext)s",
                sanitize_filename::sanitize(&literal_name).replace('%', "%%")
            );
            let extra_flags = opts.custom_ytdlp_args.clone().unwrap_or_default();
            let result = ytdlp::download_video(
                &ytdlp_path,
                &playlist_url,
                &output_dir,
                quality_height(opts.quality.as_deref()),
                entry_tx,
                opts.download_mode.as_deref(),
                opts.format_id.as_deref(),
                Some(&filename_template),
                Some(&entry.url),
                opts.cancel_token.clone(),
                cookie_path.as_deref(),
                opts.concurrent_fragments,
                opts.download_subtitles,
                &extra_flags,
                opts.audio_format.as_deref(),
            )
            .await;
            let _ = forwarder.await;

            match result {
                Ok(result) => {
                    completed += 1;
                    total_bytes = total_bytes.saturating_add(result.file_size_bytes);
                    last_path = result.file_path;
                }
                Err(error) => failures.push(format!("{}: {error:#}", entry.label)),
            }
        }

        if completed == 0 {
            return Err(anyhow!(failures.last().cloned().unwrap_or_else(|| {
                "MWTM download did not produce any files".to_string()
            })));
        }
        if !failures.is_empty() {
            return Err(anyhow!(
                "MWTM course was only partially downloaded ({completed}/{total} parts saved to {}). Retry to fetch the missing part(s): {}",
                output_dir.display(),
                failures.join("; ")
            ));
        }

        let _ = progress.send(ProgressUpdate::percent(100.0)).await;
        Ok(DownloadResult {
            file_path: if multiple { output_dir } else { last_path },
            file_size_bytes: total_bytes,
            duration_seconds: 0.0,
            torrent_id: None,
            protected_media: None,
            protection_sidecar_path: None,
        })
    }
}

#[async_trait]
impl PlatformDownloader for MixWithTheMastersDownloader {
    fn name(&self) -> &str {
        "mixwiththemasters"
    }

    fn can_handle(&self, url: &str) -> bool {
        is_mix_with_the_masters_video_url(url)
    }

    async fn get_media_info(&self, url: &str) -> anyhow::Result<MediaInfo> {
        Self::media_info(url).await
    }

    async fn download(
        &self,
        info: &MediaInfo,
        opts: &DownloadOptions,
        progress: mpsc::Sender<ProgressUpdate>,
    ) -> anyhow::Result<DownloadResult> {
        Self::download_entries(info, opts, progress).await
    }
}

pub fn is_mix_with_the_masters_video_url(url: &str) -> bool {
    parse_mwtm_video_url(url).is_some()
}

pub fn mix_with_the_masters_slug(url: &str) -> Option<String> {
    parse_mwtm_video_url(url).map(|parsed| parsed.slug)
}

fn parse_mwtm_video_url(url: &str) -> Option<MwtmVideoUrl> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.trim_start_matches("www.");
    if host != MWTM_DOMAIN {
        return None;
    }
    let segments: Vec<&str> = parsed
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect();
    let videos_index = segments.iter().position(|segment| *segment == "videos")?;
    let slug = *segments.get(videos_index + 1)?;
    if slug.is_empty() || slug == "_" {
        return None;
    }
    let tail = &segments[(videos_index + 2)..];
    let part = match tail {
        [] => None,
        ["part"] => Some(1),
        ["part", number] => Some(number.parse().ok()?),
        _ => return None,
    };
    Some(MwtmVideoUrl {
        slug: slug.to_string(),
        part,
    })
}

fn parse_page(html: &str, page_url: &str) -> anyhow::Result<ParsedPage> {
    let headings_h1 = tag_texts(html, "h1");
    let headings_h2 = tag_texts(html, "h2");
    let headings_h3 = tag_texts(html, "h3");
    let subject = headings_h1
        .get(1)
        .or_else(|| headings_h1.first())
        .cloned()
        .unwrap_or_else(|| "Mix With The Masters".to_string());
    let topic = headings_h2
        .first()
        .cloned()
        .unwrap_or_else(|| "Video".to_string());
    let course_title = if subject.eq_ignore_ascii_case(&topic) {
        topic
    } else {
        format!("{subject} - {topic}")
    };
    let author = headings_h3
        .first()
        .cloned()
        .filter(|value| !value.eq_ignore_ascii_case("comments"))
        .unwrap_or_else(|| "Mix With The Masters".to_string());

    let playlist_url = extract_attribute_url(html, "source", "src", page_url, |url| {
        url.path().ends_with(".m3u8")
    });
    let thumbnail_url = extract_meta_content(html, "og:image", page_url);
    let parts = extract_part_links(html, page_url);

    if course_title.trim().is_empty() {
        return Err(anyhow!("MWTM page did not include course metadata"));
    }
    Ok(ParsedPage {
        course_title,
        author,
        thumbnail_url,
        playlist_url,
        parts,
    })
}

fn extract_part_links(html: &str, page_url: &str) -> Vec<PartLink> {
    let Ok(regex) = Regex::new(
        r#"(?is)<a\b[^>]*\bhref\s*=\s*["']([^"']*/videos/[^"']+/part(?:/[0-9]+)?/?(?:\?[^"']*)?)["'][^>]*>(.*?)</a>"#,
    ) else {
        return Vec::new();
    };
    let Ok(base) = url::Url::parse(page_url).or_else(|_| url::Url::parse(MWTM_ORIGIN)) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut parts = Vec::new();
    for captures in regex.captures_iter(html) {
        let href = decode_html(captures.get(1).map(|m| m.as_str()).unwrap_or_default());
        let Ok(mut resolved) = base.join(&href) else {
            continue;
        };
        resolved.set_fragment(None);
        let Some(parsed) = parse_mwtm_video_url(resolved.as_str()) else {
            continue;
        };
        let Some(number) = parsed.part else {
            continue;
        };
        // Part 0 is the public trailer. Keep it downloadable when pasted
        // directly, but do not mix it into a full paid course download.
        if number == 0 {
            continue;
        }
        resolved.set_query(None);
        let dedupe_key = format!("{}:{number}", parsed.slug);
        if !seen.insert(dedupe_key) {
            continue;
        }
        let label = clean_text(captures.get(2).map(|m| m.as_str()).unwrap_or_default());
        parts.push(PartLink {
            number,
            label: if label.is_empty() {
                part_label(number)
            } else {
                label
            },
            url: resolved.to_string(),
        });
    }
    parts.sort_by_key(|part| part.number);
    parts
}

fn extract_attribute_url(
    html: &str,
    tag: &str,
    attribute: &str,
    page_url: &str,
    predicate: impl Fn(&url::Url) -> bool,
) -> Option<String> {
    let regex = Regex::new(&format!(
        r#"(?is)<{}\b[^>]*\b{}\s*=\s*["']([^"']+)["']"#,
        regex::escape(tag),
        regex::escape(attribute)
    ))
    .ok()?;
    let base = url::Url::parse(page_url).ok()?;
    let result = regex.captures_iter(html).find_map(|captures| {
        let value = decode_html(captures.get(1)?.as_str());
        let resolved = base.join(&value).ok()?;
        predicate(&resolved).then(|| resolved.to_string())
    });
    result
}

fn extract_meta_content(html: &str, property: &str, page_url: &str) -> Option<String> {
    let regex = Regex::new(&format!(
        r#"(?is)<meta\b[^>]*(?:property|name)\s*=\s*["']{}["'][^>]*\bcontent\s*=\s*["']([^"']+)["']"#,
        regex::escape(property)
    ))
    .ok()?;
    let value = decode_html(regex.captures(html)?.get(1)?.as_str());
    url::Url::parse(page_url)
        .ok()?
        .join(&value)
        .ok()
        .map(|url| url.to_string())
}

fn tag_texts(html: &str, tag: &str) -> Vec<String> {
    let Ok(regex) = Regex::new(&format!(
        r#"(?is)<{}\b[^>]*>(.*?)</{}>"#,
        regex::escape(tag),
        regex::escape(tag)
    )) else {
        return Vec::new();
    };
    regex
        .captures_iter(html)
        .filter_map(|captures| {
            let text = clean_text(captures.get(1)?.as_str());
            (!text.is_empty()).then_some(text)
        })
        .collect()
}

fn clean_text(raw: &str) -> String {
    let without_tags = Regex::new(r"(?is)<[^>]+>")
        .map(|regex| regex.replace_all(raw, " ").into_owned())
        .unwrap_or_else(|_| raw.to_string());
    decode_html(&without_tags)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_html(raw: &str) -> String {
    raw.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

fn part_label(number: u32) -> String {
    if number == 0 {
        "Trailer".to_string()
    } else {
        format!("Part {number}")
    }
}

fn quality_height(quality: Option<&str>) -> Option<u32> {
    quality.and_then(|value| value.trim_end_matches('p').parse().ok())
}

fn has_saved_cookies() -> bool {
    cookie_provider::cookie_path_for(MWTM_DOMAIN)
        .and_then(|path| std::fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.len() > 0)
}

fn refresh_cookie_hint() -> &'static str {
    "Open the entitled video while signed in, then use the OmniGet browser extension to send the page and refresh its cookies."
}

fn authentication_error() -> String {
    if has_saved_cookies() {
        format!(
            "MWTM did not expose an authorized video playlist. The saved session may be expired or the account may not have access. {}",
            refresh_cookie_hint()
        )
    } else {
        format!(
            "MWTM requires an entitled signed-in session, and no saved Mix With The Masters cookies were found. {}",
            refresh_cookie_hint()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COURSE_URL: &str =
        "https://mixwiththemasters.com/videos/young-guru-choosing-outboard-gear-no-i-d-studio";

    #[test]
    fn recognizes_courses_parts_and_localized_urls() {
        assert!(is_mix_with_the_masters_video_url(COURSE_URL));
        assert!(is_mix_with_the_masters_video_url(&format!(
            "{COURSE_URL}/part"
        )));
        assert!(is_mix_with_the_masters_video_url(&format!(
            "{COURSE_URL}/part/3"
        )));
        assert!(is_mix_with_the_masters_video_url(
            "https://mixwiththemasters.com/fr/videos/example-course/part/2"
        ));
        assert!(!is_mix_with_the_masters_video_url(
            "https://mixwiththemasters.com/videos"
        ));
        assert!(!is_mix_with_the_masters_video_url(
            "https://mixwiththemasters.com/videos/_/playlist/example/part/1/Index.m3u8"
        ));
        assert!(!is_mix_with_the_masters_video_url(
            "https://notmixwiththemasters.com/videos/example"
        ));
    }

    #[test]
    fn parses_course_metadata_signed_playlist_and_parts() {
        let html = r#"
            <meta property="og:image" content="/images/course.jpg">
            <h1>Mixing</h1><h1>No I.D. Studio</h1>
            <h2>Choosing Outboard Gear</h2><h3><a>Young Guru</a></h3>
            <video><source src="/videos/_/playlist/example/part/1/Index.m3u8?_hash=abc&amp;userId=42" type="application/x-mpegURL"></video>
            <a href="/videos/example/part">Part 1</a>
            <a href="/videos/example/part/2"><strong>Part 2</strong></a>
            <a href="/videos/example/part/3">Part 3</a>
            <a href="/videos/example/part/0">Trailer</a>
            <a href="/videos/example/part/2">Part 2 duplicate</a>
        "#;
        let page = parse_page(html, COURSE_URL).unwrap();
        assert_eq!(page.course_title, "No I.D. Studio - Choosing Outboard Gear");
        assert_eq!(page.author, "Young Guru");
        assert_eq!(page.parts.len(), 3);
        assert_eq!(page.parts[0].number, 1);
        assert_eq!(page.parts[2].number, 3);
        assert!(page
            .playlist_url
            .as_deref()
            .is_some_and(|url| url.contains("_hash=abc&userId=42")));
        assert_eq!(
            page.thumbnail_url.as_deref(),
            Some("https://mixwiththemasters.com/images/course.jpg")
        );
    }

    #[test]
    fn trailer_is_individually_supported_but_not_in_course_parts() {
        let parsed = parse_mwtm_video_url(&format!("{COURSE_URL}/part/0")).unwrap();
        assert_eq!(parsed.part, Some(0));
        assert_eq!(part_label(0), "Trailer");
    }
}
