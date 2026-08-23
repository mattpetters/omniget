use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;

use anyhow::anyhow;

pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists() || std::env::var("FLATPAK_ID").is_ok()
}

fn managed_bin_dir() -> Option<PathBuf> {
    Some(crate::core::paths::app_data_dir()?.join("bin"))
}

/// Verificação de integridade dos binários gerenciados.
///
/// Regra: **fail-closed**. Se a origem publica um hash e ele não confere, ou
/// se o hash esperado não pôde ser obtido de uma origem que sabidamente o
/// publica, o binário é descartado.
pub mod integrity {
    use anyhow::anyhow;

    pub fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }

    fn is_sha256(candidate: &str) -> bool {
        candidate.len() == 64 && candidate.bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// Lê um arquivo estilo `sha256sum`: `<64-hex><espaços>[*]<nome>`.
    /// Linha em branco ou malformada é pulada — não interrompe a busca.
    pub fn parse_sha256sums(text: &str, asset: &str) -> Option<String> {
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
                continue;
            };
            let name = name.trim_start_matches('*');
            if name == asset && is_sha256(hash) {
                return Some(hash.to_lowercase());
            }
        }
        None
    }

    /// Arquivo `.sha256sum` de asset único (o Deno publica um por asset).
    pub fn parse_single_sha256(text: &str) -> Option<String> {
        let first = text.split_whitespace().next()?;
        is_sha256(first).then(|| first.to_lowercase())
    }

    /// `digest` da API de releases do GitHub, no formato `sha256:<hex>`.
    pub fn parse_github_digest(digest: &str) -> Option<String> {
        let hex = digest.trim().strip_prefix("sha256:")?;
        is_sha256(hex).then(|| hex.to_lowercase())
    }

    pub fn verify_sha256(bytes: &[u8], expected: &str, label: &str) -> anyhow::Result<()> {
        let actual = sha256_hex(bytes);
        if actual == expected.to_lowercase() {
            tracing::info!("[integrity] {} verificado (sha256 confere)", label);
            return Ok(());
        }
        Err(anyhow!(
            "{}: hash nao confere — esperado {}, obtido {}. Download descartado.",
            label,
            expected,
            actual
        ))
    }

    /// Busca o hash esperado num arquivo de sums remoto. `Err` quando a origem
    /// publica sums mas não conseguimos obtê-los — o chamador deve abortar.
    pub async fn expected_from_sums_url(
        client: &reqwest::Client,
        sums_url: &str,
        asset: &str,
    ) -> anyhow::Result<String> {
        let response = client
            .get(sums_url)
            .send()
            .await
            .map_err(|e| anyhow!("nao foi possivel buscar {}: {}", sums_url, e))?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "nao foi possivel buscar {}: HTTP {}",
                sums_url,
                response.status()
            ));
        }
        let text = response
            .text()
            .await
            .map_err(|e| anyhow!("corpo ilegivel de {}: {}", sums_url, e))?;
        parse_sha256sums(&text, asset)
            .or_else(|| parse_single_sha256(&text))
            .ok_or_else(|| anyhow!("{} nao esta listado em {}", asset, sums_url))
    }
}

pub fn bin_name(tool: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}.exe", tool)
    } else {
        tool.to_string()
    }
}

/// Traduz o nome interno da ferramenta para o nome que a UI e o arquivo de
/// overrides usam. `find_tool` recebe "ffmpeg"; a tabela mostra "FFmpeg".
fn override_name(tool: &str) -> &str {
    match tool {
        "ffmpeg" => "FFmpeg",
        "yt-dlp" => "yt-dlp",
        other => other,
    }
}

pub async fn find_tool(tool: &str) -> Option<PathBuf> {
    find_tool_with_source(tool).await.map(|(path, _)| path)
}

/// Like `find_tool` but also returns a source tag: "flatpak", "managed", or "system".
/// Returns `None` if the tool is not found anywhere.
pub async fn find_tool_with_source(tool: &str) -> Option<(PathBuf, &'static str)> {
    // Issue #222. Antes de tudo: o caminho que o usuario escolheu. "custom" e
    // uma origem propria para a tabela de dependencias poder mostrar de onde o
    // binario veio, em vez de mentir "managed".
    if let Some(custom) = crate::core::binary_overrides::get(override_name(tool)) {
        return Some((custom, "custom"));
    }
    let name = bin_name(tool);
    let version_flag = version_flag_for(tool);

    #[cfg(target_os = "linux")]
    {
        let flatpak_path = PathBuf::from("/app/bin").join(&name);
        if flatpak_path.exists() {
            return Some((flatpak_path, "flatpak"));
        }
    }

    // Check managed bin dir
    if let Some(managed_path) = managed_bin_dir().map(|d| d.join(&name)) {
        if managed_path.exists() {
            let check = {
                let managed = managed_path.clone();
                let vf = version_flag.to_string();
                tokio::task::spawn_blocking(move || {
                    crate::core::process::std_command(&managed)
                        .arg(&vf)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .ok()
                        .filter(|s| s.success())
                })
                .await
                .ok()
                .flatten()
            };
            if check.is_some() {
                return Some((managed_path, "managed"));
            }
        }
    }

    // System PATH
    let result = {
        let name2 = name.clone();
        let vf = version_flag.to_string();
        tokio::task::spawn_blocking(move || {
            crate::core::process::std_command(&name2)
                .arg(&vf)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()
                .filter(|s| s.success())
        })
        .await
        .ok()
        .flatten()
    };

    if result.is_some() {
        let abs = resolve_absolute_path(&name);
        return Some((abs, "system"));
    }

    None
}

/// Resolve a bare binary name to its absolute path via `where` (Windows)
/// or `which` (Unix). Returns the original name as fallback.
fn resolve_absolute_path(bin_name: &str) -> PathBuf {
    let finder = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    if let Ok(output) = crate::core::process::std_command(finder)
        .arg(bin_name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                let path = line.trim();
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
        }
    }
    PathBuf::from(bin_name)
}

fn version_flag_for(tool: &str) -> &'static str {
    match tool {
        "ffmpeg" | "ffprobe" => "-version",
        _ => "--version",
    }
}

/// Read the version string from a tool binary at a known path.
/// This avoids re-running tool discovery when the path is already known.
pub async fn check_version_at_path(path: &std::path::Path, tool: &str) -> Option<String> {
    let version_flag = version_flag_for(tool);
    let path = path.to_path_buf();
    let vf = version_flag.to_string();
    let output = tokio::task::spawn_blocking(move || {
        crate::core::process::std_command(&path)
            .arg(&vf)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    })
    .await
    .ok()?
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("");

    if tool == "ffmpeg" || tool == "ffprobe" {
        first_line.split_whitespace().nth(2).map(|s| s.to_string())
    } else if tool == "aria2c" {
        first_line.split_whitespace().nth(2).map(|s| s.to_string())
    } else {
        Some(first_line.trim().to_string())
    }
}

pub async fn check_version(tool: &str) -> Option<String> {
    let _timer_start = std::time::Instant::now();
    let path = find_tool(tool).await?;
    let result = check_version_at_path(&path, tool).await;
    tracing::debug!(
        "[perf] check_version({}) took {:?}",
        tool,
        _timer_start.elapsed()
    );
    result
}

pub fn replace_managed_binary(
    temp: &std::path::Path,
    target: &std::path::Path,
) -> anyhow::Result<()> {
    if !target.exists() {
        std::fs::rename(temp, target)
            .map_err(|e| anyhow!("Failed to move {} into place: {}", target.display(), e))?;
        return Ok(());
    }

    if cfg!(windows) {
        let file_name = target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("binary")
            .to_string();
        let old = target.with_file_name(format!("{}.old", file_name));
        let _ = std::fs::remove_file(&old);
        if let Err(e) = std::fs::rename(target, &old) {
            let _ = std::fs::remove_file(temp);
            return Err(anyhow!(
                "{} is in use by another process ({}). Wait for active downloads to finish or cancel them, then try again.",
                file_name,
                e
            ));
        }
        if let Err(e) = std::fs::rename(temp, target) {
            let _ = std::fs::rename(&old, target);
            let _ = std::fs::remove_file(temp);
            return Err(anyhow!("Failed to replace {}: {}", file_name, e));
        }
        let _ = std::fs::remove_file(&old);
        Ok(())
    } else {
        std::fs::rename(temp, target)
            .map_err(|e| anyhow!("Failed to replace {}: {}", target.display(), e))?;
        Ok(())
    }
}

pub async fn update_ffmpeg() -> anyhow::Result<PathBuf> {
    if is_flatpak() {
        return Err(anyhow!(
            "FFmpeg is provided by the Flatpak runtime and cannot be updated from inside the app"
        ));
    }
    let path = download_ffmpeg().await?;
    crate::core::ytdlp::reset_ffmpeg_location_cache();
    Ok(path)
}

pub async fn ensure_ffmpeg() -> anyhow::Result<PathBuf> {
    // Always ensure the managed binary exists — the standalone yt-dlp.exe
    // cannot discover system FFmpeg from PATH.
    if !is_flatpak() {
        let managed = managed_bin_dir().map(|d| d.join(bin_name("ffmpeg")));
        if managed.as_ref().map_or(true, |p| !p.exists()) {
            if let Ok(path) = download_ffmpeg().await {
                crate::core::ytdlp::reset_ffmpeg_location_cache();
                return Ok(path);
            }
        }
    }

    if let Some(path) = find_tool("ffmpeg").await {
        return Ok(path);
    }
    if is_flatpak() {
        return Err(anyhow!("FFmpeg not found in Flatpak sandbox"));
    }
    let path = download_ffmpeg().await?;
    crate::core::ytdlp::reset_ffmpeg_location_cache();
    Ok(path)
}

async fn download_ffmpeg() -> anyhow::Result<PathBuf> {
    let bin_dir = managed_bin_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
    std::fs::create_dir_all(&bin_dir)?;

    let ffmpeg_name = bin_name("ffmpeg");
    let ffprobe_name = bin_name("ffprobe");
    let ffmpeg_target = bin_dir.join(&ffmpeg_name);

    let downloads = ffmpeg_download_urls();

    let client = crate::core::http_client::apply_global_proxy(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    for (url, archive_type) in downloads {
        tracing::info!("Downloading FFmpeg component from {}", url);
        let response = client.get(url).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to download FFmpeg from {}: HTTP {}",
                url,
                response.status()
            ));
        }

        let temp_path = bin_dir.join(".ffmpeg_download.tmp");
        let bytes = response.bytes().await?;
        let data = bytes.to_vec();
        let temp_clone = temp_path.clone();
        tokio::task::spawn_blocking(move || std::fs::write(&temp_clone, &data))
            .await
            .map_err(|e| anyhow!("spawn_blocking failed: {}", e))??;

        let file_size = std::fs::metadata(&temp_path)?.len();
        if file_size < 1_000_000 {
            let _ = std::fs::remove_file(&temp_path);
            return Err(anyhow!(
                "Downloaded file from {} is too small ({}B) — likely an error page",
                url,
                file_size
            ));
        }

        match archive_type {
            ArchiveType::Zip => {
                extract_zip_ffmpeg(&temp_path, &bin_dir, &ffmpeg_name, &ffprobe_name).await?
            }
            ArchiveType::TarXz => {
                extract_tar_xz_ffmpeg(&temp_path, &bin_dir, &ffmpeg_name, &ffprobe_name).await?
            }
        }

        let _ = std::fs::remove_file(&temp_path);
    }

    for name in [&ffmpeg_name, &ffprobe_name] {
        let staged = bin_dir.join(format!("{}.new", name));
        if staged.exists() {
            replace_managed_binary(&staged, &bin_dir.join(name))?;
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        let _ = std::fs::set_permissions(&ffmpeg_target, perms.clone());
        let ffprobe_path = bin_dir.join(&ffprobe_name);
        if ffprobe_path.exists() {
            let _ = std::fs::set_permissions(&ffprobe_path, perms);
        }
    }

    #[cfg(target_os = "macos")]
    {
        let ffmpeg_mac = ffmpeg_target.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            crate::core::process::std_command("xattr")
                .args(["-d", "com.apple.quarantine"])
                .arg(&ffmpeg_mac)
                .output()
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|r| r)
        {
            tracing::warn!("Failed to remove quarantine from ffmpeg: {}", e);
        }
        let ffprobe_path = bin_dir.join(&ffprobe_name);
        if ffprobe_path.exists() {
            let ffprobe_mac = ffprobe_path.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                crate::core::process::std_command("xattr")
                    .args(["-d", "com.apple.quarantine"])
                    .arg(&ffprobe_mac)
                    .output()
            })
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))
            .and_then(|r| r)
            {
                tracing::warn!("Failed to remove quarantine from ffprobe: {}", e);
            }
        }
    }

    if !ffmpeg_target.exists() {
        return Err(anyhow!("FFmpeg binary not found after extraction"));
    }

    let verify = {
        let target = ffmpeg_target.clone();
        tokio::task::spawn_blocking(move || {
            crate::core::process::std_command(&target)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        })
        .await
        .map_err(|e| anyhow!("spawn_blocking failed: {}", e))?
    };
    match verify {
        Ok(s) if s.success() => {}
        Ok(s) => {
            return Err(anyhow!(
                "FFmpeg installed but failed to execute (exit code {})",
                s
            ))
        }
        Err(e) => return Err(anyhow!("FFmpeg installed but failed to execute: {}", e)),
    }

    tracing::info!("FFmpeg installed to {}", ffmpeg_target.display());
    Ok(ffmpeg_target)
}

enum ArchiveType {
    Zip,
    TarXz,
}

fn ffmpeg_download_urls() -> Vec<(&'static str, ArchiveType)> {
    if cfg!(target_os = "windows") {
        vec![(
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
            ArchiveType::Zip,
        )]
    } else if cfg!(target_os = "macos") {
        vec![
            (
                "https://evermeet.cx/ffmpeg/getrelease/zip",
                ArchiveType::Zip,
            ),
            (
                "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip",
                ArchiveType::Zip,
            ),
        ]
    } else if cfg!(target_arch = "aarch64") {
        vec![(
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linuxarm64-gpl.tar.xz",
            ArchiveType::TarXz,
        )]
    } else {
        vec![(
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz",
            ArchiveType::TarXz,
        )]
    }
}

async fn extract_zip_ffmpeg(
    archive_path: &std::path::Path,
    bin_dir: &std::path::Path,
    ffmpeg_name: &str,
    ffprobe_name: &str,
) -> anyhow::Result<()> {
    let archive_path = archive_path.to_path_buf();
    let bin_dir = bin_dir.to_path_buf();
    let ffmpeg_name = ffmpeg_name.to_string();
    let ffprobe_name = ffprobe_name.to_string();

    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive_path)
            .map_err(|e| anyhow!("Failed to open archive: {}", e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| anyhow!("Failed to open zip: {}", e))?;

        let targets = [ffmpeg_name.as_str(), ffprobe_name.as_str()];

        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| anyhow!("Failed to read zip entry: {}", e))?;

            let name = entry.name().to_string();
            for target in &targets {
                if name.ends_with(target) {
                    let dest = bin_dir.join(format!("{}.new", target));
                    let mut out = std::fs::File::create(&dest)?;
                    std::io::copy(&mut entry, &mut out)?;
                    break;
                }
            }
        }

        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|e| anyhow!("Spawn blocking failed: {}", e))??;

    Ok(())
}

async fn extract_tar_xz_ffmpeg(
    archive_path: &std::path::Path,
    bin_dir: &std::path::Path,
    ffmpeg_name: &str,
    ffprobe_name: &str,
) -> anyhow::Result<()> {
    let archive_path = archive_path.to_path_buf();
    let bin_dir = bin_dir.to_path_buf();
    let ffmpeg_name = ffmpeg_name.to_string();
    let ffprobe_name = ffprobe_name.to_string();

    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive_path)
            .map_err(|e| anyhow!("Failed to open archive: {}", e))?;
        let decompressor = xz2::read::XzDecoder::new(file);
        let mut archive = tar::Archive::new(decompressor);
        let targets = [ffmpeg_name.as_str(), ffprobe_name.as_str()];

        for entry_result in archive
            .entries()
            .map_err(|e| anyhow!("Failed to read tar entries: {}", e))?
        {
            let mut entry = entry_result.map_err(|e| anyhow!("Failed to read tar entry: {}", e))?;
            let path = entry
                .path()
                .map_err(|e| anyhow!("Failed to read entry path: {}", e))?;
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            for target in &targets {
                if file_name == *target {
                    let dest = bin_dir.join(format!("{}.new", target));
                    let mut out = std::fs::File::create(&dest)?;
                    std::io::copy(&mut entry, &mut out)?;
                    break;
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|e| anyhow!("Spawn blocking failed: {}", e))??;
    Ok(())
}

// --- aria2c ---

// --- deno (JS runtime for yt-dlp nsig challenge) ---

/// Ensures a JavaScript runtime is available for yt-dlp's YouTube nsig
/// challenge solver. Checks for any existing runtime first (Node.js, Deno,
/// Bun), then auto-downloads Deno if none is found.
pub async fn ensure_js_runtime() -> Option<PathBuf> {
    // Check system-installed runtimes first.
    for tool in &["deno", "node", "bun"] {
        if let Some(path) = find_tool(tool).await {
            return Some(path);
        }
    }

    // Check well-known install locations on Windows.
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Program Files (x86)\nodejs\node.exe",
        ];
        for path in &candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
    }

    // No runtime found — download Deno (recommended by yt-dlp, fast, single binary).
    match download_deno().await {
        Ok(path) => {
            crate::core::ytdlp::reset_js_runtime_cache();
            Some(path)
        }
        Err(e) => {
            tracing::warn!("Failed to download Deno JS runtime: {}", e);
            None
        }
    }
}

async fn download_deno() -> anyhow::Result<PathBuf> {
    let bin_dir = managed_bin_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
    std::fs::create_dir_all(&bin_dir)?;

    let deno_name = bin_name("deno");
    let deno_target = bin_dir.join(&deno_name);

    if deno_target.exists() {
        return Ok(deno_target);
    }

    let url = if cfg!(target_os = "windows") {
        "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-apple-darwin.zip"
        } else {
            "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-apple-darwin.zip"
        }
    } else if cfg!(target_arch = "aarch64") {
        "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-unknown-linux-gnu.zip"
    } else {
        "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip"
    };

    tracing::info!("Downloading Deno JS runtime from {}", url);

    let client = crate::core::http_client::apply_global_proxy(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download Deno: HTTP {}",
            response.status()
        ));
    }

    let bytes = response.bytes().await?;
    let data = bytes.to_vec();
    let bin_dir_clone = bin_dir.clone();
    let deno_name_clone = deno_name.clone();

    tokio::task::spawn_blocking(move || {
        let cursor = std::io::Cursor::new(&data);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| anyhow!("Failed to open Deno zip: {}", e))?;

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| anyhow!("Failed to read zip entry: {}", e))?;

            let name = file.name().to_string();
            if name.ends_with(&deno_name_clone) || name == "deno" || name == "deno.exe" {
                let dest = bin_dir_clone.join(&deno_name_clone);
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut buf)?;
                std::fs::write(&dest, &buf)?;
                break;
            }
        }

        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|e| anyhow!("Spawn blocking failed: {}", e))??;

    if !deno_target.exists() {
        return Err(anyhow!("Deno binary not found after extraction"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&deno_target, std::fs::Permissions::from_mode(0o755));
    }

    #[cfg(target_os = "macos")]
    {
        let deno_mac = deno_target.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::core::process::std_command("xattr")
                .args(["-d", "com.apple.quarantine"])
                .arg(&deno_mac)
                .output()
        })
        .await;
    }

    tracing::info!("Deno installed to {}", deno_target.display());
    Ok(deno_target)
}

pub async fn ensure_gallerydl() -> Option<PathBuf> {
    if let Some(path) = find_tool("gallery-dl").await {
        return Some(path);
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        match download_gallerydl().await {
            Ok(path) => return Some(path),
            Err(e) => tracing::warn!("Failed to download gallery-dl: {}", e),
        }
    }

    None
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
async fn download_gallerydl() -> anyhow::Result<PathBuf> {
    let bin_dir = managed_bin_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
    std::fs::create_dir_all(&bin_dir)?;

    let target = bin_dir.join(bin_name("gallery-dl"));

    #[cfg(target_os = "windows")]
    let url = "https://github.com/mikf/gallery-dl/releases/latest/download/gallery-dl.exe";
    #[cfg(target_os = "linux")]
    let url = "https://github.com/mikf/gallery-dl/releases/latest/download/gallery-dl.bin";

    let client = crate::core::http_client::apply_global_proxy(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(180))
        .build()?;

    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download gallery-dl: HTTP {}",
            response.status()
        ));
    }

    let bytes = response.bytes().await?;
    let data = bytes.to_vec();
    let target_clone = target.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        std::fs::write(&target_clone, &data)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&target_clone)?.permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&target_clone, perm)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("Spawn blocking failed: {}", e))??;

    if !target.exists() {
        return Err(anyhow!("gallery-dl binary not found after download"));
    }

    Ok(target)
}

pub async fn ensure_aria2c() -> Option<PathBuf> {
    if let Some(path) = find_tool("aria2c").await {
        return Some(path);
    }

    // Auto-download only on Windows
    #[cfg(target_os = "windows")]
    {
        match download_aria2c().await {
            Ok(path) => return Some(path),
            Err(e) => {
                tracing::warn!("Failed to download aria2c: {}", e);
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
async fn download_aria2c() -> anyhow::Result<PathBuf> {
    let bin_dir = managed_bin_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
    std::fs::create_dir_all(&bin_dir)?;

    let aria2c_name = bin_name("aria2c");
    let aria2c_target = bin_dir.join(&aria2c_name);

    let url = "https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip";

    let client = crate::core::http_client::apply_global_proxy(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download aria2c: HTTP {}",
            response.status()
        ));
    }

    let bytes = response.bytes().await?;

    let data = bytes.to_vec();
    let bin_dir_clone = bin_dir.clone();
    let aria2c_name_clone = aria2c_name.clone();

    tokio::task::spawn_blocking(move || {
        let cursor = std::io::Cursor::new(&data);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| anyhow!("Failed to open aria2c zip: {}", e))?;

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| anyhow!("Failed to read zip entry: {}", e))?;

            let name = file.name().to_string();
            if name.ends_with(&aria2c_name_clone) {
                let dest = bin_dir_clone.join(&aria2c_name_clone);
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut buf)?;
                std::fs::write(&dest, &buf)?;
                break;
            }
        }

        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|e| anyhow!("Spawn blocking failed: {}", e))??;

    if !aria2c_target.exists() {
        return Err(anyhow!("aria2c binary not found after extraction"));
    }

    Ok(aria2c_target)
}

#[cfg(test)]
mod integrity_tests {
    use super::integrity::*;

    const VAZIO: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn sha256_de_vetor_conhecido() {
        assert_eq!(sha256_hex(b""), VAZIO);
    }

    #[test]
    fn verify_sha256_recusa_binario_adulterado() {
        assert!(verify_sha256(b"", VAZIO, "t").is_ok());
        assert!(verify_sha256(b"", &VAZIO.to_uppercase(), "t").is_ok());
        let erro = verify_sha256(b"binario trocado", VAZIO, "yt-dlp").unwrap_err();
        assert!(erro.to_string().contains("yt-dlp"));
    }

    #[test]
    fn linha_em_branco_nao_interrompe_a_busca_no_sums() {
        // Regressao: a versao anterior usava `?` dentro do laco, entao a
        // primeira linha vazia ou de um token so abortava a funcao inteira e o
        // asset seguinte nunca era encontrado -> instalacao sem verificacao.
        let sums = format!(
            "\n\ncomentario\n{}  yt-dlp.exe\n\n{}  yt-dlp_macos\n",
            VAZIO,
            "1".repeat(64)
        );
        assert_eq!(
            parse_sha256sums(&sums, "yt-dlp.exe").as_deref(),
            Some(VAZIO)
        );
        assert_eq!(
            parse_sha256sums(&sums, "yt-dlp_macos").as_deref(),
            Some("1".repeat(64).as_str())
        );
        assert_eq!(parse_sha256sums(&sums, "ausente"), None);
    }

    #[test]
    fn sums_aceita_marcador_binario_e_recusa_hash_invalido() {
        let bin = format!("{} *ffmpeg.zip\n", VAZIO);
        assert_eq!(parse_sha256sums(&bin, "ffmpeg.zip").as_deref(), Some(VAZIO));
        assert_eq!(parse_sha256sums("abc  yt-dlp.exe\n", "yt-dlp.exe"), None);
        assert_eq!(
            parse_sha256sums(&format!("{}  yt-dlp.exe\n", "z".repeat(64)), "yt-dlp.exe"),
            None
        );
    }

    #[test]
    fn digest_da_api_do_github() {
        assert_eq!(
            parse_github_digest(&format!("sha256:{}", VAZIO)).as_deref(),
            Some(VAZIO)
        );
        assert_eq!(parse_github_digest(VAZIO), None);
        assert_eq!(parse_github_digest("sha512:abc"), None);
    }
}

/// N_m3u8DL-RE is intentionally pinned. Its beta releases are not API-stable:
/// v0.6.0 (20260628) regressed redirected-console downloads and could exit 0
/// immediately after parsing a valid Udemy master playlist. Keep this version
/// coupled to the invocation/output validation in `core::udemy::drm`.
const N_M3U8DL_RE_TAG: &str = "v0.5.1-beta";
const N_M3U8DL_RE_VERSION: &str = "0.5.1";

/// The Python dependency is installed into an isolated managed venv so the
/// desktop app does not depend on (or mutate) the user's global Python site.
const PYWIDEVINE_VERSION: &str = "1.9.0";

/// Pinned Bento4 build (the bok.net binaries are versioned by build number and
/// have no "latest" API; this build is verified-good across platforms).
const BENTO4_BUILD: &str = "1-6-0-641";

fn n_m3u8dl_install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn pywidevine_install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}
#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) {}

#[cfg(target_os = "macos")]
async fn dequarantine(path: &std::path::Path) {
    let p = path.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || {
        crate::core::process::std_command("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(&p)
            .output()
    })
    .await;
}
#[cfg(not(target_os = "macos"))]
async fn dequarantine(_path: &std::path::Path) {}

fn drm_http_client() -> anyhow::Result<reqwest::Client> {
    Ok(
        crate::core::http_client::apply_global_proxy(reqwest::Client::builder())
            .timeout(std::time::Duration::from_secs(300))
            .build()?,
    )
}

fn n_m3u8dl_asset() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") {
            (
                "N_m3u8DL-RE_v0.5.1-beta_win-arm64_20251029.zip",
                "eb7f645399ae4b67101070d14f05c8af905ce94bc4d83cb4323456c8f267d53e",
            )
        } else {
            (
                "N_m3u8DL-RE_v0.5.1-beta_win-x64_20251029.zip",
                "7e2e5e64c2893aef118febc2213cd43706fce8bd0ffd5e8dd94024d79ea365e9",
            )
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            (
                "N_m3u8DL-RE_v0.5.1-beta_osx-arm64_20251029.tar.gz",
                "537866d7d03c9aed04c910014bceae26a3db494c1d1edae9c59ddaaa29b0a1c7",
            )
        } else {
            (
                "N_m3u8DL-RE_v0.5.1-beta_osx-x64_20251029.tar.gz",
                "fb0d9fd6c18b08a5c55e49f60d3c219471196bd05bf15e58f318a44da500f65a",
            )
        }
    } else if cfg!(target_arch = "aarch64") {
        (
            "N_m3u8DL-RE_v0.5.1-beta_linux-arm64_20251029.tar.gz",
            "b9cce9978e94fd8ce509ee86a6543cccffeb0ee5b7b7aeff1314104265ac65ad",
        )
    } else {
        (
            "N_m3u8DL-RE_v0.5.1-beta_linux-x64_20251029.tar.gz",
            "2acce91b64af3ee676a32d1002e1382840d81f430e1b7f8d5b151ce1eb6fb590",
        )
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn n_m3u8dl_version_is_pinned(output: &str) -> bool {
    output.lines().map(str::trim).any(|line| {
        line == N_M3U8DL_RE_VERSION || line.starts_with(&format!("{N_M3U8DL_RE_VERSION}+"))
    })
}

async fn n_m3u8dl_binary_is_pinned(path: &std::path::Path) -> bool {
    let output = match crate::core::process::command(path)
        .arg("--version")
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    n_m3u8dl_version_is_pinned(&combined)
}

/// Ensure a verified N_m3u8DL-RE build is available. We replace unpinned
/// managed builds rather than following `releases/latest`, since beta updates
/// have previously broken valid encrypted-HLS downloads without a non-zero
/// process status.
pub async fn ensure_n_m3u8dl_re() -> anyhow::Result<PathBuf> {
    let bin_dir = managed_bin_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
    std::fs::create_dir_all(&bin_dir)?;
    let target = bin_dir.join(bin_name("N_m3u8DL-RE"));

    if target.exists() && n_m3u8dl_binary_is_pinned(&target).await {
        return Ok(target);
    }
    if !target.exists() {
        if let Some(system) = find_tool("N_m3u8DL-RE").await {
            if n_m3u8dl_binary_is_pinned(&system).await {
                return Ok(system);
            }
        }
    }

    let _guard = n_m3u8dl_install_lock().lock().await;
    if target.exists() && n_m3u8dl_binary_is_pinned(&target).await {
        return Ok(target);
    }

    let (asset_name, expected_sha256) = n_m3u8dl_asset();
    let url = format!(
        "https://github.com/nilaoda/N_m3u8DL-RE/releases/download/{N_M3U8DL_RE_TAG}/{asset_name}"
    );
    tracing::info!(
        "Downloading pinned N_m3u8DL-RE {} from {}",
        N_M3U8DL_RE_TAG,
        url
    );
    let bytes = drm_http_client()?
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec();
    let actual_sha256 = sha256_hex(&bytes);
    if actual_sha256 != expected_sha256 {
        return Err(anyhow!(
            "N_m3u8DL-RE checksum mismatch (expected {expected_sha256}, got {actual_sha256}) — refusing to install"
        ));
    }

    let staged = bin_dir.join(format!(".{}.new", bin_name("N_m3u8DL-RE")));
    let _ = std::fs::remove_file(&staged);
    let member = bin_name("N_m3u8DL-RE");
    let staged_clone = staged.clone();
    let is_zip = asset_name.ends_with(".zip");
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        if is_zip {
            extract_member_from_zip(&bytes, &member, &staged_clone)
        } else {
            extract_member_from_tar_gz(&bytes, &member, &staged_clone)
        }
    })
    .await
    .map_err(|error| anyhow!("N_m3u8DL-RE extract task failed: {error}"))??;

    if !staged.exists() {
        return Err(anyhow!("N_m3u8DL-RE binary not found after extraction"));
    }
    make_executable(&staged);
    dequarantine(&staged).await;
    if !n_m3u8dl_binary_is_pinned(&staged).await {
        let _ = std::fs::remove_file(&staged);
        return Err(anyhow!(
            "Downloaded N_m3u8DL-RE did not report expected version {N_M3U8DL_RE_VERSION}"
        ));
    }
    replace_managed_binary(&staged, &target)?;
    tracing::info!(
        "N_m3u8DL-RE {} installed to {}",
        N_M3U8DL_RE_TAG,
        target.display()
    );
    Ok(target)
}

fn managed_pywidevine_python(venv: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

async fn python_has_managed_pywidevine(python: &std::path::Path) -> bool {
    let check = format!(
        "import importlib.metadata; from pywidevine.cdm import Cdm; from pywidevine.device import Device; from pywidevine.pssh import PSSH; assert importlib.metadata.version('pywidevine') == '{PYWIDEVINE_VERSION}'"
    );
    crate::core::process::command(python)
        .args(["-c", &check])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

fn process_failure_tail(output: &std::process::Output) -> String {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Ensure an isolated Python runtime containing the pywidevine version tested
/// with OmniGet's CDM helper. The global Python environment is never modified.
pub async fn ensure_pywidevine_python() -> anyhow::Result<PathBuf> {
    let bin_dir = managed_bin_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
    std::fs::create_dir_all(&bin_dir)?;
    let venv = bin_dir.join(format!("pywidevine-{PYWIDEVINE_VERSION}"));
    let managed_python = managed_pywidevine_python(&venv);
    if managed_python.exists() && python_has_managed_pywidevine(&managed_python).await {
        return Ok(managed_python);
    }

    let _guard = pywidevine_install_lock().lock().await;
    if managed_python.exists() && python_has_managed_pywidevine(&managed_python).await {
        return Ok(managed_python);
    }

    let mut base_python = None;
    for candidate in ["python3", "python"] {
        if let Some(path) = find_tool(candidate).await {
            base_python = Some(path);
            break;
        }
    }
    let base_python = base_python.ok_or_else(|| {
        anyhow!(
            "Python 3 was not found. Udemy Widevine downloads need Python 3 to create OmniGet's managed pywidevine runtime"
        )
    })?;

    if venv.exists() {
        std::fs::remove_dir_all(&venv).map_err(|error| {
            anyhow!(
                "Could not repair managed pywidevine runtime at {}: {error}",
                venv.display()
            )
        })?;
    }
    tracing::info!(
        "Creating managed pywidevine {} runtime with {}",
        PYWIDEVINE_VERSION,
        base_python.display()
    );
    let create = crate::core::process::command(&base_python)
        .args(["-m", "venv"])
        .arg(&venv)
        .output()
        .await
        .map_err(|error| {
            anyhow!(
                "Failed to run {} -m venv for the managed pywidevine runtime: {error}",
                base_python.display()
            )
        })?;
    if !create.status.success() {
        return Err(anyhow!(
            "Python could not create OmniGet's managed pywidevine runtime (exit {}). Install Python's venv support and retry. Details: {}",
            create.status,
            process_failure_tail(&create)
        ));
    }

    let requirement = format!("pywidevine=={PYWIDEVINE_VERSION}");
    let install = crate::core::process::command(&managed_python)
        .args([
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--no-input",
            &requirement,
        ])
        .output()
        .await
        .map_err(|error| {
            anyhow!("Failed to start pip for the managed pywidevine runtime: {error}")
        })?;
    if !install.status.success() {
        return Err(anyhow!(
            "Could not install {requirement} into OmniGet's managed runtime (exit {}). Check the network connection and retry. Details: {}",
            install.status,
            process_failure_tail(&install)
        ));
    }
    if !python_has_managed_pywidevine(&managed_python).await {
        return Err(anyhow!(
            "Managed Python runtime was created, but pywidevine {PYWIDEVINE_VERSION} failed its import check at {}",
            managed_python.display()
        ));
    }
    tracing::info!(
        "Managed pywidevine {} runtime ready at {}",
        PYWIDEVINE_VERSION,
        managed_python.display()
    );
    Ok(managed_python)
}

/// Ensure `mp4decrypt` (Bento4) is available, auto-downloading a pinned Bento4
/// build for this platform if missing.
pub async fn ensure_mp4decrypt() -> anyhow::Result<PathBuf> {
    if let Some(path) = find_tool("mp4decrypt").await {
        return Ok(path);
    }
    let bin_dir = managed_bin_dir().ok_or_else(|| anyhow!("Could not determine data directory"))?;
    std::fs::create_dir_all(&bin_dir)?;
    let target = bin_dir.join(bin_name("mp4decrypt"));
    if target.exists() {
        return Ok(target);
    }

    let plat = if cfg!(target_os = "windows") {
        Some("x86_64-microsoft-win32")
    } else if cfg!(target_os = "macos") {
        Some("universal-apple-macosx")
    } else if cfg!(target_arch = "x86_64") {
        Some("x86_64-unknown-linux")
    } else {
        None
    };
    let plat = plat.ok_or_else(|| {
        anyhow!("No prebuilt Bento4 mp4decrypt for this platform — install it via your package manager (e.g. `brew install bento4`)")
    })?;

    let url = format!("https://www.bok.net/Bento4/binaries/Bento4-SDK-{BENTO4_BUILD}.{plat}.zip");
    tracing::info!("Downloading Bento4 (mp4decrypt) from {}", url);
    let client = drm_http_client()?;
    let bytes = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec();

    // The zip nests files under `Bento4-SDK-.../bin/mp4decrypt`.
    let member = format!("bin/{}", bin_name("mp4decrypt"));
    let target_clone = target.clone();
    tokio::task::spawn_blocking(move || extract_member_from_zip(&bytes, &member, &target_clone))
        .await
        .map_err(|e| anyhow!("extract task failed: {e}"))??;

    if !target.exists() {
        return Err(anyhow!("mp4decrypt binary not found after extraction"));
    }
    make_executable(&target);
    dequarantine(&target).await;
    tracing::info!("mp4decrypt installed to {}", target.display());
    Ok(target)
}

/// Extract the first archive member whose path ends with `member_suffix` into
/// `dest` (a zip blob held in memory).
fn extract_member_from_zip(
    data: &[u8],
    member_suffix: &str,
    dest: &std::path::Path,
) -> anyhow::Result<()> {
    let cursor = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| anyhow!("open zip: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| anyhow!("zip entry: {e}"))?;
        let name = entry.name().replace('\\', "/");
        if name.ends_with(member_suffix) || name.ends_with(&format!("/{member_suffix}")) {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf)?;
            std::fs::write(dest, &buf)?;
            return Ok(());
        }
    }
    Err(anyhow!("member '{member_suffix}' not found in zip"))
}

/// Extract the first tar.gz member whose filename equals `member_name` into `dest`.
fn extract_member_from_tar_gz(
    data: &[u8],
    member_name: &str,
    dest: &std::path::Path,
) -> anyhow::Result<()> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(data));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().map_err(|e| anyhow!("tar entries: {e}"))? {
        let mut entry = entry.map_err(|e| anyhow!("tar entry: {e}"))?;
        let path = entry.path().map_err(|e| anyhow!("tar path: {e}"))?;
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname == member_name {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf)?;
            std::fs::write(dest, &buf)?;
            return Ok(());
        }
    }
    Err(anyhow!("member '{member_name}' not found in tar.gz"))
}

#[cfg(test)]
mod udemy_drm_dependency_tests {
    use super::*;

    #[test]
    fn accepts_only_the_pinned_n_m3u8dl_version() {
        assert!(n_m3u8dl_version_is_pinned(
            "0.5.1+c1f6db5639397dde362c31b31eebd88c796c90da"
        ));
        assert!(n_m3u8dl_version_is_pinned("0.5.1"));
        assert!(!n_m3u8dl_version_is_pinned(
            "0.6.0+df70f0b3da0c630bd413bf617e758051f6b64757"
        ));
        assert!(!n_m3u8dl_version_is_pinned("N_m3u8DL-RE 0.5.10"));
    }

    #[test]
    fn pinned_asset_has_a_published_sha256() {
        let (asset, digest) = n_m3u8dl_asset();
        assert!(asset.contains("v0.5.1-beta"));
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn managed_python_path_matches_platform_venv_layout() {
        let path = managed_pywidevine_python(std::path::Path::new("runtime"));
        if cfg!(target_os = "windows") {
            assert!(path.ends_with(std::path::Path::new("Scripts/python.exe")));
        } else {
            assert!(path.ends_with(std::path::Path::new("bin/python")));
        }
    }

    #[tokio::test]
    #[ignore = "downloads pinned DRM dependencies into a temporary data directory"]
    async fn provisions_verified_managed_drm_runtime() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let data_dir = std::env::temp_dir().join(format!(
            "omniget-managed-drm-{}-{nonce}",
            std::process::id()
        ));
        let previous = std::env::var_os("OMNIGET_DATA_DIR");
        std::env::set_var("OMNIGET_DATA_DIR", &data_dir);

        let n_m3u8dl = ensure_n_m3u8dl_re().await.unwrap();
        assert!(n_m3u8dl_binary_is_pinned(&n_m3u8dl).await);
        let python = ensure_pywidevine_python().await.unwrap();
        assert!(python_has_managed_pywidevine(&python).await);

        if let Some(previous) = previous {
            std::env::set_var("OMNIGET_DATA_DIR", previous);
        } else {
            std::env::remove_var("OMNIGET_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
