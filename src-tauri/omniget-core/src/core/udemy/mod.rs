//! Udemy Widevine (CBCS) DRM download + decryption.
//!
//! Ported from the proven pipeline in `mattpetters/udemy-dl` (verified
//! end-to-end 2026-05-24). For each DRM lecture:
//!   1. Fetch lecture detail → `media_license_token` + HLS source ([`api`]).
//!   2. Download encrypted CBCS HLS via `N_m3u8DL-RE` ([`drm`]).
//!   3. Extract the Widevine PSSH box ([`pssh`]).
//!   4. Get content keys via the bundled `widevine_cdm.py` + a user-supplied
//!      L3 `.wvd` CDM ([`drm::get_content_keys`]).
//!   5. Decrypt with `mp4decrypt` (Bento4), mux with `ffmpeg`.
//!
//! ## Hard invariant — User-Agent consistency
//!
//! The Udemy API embeds the requesting User-Agent into the JWT
//! `media_license_token` (field `user_agent`). The license server then
//! validates that the UA on the license POST matches the UA embedded in the
//! token. A mismatch yields **HTTP 401** (not 403), which masquerades as an
//! auth/CDM problem. Therefore [`UDEMY_UA`] MUST be used for ALL `*.udemy.com`
//! requests: the API client, the `N_m3u8DL-RE -H` header, and the license POST
//! inside `widevine_cdm.py`.

pub mod api;
pub mod drm;
pub mod pssh;

/// User-Agent for every `*.udemy.com` request. See module docs for why this
/// must be identical across the API client, N_m3u8DL-RE, and the license POST.
/// Udemy's CloudFront WAF also rejects `Python-urllib/*` and browser UAs with
/// 403; a plain curl UA is consistently accepted.
pub const UDEMY_UA: &str = "curl/8.7.1";

/// Returns true for a Udemy lecture URL we can drive the DRM pipeline against:
/// `https://{*.}udemy.com/course/{slug}/learn/lecture/{id}`.
pub fn is_udemy_lecture_url(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let host_ok = parsed
        .host_str()
        .map(|h| {
            let h = h.to_lowercase();
            h == "udemy.com" || h.ends_with(".udemy.com")
        })
        .unwrap_or(false);
    if !host_ok {
        return false;
    }
    let path = parsed.path();
    path.contains("/course/") && path.contains("/learn/lecture/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_personal_and_enterprise_lecture_urls() {
        assert!(is_udemy_lecture_url(
            "https://www.udemy.com/course/foo/learn/lecture/12345"
        ));
        assert!(is_udemy_lecture_url(
            "https://intuit.udemy.com/course/foo/learn/lecture/12345"
        ));
    }

    #[test]
    fn rejects_non_lecture_and_non_udemy() {
        assert!(!is_udemy_lecture_url("https://www.udemy.com/course/foo/"));
        assert!(!is_udemy_lecture_url(
            "https://example.com/course/foo/learn/lecture/1"
        ));
        assert!(!is_udemy_lecture_url("not a url"));
    }
}
