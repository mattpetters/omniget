//! Udemy Widevine (CBCS) DRM download + decrypt pipeline.
//! Ported from `mattpetters/udemy-dl` `src/download/drm.rs`, adapted to
//! omniget's process wrapper, cancellation tokens, and progress events.
//!
//! Pipeline: N_m3u8DL-RE (encrypted HLS) → PSSH extract → pywidevine CDM
//! (`widevine_cdm.py`) → mp4decrypt (Bento4) → ffmpeg remux.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::pssh::extract_pssh_b64;
use super::UDEMY_UA;
use crate::models::progress::ProgressUpdate;

/// The CDM license helper, embedded at compile time and written to disk on
/// demand (see [`materialize_cdm_script`]).
const CDM_SCRIPT: &str = include_str!("widevine_cdm.py");

/// Resolved external tools for the DRM pipeline. Built by the platform
/// downloader (which resolves/auto-downloads each binary) and passed in so this
/// module stays free of tool-discovery policy.
#[derive(Clone, Debug)]
pub struct DrmTools {
    pub n_m3u8dl: PathBuf,
    pub mp4decrypt: PathBuf,
    pub ffmpeg: PathBuf,
    pub python: PathBuf,
    pub cdm_script: PathBuf,
}

/// Write the embedded `widevine_cdm.py` to the managed bin dir and return its
/// path. Overwrites each run so the helper tracks the binary.
pub async fn materialize_cdm_script() -> Result<PathBuf> {
    let dir = crate::core::paths::app_data_dir()
        .ok_or_else(|| anyhow!("Could not determine app data directory"))?
        .join("bin");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("widevine_cdm.py");
    tokio::fs::write(&path, CDM_SCRIPT).await?;
    Ok(path)
}

/// Map a quality string to an N_m3u8DL-RE `--select-video` expression.
///
/// The `res=` value is a regex matched against the stream's `WIDTHxHEIGHT`
/// string. The separator is a literal `x` — using `*` is a bug (`*` is a regex
/// quantifier that matches no stream, leaving audio-only). Always `WIDTHxHEIGHT`.
pub(crate) fn video_selector(quality: &str) -> &'static str {
    match quality {
        "1080" | "best" | "highest" => "best",
        "720" => "res=1280x720",
        "480" => "res=854x480",
        "360" => "res=640x360",
        _ => "best",
    }
}

/// Full DRM download + decrypt for a single lecture. Returns the final muxed
/// `.mp4`. Idempotent: if the final file already exists it is returned as-is.
#[allow(clippy::too_many_arguments)]
pub async fn download_drm(
    hls_url: &str,
    auth_token: &str,
    license_url: &str,
    out_dir: &Path,
    base_name: &str,
    quality: &str,
    wvd_path: Option<&Path>,
    tools: &DrmTools,
    cancel: &CancellationToken,
    progress: &mpsc::Sender<ProgressUpdate>,
) -> Result<PathBuf> {
    let final_path = out_dir.join(format!("{base_name}.mp4"));
    if final_path.exists() {
        let _ = progress.send(ProgressUpdate::percent(100.0)).await;
        return Ok(final_path);
    }

    let tmp_dir = out_dir.join(format!("{base_name}.drm_tmp"));
    tokio::fs::create_dir_all(&tmp_dir).await?;

    // 1. Download encrypted CBCS HLS (video + optional audio) — progress 0..80%.
    let _ = progress.send(ProgressUpdate::percent(-2.0)).await; // "Connecting"
    let (enc_video, enc_audio) = download_encrypted_hls(
        hls_url,
        &tmp_dir,
        base_name,
        quality,
        &tools.n_m3u8dl,
        cancel,
        progress,
    )
    .await
    .context("N_m3u8DL-RE failed to download the encrypted HLS stream")?;

    check_cancel(cancel)?;

    // 2. Extract the Widevine PSSH from the encrypted video.
    let pssh_b64 = {
        let bytes = tokio::fs::read(&enc_video).await?;
        let head = &bytes[..bytes.len().min(50 * 1024)];
        extract_pssh_b64(head).context("Failed to extract Widevine PSSH from encrypted file")?
    };

    // 3. Fetch content keys via the pywidevine CDM helper — progress ~85%.
    let _ = progress.send(ProgressUpdate::percent(85.0)).await;
    let keys = get_content_keys(
        &pssh_b64,
        auth_token,
        license_url,
        wvd_path,
        &tools.python,
        &tools.cdm_script,
    )
    .await?;
    if keys.is_empty() {
        bail!("No Widevine content keys returned for: {base_name}");
    }

    check_cancel(cancel)?;

    // 4. Decrypt video (and audio) with mp4decrypt — progress ~90%.
    let _ = progress.send(ProgressUpdate::percent(90.0)).await;
    let dec_video = tmp_dir.join(format!("{base_name}.dec.mp4"));
    decrypt_with_mp4decrypt(&enc_video, &dec_video, &keys, &tools.mp4decrypt).await?;

    let dec_audio = if let Some(ref enc_a) = enc_audio {
        let ext = enc_a.extension().and_then(|e| e.to_str()).unwrap_or("m4a");
        let dec_a = tmp_dir.join(format!("{base_name}.dec.{ext}"));
        decrypt_with_mp4decrypt(enc_a, &dec_a, &keys, &tools.mp4decrypt).await?;
        Some(dec_a)
    } else {
        None
    };

    check_cancel(cancel)?;

    // 5. Mux + strip residual encryption signaling — progress ~98%.
    let _ = progress.send(ProgressUpdate::percent(98.0)).await;
    mux_ffmpeg(&dec_video, dec_audio.as_deref(), &final_path, &tools.ffmpeg).await?;

    // 6. Verify and clean up.
    let meta = std::fs::metadata(&final_path)
        .with_context(|| format!("Decrypted output missing: {}", final_path.display()))?;
    if meta.len() == 0 {
        let _ = std::fs::remove_file(&final_path);
        bail!("Decrypted output is empty (0 bytes) for: {base_name}");
    }
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    let _ = progress.send(ProgressUpdate::percent(100.0)).await;
    Ok(final_path)
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        bail!("Download cancelled");
    }
    Ok(())
}

/// Download encrypted CBCS HLS via N_m3u8DL-RE → `(video, Option<audio>)`.
///
/// No `--mux-after-done`: N_m3u8DL-RE cannot mux CBCS-encrypted streams; we
/// decrypt each first, then mux with ffmpeg. `--select-video`/`--select-audio`
/// suppress the interactive stream-selection prompt.
async fn download_encrypted_hls(
    hls_url: &str,
    tmp_dir: &Path,
    base_name: &str,
    quality: &str,
    tool: &Path,
    cancel: &CancellationToken,
    progress: &mpsc::Sender<ProgressUpdate>,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let video_canonical = tmp_dir.join(format!("{base_name}.mp4"));
    if video_canonical.exists() {
        return Ok((video_canonical, find_audio_file(tmp_dir)));
    }

    let mut cmd = crate::core::process::command(tool);
    cmd.arg(hls_url)
        .args(["--save-dir", &tmp_dir.to_string_lossy()])
        .args(["--save-name", base_name])
        .arg("--no-date-info")
        // Browser-ish UA so Udemy's WAF doesn't 403 the manifest.
        .args(["-H", &format!("User-Agent: {UDEMY_UA}")])
        .args(["--select-video", video_selector(quality)])
        .args(["--select-audio", "best"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "Failed to run N_m3u8DL-RE ({}) — is it installed?",
            tool.display()
        )
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Drain stderr to the download log (benign noise like the unsupported 'skd'
    // key scheme is expected — we handle Widevine decryption separately).
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(stderr) = stderr {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(id) = crate::core::log_hook::current_download_id() {
                    crate::core::log_hook::emit_log(id, &line);
                }
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    });

    // Parse progress from stdout (N_m3u8DL-RE prints a `NN.NN%` token). Scale to
    // 0..80% — decrypt/mux occupy the remainder.
    let progress_tx = progress.clone();
    let stdout_task = tokio::spawn(async move {
        if let Some(stdout) = stdout {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(id) = crate::core::log_hook::current_download_id() {
                    crate::core::log_hook::emit_log(id, &line);
                }
                if let Some(pct) = parse_percent(&line) {
                    let scaled = (pct * 0.8).clamp(0.0, 80.0);
                    let _ = progress_tx.send(ProgressUpdate::percent(scaled)).await;
                }
            }
        }
    });

    let status = tokio::select! {
        s = child.wait() => s.context("N_m3u8DL-RE process error")?,
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            bail!("Download cancelled");
        }
    };

    let _ = stdout_task.await;
    let stderr_out = stderr_task.await.unwrap_or_default();

    if !status.success() {
        let tail = stderr_out
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ");
        bail!("N_m3u8DL-RE failed (exit {status}): {tail}");
    }

    let video = if video_canonical.exists() {
        video_canonical
    } else {
        // Fall back to any .mp4 N_m3u8DL-RE produced.
        std::fs::read_dir(tmp_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|e| e.to_str()) == Some("mp4"))
            .ok_or_else(|| anyhow!("N_m3u8DL-RE ran but produced no .mp4 in {tmp_dir:?}"))?
    };

    Ok((video, find_audio_file(tmp_dir)))
}

/// Udemy CBCS audio is a separate `.m4a` file alongside the video.
fn find_audio_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("m4a"))
}

/// Run `widevine_cdm.py` to exchange the PSSH + token for `kid:key` pairs.
async fn get_content_keys(
    pssh_b64: &str,
    auth_token: &str,
    license_url: &str,
    wvd_path: Option<&Path>,
    python: &Path,
    script: &Path,
) -> Result<Vec<String>> {
    let mut cmd = crate::core::process::command(python);
    cmd.arg(script)
        .args(["--pssh", pssh_b64])
        .args(["--token", auth_token])
        .args(["--license-url", license_url]);
    if let Some(wvd) = wvd_path {
        cmd.args(["--wvd", &wvd.to_string_lossy()]);
    }

    let output = cmd
        .output()
        .await
        .context("Failed to run widevine_cdm.py (is python3 + pywidevine installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("widevine_cdm.py failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let keys: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.contains(':') && l.len() > 32)
        .collect();
    Ok(keys)
}

async fn decrypt_with_mp4decrypt(
    enc: &Path,
    dec: &Path,
    keys: &[String],
    mp4decrypt: &Path,
) -> Result<()> {
    let mut cmd = crate::core::process::command(mp4decrypt);
    for key in keys {
        cmd.args(["--key", key]);
    }
    cmd.arg(enc).arg(dec);

    let status = cmd.status().await.with_context(|| {
        format!(
            "Failed to run mp4decrypt ({}) — is Bento4 installed?",
            mp4decrypt.display()
        )
    })?;
    if !status.success() {
        bail!("mp4decrypt failed decrypting: {}", enc.display());
    }
    Ok(())
}

/// Mux decrypted video (+ optional audio) into one MP4, stripping residual
/// encryption signaling left by mp4decrypt (`-c copy`, no re-encode).
async fn mux_ffmpeg(
    video: &Path,
    audio: Option<&Path>,
    output: &Path,
    ffmpeg: &Path,
) -> Result<()> {
    let mut cmd = crate::core::process::command(ffmpeg);
    cmd.args(["-y", "-loglevel", "error", "-i"]).arg(video);
    if let Some(a) = audio {
        cmd.arg("-i").arg(a);
    }
    cmd.args(["-c", "copy", "-f", "mp4"]).arg(output);

    let status = cmd
        .status()
        .await
        .context("Failed to run ffmpeg for mux/remux")?;
    if !status.success() {
        bail!("ffmpeg mux failed for: {}", video.display());
    }
    Ok(())
}

/// Parse the last `NN.NN%` token from a line of N_m3u8DL-RE output.
fn parse_percent(line: &str) -> Option<f64> {
    let mut best: Option<f64> = None;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // Walk backwards over digits and a single dot.
            let mut j = i;
            let mut seen_dot = false;
            while j > 0 {
                let c = bytes[j - 1];
                if c.is_ascii_digit() {
                    j -= 1;
                } else if c == b'.' && !seen_dot {
                    seen_dot = true;
                    j -= 1;
                } else {
                    break;
                }
            }
            if j < i {
                if let Ok(v) = line[j..i].parse::<f64>() {
                    if (0.0..=100.0).contains(&v) {
                        best = Some(v);
                    }
                }
            }
        }
        i += 1;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_selector_uses_literal_x_separator() {
        assert_eq!(video_selector("720"), "res=1280x720");
        assert_eq!(video_selector("480"), "res=854x480");
        assert_eq!(video_selector("360"), "res=640x360");
    }

    #[test]
    fn video_selector_never_contains_asterisk() {
        for q in ["1080", "720", "480", "360", "best", "weird"] {
            assert!(!video_selector(q).contains('*'));
        }
    }

    #[test]
    fn video_selector_best_and_unknown_fall_back_to_best() {
        assert_eq!(video_selector("1080"), "best");
        assert_eq!(video_selector("best"), "best");
        assert_eq!(video_selector("anything"), "best");
    }

    #[test]
    fn parses_percent_token() {
        assert_eq!(
            parse_percent("Vid 1280x720 | 2535 Kbps  45.30%"),
            Some(45.30)
        );
        assert_eq!(parse_percent("100.00%"), Some(100.0));
        assert_eq!(parse_percent("no percent here"), None);
        assert_eq!(parse_percent("999% bogus"), None);
    }
}
