//! Udemy Widevine (CBCS) DRM download + decrypt pipeline.
//! Ported from `mattpetters/udemy-dl` `src/download/drm.rs`, adapted to
//! omniget's process wrapper, cancellation tokens, and progress events.
//!
//! Pipeline: N_m3u8DL-RE (encrypted HLS) → PSSH extract → pywidevine CDM
//! (`widevine_cdm.py`) → mp4decrypt (Bento4) → ffmpeg remux.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
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

fn usable_wvd(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// Resolve and validate the Widevine L3 device file before downloading the
/// encrypted media. This mirrors the helper's historical fallback locations,
/// but makes a missing/empty device an immediate actionable Rust error.
pub fn resolve_widevine_device_path(explicit: Option<&Path>, cdm_script: &Path) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if usable_wvd(path) {
            return Ok(path.to_path_buf());
        }
        bail!(
            "Widevine device file is missing, empty, or not a file: {}. Select a valid L3 .wvd device in Settings and retry",
            path.display()
        );
    }

    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".config/udemy-dl/device.wvd"));
        candidates.push(home.join("Library/Application Support/udemy-dl/device.wvd"));
        candidates.push(home.join(".config/omniget/device.wvd"));
    }
    if let Some(parent) = cdm_script.parent() {
        candidates.push(parent.join("device.wvd"));
    }
    candidates.push(PathBuf::from("device.wvd"));

    if let Some(path) = candidates.iter().find(|path| usable_wvd(path)) {
        return Ok(path.clone());
    }
    let searched = candidates
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    bail!(
        "No usable Widevine L3 .wvd device file was found. Select one in Settings or place device.wvd in ~/.config/omniget. Searched:\n{searched}"
    )
}

/// Verify both the pywidevine imports and the selected WVD payload before the
/// relatively expensive encrypted-HLS download starts.
pub async fn preflight_widevine(python: &Path, wvd: &Path) -> Result<()> {
    if !usable_wvd(wvd) {
        bail!(
            "Widevine device preflight failed: {} is missing or empty",
            wvd.display()
        );
    }
    let code = "import sys; from pywidevine.cdm import Cdm; from pywidevine.device import Device; from pywidevine.pssh import PSSH; Device.load(sys.argv[1])";
    let output = crate::core::process::command(python)
        .args(["-c", code])
        .arg(wvd)
        .output()
        .await
        .with_context(|| {
            format!(
                "Could not start Python Widevine preflight with {}",
                python.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let details = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!(
            "Widevine preflight failed with Python {} and device {} (exit {}): {}",
            python.display(),
            wvd.display(),
            output.status,
            if details.is_empty() {
                "pywidevine imports or Device.load failed without diagnostic output"
            } else {
                details
            }
        );
    }
    Ok(())
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
    if usable_media_file(&final_path) {
        let _ = progress.send(ProgressUpdate::percent(100.0)).await;
        return Ok(final_path);
    }
    if final_path.exists() {
        let _ = std::fs::remove_file(&final_path);
    }

    let wvd_path = resolve_widevine_device_path(wvd_path, &tools.cdm_script)
        .context("Widevine device preflight failed")?;
    preflight_widevine(&tools.python, &wvd_path)
        .await
        .context("Python/pywidevine/WVD preflight failed")?;

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
        let mut file = tokio::fs::File::open(&enc_video).await?;
        let mut head = vec![0_u8; 50 * 1024];
        let read = file.read(&mut head).await?;
        head.truncate(read);
        extract_pssh_b64(&head).context("Failed to extract Widevine PSSH from encrypted file")?
    };

    // 3. Fetch content keys via the pywidevine CDM helper — progress ~85%.
    let _ = progress.send(ProgressUpdate::percent(85.0)).await;
    let keys = get_content_keys(
        &pssh_b64,
        auth_token,
        license_url,
        Some(&wvd_path),
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
    if usable_media_file(&video_canonical) {
        return Ok((video_canonical, find_audio_file(tmp_dir)));
    }
    if video_canonical.exists() {
        let _ = std::fs::remove_file(&video_canonical);
    }

    let mut cmd = n_m3u8dl_command(hls_url, tmp_dir, base_name, quality, tool);
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
        let mut tail = VecDeque::new();
        if let Some(stderr) = stderr {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(id) = crate::core::log_hook::current_download_id() {
                    crate::core::log_hook::emit_log(id, &line);
                }
                remember_output_line(&mut tail, line);
            }
        }
        tail.into_iter().collect::<Vec<_>>().join("\n")
    });

    // Parse progress from stdout (N_m3u8DL-RE prints a `NN.NN%` token). Scale to
    // 0..80% — decrypt/mux occupy the remainder.
    let progress_tx = progress.clone();
    let stdout_task = tokio::spawn(async move {
        let mut tail = VecDeque::new();
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
                remember_output_line(&mut tail, line);
            }
        }
        tail.into_iter().collect::<Vec<_>>().join("\n")
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

    let stdout_out = stdout_task.await.unwrap_or_default();
    let stderr_out = stderr_task.await.unwrap_or_default();
    let diagnostic_tail = combined_output_tail(&stdout_out, &stderr_out);

    if !status.success() {
        bail!(
            "N_m3u8DL-RE failed (exit {status}) using {}: {diagnostic_tail}",
            tool.display()
        );
    }

    let video = if usable_media_file(&video_canonical) {
        video_canonical
    } else {
        // Fall back to a non-empty encrypted .mp4 N_m3u8DL-RE produced under a
        // language/track-suffixed name, but never accept a stale `.dec.mp4`.
        find_video_file(tmp_dir).ok_or_else(|| {
            anyhow!(
                "N_m3u8DL-RE exited successfully but produced no non-empty encrypted video in {}. Files: {}. Output: {}",
                tmp_dir.display(),
                describe_dir(tmp_dir),
                diagnostic_tail
            )
        })?
    };

    Ok((video, find_audio_file(tmp_dir)))
}

fn n_m3u8dl_command(
    hls_url: &str,
    tmp_dir: &Path,
    base_name: &str,
    quality: &str,
    tool: &Path,
) -> tokio::process::Command {
    let mut cmd = crate::core::process::command(tool);
    cmd.arg(hls_url)
        .args(["--save-dir", &tmp_dir.to_string_lossy()])
        .args(["--save-name", base_name])
        .arg("--no-date-info")
        .arg("--no-ansi-color")
        .arg("--disable-update-check")
        // Browser-ish UA so Udemy's WAF doesn't 403 the manifest.
        .args(["-H", &format!("User-Agent: {UDEMY_UA}")])
        .args(["--select-video", video_selector(quality)])
        .args(["--select-audio", "best"])
        // Spectre.Console reports a zero-width redirected terminal in some
        // desktop launches. N_m3u8DL-RE v0.6 then silently exited after parsing
        // the master playlist (or crashed in LiveRenderable). Supplying stable
        // dimensions keeps redirected progress rendering deterministic.
        .env("COLUMNS", "160")
        .env("LINES", "60")
        .env("TERM", "xterm-256color")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // N_m3u8DL-RE 0.5.1 resolves the raw metadata directory from the process
        // working directory instead of --save-dir. Installed macOS apps often
        // start in `/`, which turns a lecture title into a root-level path and
        // fails on the read-only system volume. Keep every side file inside the
        // already-created per-lecture temporary directory.
        .current_dir(tmp_dir);
    cmd
}

const OUTPUT_TAIL_LINES: usize = 24;

fn remember_output_line(tail: &mut VecDeque<String>, line: String) {
    if tail.len() == OUTPUT_TAIL_LINES {
        tail.pop_front();
    }
    tail.push_back(line);
}

fn combined_output_tail(stdout: &str, stderr: &str) -> String {
    let combined = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    if combined.is_empty() {
        "no diagnostic output".to_string()
    } else {
        combined
    }
}

fn usable_media_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn find_video_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            path.extension().and_then(|ext| ext.to_str()) == Some("mp4")
                && !name.ends_with(".dec.mp4")
                && usable_media_file(path)
        })
}

fn describe_dir(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return "<unreadable directory>".to_string();
    };
    let files = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<non-UTF8>");
            let len = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            format!("{name} ({len} bytes)")
        })
        .collect::<Vec<_>>();
    if files.is_empty() {
        "<empty>".to_string()
    } else {
        files.join(", ")
    }
}

/// Udemy CBCS audio is a separate `.m4a` file alongside the video.
fn find_audio_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("m4a") && usable_media_file(p))
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

    let output = cmd.output().await.with_context(|| {
        format!(
            "Failed to run mp4decrypt ({}) — is Bento4 installed?",
            mp4decrypt.display()
        )
    })?;
    if !output.status.success() {
        bail!(
            "mp4decrypt failed decrypting {} (exit {}): {}",
            enc.display(),
            output.status,
            combined_output_tail(
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr)
            )
        );
    }
    if !usable_media_file(dec) {
        bail!(
            "mp4decrypt exited successfully but produced no non-empty output at {}",
            dec.display()
        );
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

    let result = cmd
        .output()
        .await
        .context("Failed to run ffmpeg for mux/remux")?;
    if !result.status.success() {
        bail!(
            "ffmpeg mux failed for {} (exit {}): {}",
            video.display(),
            result.status,
            combined_output_tail(
                &String::from_utf8_lossy(&result.stdout),
                &String::from_utf8_lossy(&result.stderr)
            )
        );
    }
    if !usable_media_file(output) {
        bail!(
            "ffmpeg exited successfully but produced no non-empty muxed output at {}",
            output.display()
        );
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

    fn test_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "omniget-drm-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

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

    #[test]
    fn explicit_wvd_must_be_a_nonempty_file() {
        let dir = test_dir("wvd");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("widevine_cdm.py");
        let wvd = dir.join("device.wvd");

        let missing = resolve_widevine_device_path(Some(&wvd), &script).unwrap_err();
        assert!(format!("{missing:#}").contains("missing, empty, or not a file"));

        std::fs::write(&wvd, []).unwrap();
        assert!(resolve_widevine_device_path(Some(&wvd), &script).is_err());

        std::fs::write(&wvd, [1_u8, 2, 3]).unwrap();
        assert_eq!(
            resolve_widevine_device_path(Some(&wvd), &script).unwrap(),
            wvd
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn media_discovery_rejects_empty_and_decrypted_files() {
        let dir = test_dir("media");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.mp4"), []).unwrap();
        std::fs::write(dir.join("lecture.dec.mp4"), [1_u8]).unwrap();
        std::fs::write(dir.join("empty.m4a"), []).unwrap();
        assert_eq!(find_video_file(&dir), None);
        assert_eq!(find_audio_file(&dir), None);

        let video = dir.join("lecture.video.mp4");
        let audio = dir.join("lecture.en.m4a");
        std::fs::write(&video, [1_u8]).unwrap();
        std::fs::write(&audio, [1_u8]).unwrap();
        assert_eq!(find_video_file(&dir), Some(video));
        assert_eq!(find_audio_file(&dir), Some(audio));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_tool_output_has_an_actionable_placeholder() {
        assert_eq!(combined_output_tail("", ""), "no diagnostic output");
        assert_eq!(
            combined_output_tail("downloaded", "warning"),
            "downloaded | warning"
        );
    }

    #[test]
    fn n_m3u8dl_runs_from_its_writable_per_lecture_directory() {
        let dir = test_dir("nm3u8-cwd");
        let command = n_m3u8dl_command(
            "https://example.invalid/master.m3u8",
            &dir,
            "01 - Hello, World",
            "best",
            Path::new("/usr/bin/false"),
        );

        assert_eq!(command.as_std().get_current_dir(), Some(dir.as_path()));
    }
}
