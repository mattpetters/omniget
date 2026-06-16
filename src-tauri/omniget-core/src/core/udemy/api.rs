//! Udemy api-2.0 client + lecture/course metadata.
//! Ported from `mattpetters/udemy-dl` (`src/api/*`), with the portal host
//! derived from each lecture URL instead of hardcoded to `intuit.udemy.com`.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use reqwest::{cookie::Jar, Client};
use serde::Deserialize;

use super::UDEMY_UA;

/// Extract and validate the Udemy portal host from a URL (`*.udemy.com`).
pub fn udemy_host(url: &str) -> Result<String> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("invalid Udemy URL: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("Udemy URL has no host"))?
        .to_lowercase();
    if host == "udemy.com" || host.ends_with(".udemy.com") {
        Ok(host)
    } else {
        Err(anyhow!("not a udemy.com host: {host}"))
    }
}

/// Authenticated Udemy api-2.0 client. The portal host (e.g. `www.udemy.com` or
/// `acme.udemy.com`) is captured so the license-server URL matches the portal.
#[derive(Clone)]
pub struct UdemyClient {
    pub http: Client,
    host: String,
    base_url: String,
}

impl UdemyClient {
    /// Build a client for the portal that owns `lecture_url`, authenticating
    /// with `jar` (browser-extension cookies). The `UDEMY_UA` invariant is
    /// applied here so the token's embedded UA matches the later license POST.
    pub fn from_lecture_url(lecture_url: &str, jar: Option<Arc<Jar>>) -> Result<Self> {
        Self::new(&udemy_host(lecture_url)?, jar)
    }

    pub fn new(host: &str, jar: Option<Arc<Jar>>) -> Result<Self> {
        let base_url = format!("https://{host}/api-2.0");

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            format!("https://{host}/").parse()?,
        );
        headers.insert(
            reqwest::header::ACCEPT,
            "application/json, text/plain, */*".parse()?,
        );

        let mut builder = crate::core::http_client::apply_global_proxy(Client::builder())
            .user_agent(UDEMY_UA)
            .default_headers(headers);
        if let Some(jar) = jar {
            builder = builder.cookie_provider(jar);
        }

        Ok(Self {
            http: builder.build()?,
            host: host.to_string(),
            base_url,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// Widevine license endpoint for this portal.
    pub fn license_url(&self) -> String {
        format!(
            "https://{}/media-license-server/validate-auth-token",
            self.host
        )
    }
}

// ---- Courses ----

#[derive(Debug, Clone, Deserialize)]
pub struct Course {
    pub id: u64,
    #[serde(default)]
    pub title: String,
}

/// Extract the course slug from a lecture/course URL.
/// `https://www.udemy.com/course/my-course/learn/lecture/123` → `my-course`.
pub fn parse_slug(input: &str) -> String {
    if let Some(after) = input.split("/course/").nth(1) {
        after.split('/').next().unwrap_or(after).to_string()
    } else {
        input.trim_matches('/').to_string()
    }
}

/// Extract the numeric lecture id from a lecture URL (`.../lecture/{id}`).
pub fn parse_lecture_id(input: &str) -> Option<u64> {
    let after = input.split("/lecture/").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Resolve a course slug to its numeric ID.
pub async fn resolve_course(client: &UdemyClient, slug: &str) -> Result<Course> {
    let url = format!("{}/courses/{}/", client.base_url(), slug);
    let params = [("fields[course]", "id,title")];
    let resp = client
        .http
        .get(&url)
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json::<Course>()
        .await?;
    Ok(resp)
}

// ---- Lectures ----

#[derive(Debug, Clone, Deserialize)]
pub struct MediaSource {
    #[serde(rename = "type")]
    pub media_type: String,
    pub src: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct LectureDetail {
    pub id: u64,
    pub title: String,
    pub asset_type: String,
    pub is_drmed: bool,
    /// Widevine auth token — expires quickly, fetch fresh per lecture.
    pub media_license_token: Option<String>,
    pub media_sources: Vec<MediaSource>,
}

pub async fn get_lecture_detail(
    client: &UdemyClient,
    course_id: u64,
    lecture_id: u64,
) -> Result<LectureDetail> {
    let url = format!(
        "{}/users/me/subscribed-courses/{}/lectures/{}/",
        client.base_url(),
        course_id,
        lecture_id
    );
    let params = [
        ("fields[lecture]", "id,title,asset"),
        (
            "fields[asset]",
            "asset_type,length,media_license_token,course_is_drmed,media_sources",
        ),
    ];

    let resp = client
        .http
        .get(&url)
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;

    let asset = &resp["asset"];
    let media_sources: Vec<MediaSource> = asset["media_sources"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    Ok(LectureDetail {
        id: resp["id"].as_u64().unwrap_or(lecture_id),
        title: resp["title"].as_str().unwrap_or("").to_string(),
        asset_type: asset["asset_type"].as_str().unwrap_or("Video").to_string(),
        is_drmed: asset["course_is_drmed"].as_bool().unwrap_or(false),
        media_license_token: asset["media_license_token"].as_str().map(|s| s.to_string()),
        media_sources,
    })
}

/// Select the HLS master playlist URL (`application/x-mpegURL`).
pub fn select_hls(sources: &[MediaSource]) -> Option<String> {
    sources
        .iter()
        .find(|s| s.media_type == "application/x-mpegURL")
        .map(|s| s.src.clone())
}

/// Select the best non-DRM MP4 source: exact quality match, else highest.
/// Returns `(url, quality_label)`.
pub fn select_best_mp4(
    sources: &[MediaSource],
    preferred_quality: &str,
) -> Option<(String, String)> {
    let target: Option<u32> = preferred_quality.parse().ok();
    let mp4: Vec<&MediaSource> = sources
        .iter()
        .filter(|s| s.media_type == "video/mp4")
        .collect();
    if mp4.is_empty() {
        return None;
    }
    if let Some(target_q) = target {
        if let Some(exact) = mp4
            .iter()
            .find(|s| s.label.parse::<u32>().unwrap_or(0) == target_q)
        {
            return Some((exact.src.clone(), exact.label.clone()));
        }
    }
    let best = mp4
        .iter()
        .max_by_key(|s| s.label.parse::<u32>().unwrap_or(0))?;
    Some((best.src.clone(), best.label.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4(label: &str) -> MediaSource {
        MediaSource {
            media_type: "video/mp4".into(),
            src: format!("https://cdn/{label}.mp4"),
            label: label.into(),
        }
    }
    fn hls() -> MediaSource {
        MediaSource {
            media_type: "application/x-mpegURL".into(),
            src: "https://cdn/master.m3u8".into(),
            label: String::new(),
        }
    }

    #[test]
    fn host_derivation() {
        assert_eq!(
            udemy_host("https://www.udemy.com/course/x/learn/lecture/1").unwrap(),
            "www.udemy.com"
        );
        assert_eq!(
            udemy_host("https://acme.udemy.com/course/x/learn/lecture/1").unwrap(),
            "acme.udemy.com"
        );
        assert!(udemy_host("https://example.com/x").is_err());
    }

    #[test]
    fn slug_and_lecture_id() {
        let u = "https://www.udemy.com/course/my-course/learn/lecture/98765";
        assert_eq!(parse_slug(u), "my-course");
        assert_eq!(parse_lecture_id(u), Some(98765));
    }

    #[test]
    fn license_url_matches_portal() {
        let c = UdemyClient::new("acme.udemy.com", None).unwrap();
        assert_eq!(
            c.license_url(),
            "https://acme.udemy.com/media-license-server/validate-auth-token"
        );
    }

    #[test]
    fn mp4_selection() {
        let s = vec![mp4("480"), mp4("1080"), mp4("720")];
        assert_eq!(select_best_mp4(&s, "best").unwrap().1, "1080");
        assert_eq!(select_best_mp4(&s, "720").unwrap().1, "720");
        assert_eq!(select_best_mp4(&s, "1440").unwrap().1, "1080");
        assert!(select_best_mp4(&[], "720").is_none());
    }

    #[test]
    fn hls_selection() {
        assert!(select_hls(&[mp4("720"), hls()]).is_some());
        assert!(select_hls(&[mp4("720")]).is_none());
    }
}
