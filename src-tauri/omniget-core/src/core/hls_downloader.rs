use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use m3u8_rs::{parse_master_playlist, parse_media_playlist, MasterPlaylist, VariantStream};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::models::progress::ProgressUpdate;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub struct HlsDownloadResult {
    pub path: PathBuf,
    pub file_size: u64,
    pub segments: usize,
    pub protected_passthrough: bool,
    pub protected_media: Option<ProtectedMediaInfo>,
    pub protection_sidecar_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedHlsPolicy {
    Fail,
    SaveEncrypted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtectedMediaInfo {
    pub marker: String,
    pub encrypted: bool,
    pub encryption_method: String,
    pub source_url: String,
    pub key_uri: Option<String>,
    pub key_format: Option<String>,
    pub decryption_status: String,
    pub note: String,
}

pub struct HlsDownloader {
    client: Client,
    user_agent_override: Option<String>,
    /// Optional rich progress channel; receives percent (completed/total
    /// segments) plus accumulated downloaded bytes as segments finish.
    progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
    protected_hls_policy: ProtectedHlsPolicy,
}

impl Default for HlsDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl HlsDownloader {
    pub fn new() -> Self {
        let builder = crate::core::http_client::apply_global_proxy(
            Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(300))
                .pool_max_idle_per_host(50)
                .pool_idle_timeout(Duration::from_secs(30)),
        );
        let client = match builder.build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("HLS client build failed, falling back to default: {}", e);
                Client::new()
            }
        };
        Self::with_client(client)
    }

    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            user_agent_override: None,
            progress_tx: None,
            protected_hls_policy: ProtectedHlsPolicy::Fail,
        }
    }

    pub fn with_user_agent_override(mut self, ua: Option<String>) -> Self {
        self.user_agent_override = ua;
        self
    }

    /// Attach a channel that receives per-segment progress updates
    /// (percent = completed / total segments, with accumulated bytes).
    pub fn with_progress(mut self, tx: mpsc::Sender<ProgressUpdate>) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    pub fn with_protected_hls_policy(mut self, policy: ProtectedHlsPolicy) -> Self {
        self.protected_hls_policy = policy;
        self
    }

    fn effective_user_agent(&self) -> &str {
        self.user_agent_override.as_deref().unwrap_or(USER_AGENT)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn download(
        &self,
        m3u8_url: &str,
        output_path: &str,
        referer: &str,
        bytes_tx: Option<UnboundedSender<u64>>,
        cancel_token: CancellationToken,
        max_concurrent: u32,
        max_retries: u32,
    ) -> anyhow::Result<HlsDownloadResult> {
        self.download_with_quality(
            m3u8_url,
            output_path,
            referer,
            bytes_tx,
            cancel_token,
            max_concurrent,
            max_retries,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn download_with_quality(
        &self,
        m3u8_url: &str,
        output_path: &str,
        referer: &str,
        bytes_tx: Option<UnboundedSender<u64>>,
        cancel_token: CancellationToken,
        max_concurrent: u32,
        max_retries: u32,
        max_height: Option<u32>,
    ) -> anyhow::Result<HlsDownloadResult> {
        if cancel_token.is_cancelled() {
            anyhow::bail!("Download cancelled by user");
        }

        let m3u8_text = self.fetch_m3u8_with_retry(m3u8_url, referer, 3).await?;

        let m3u8_bytes = m3u8_text.as_bytes();

        if let Ok((_, master)) = parse_master_playlist(m3u8_bytes) {
            if let Some(variant) = select_best_variant(&master, max_height.unwrap_or(720)) {
                let variant_url = resolve_url(m3u8_url, &variant.uri);
                return self
                    .download_media_playlist(
                        &variant_url,
                        output_path,
                        referer,
                        bytes_tx,
                        cancel_token,
                        max_concurrent,
                        max_retries,
                    )
                    .await;
            }
        }

        if parse_media_playlist(m3u8_bytes).is_ok() {
            return self
                .download_media_playlist(
                    m3u8_url,
                    output_path,
                    referer,
                    bytes_tx,
                    cancel_token,
                    max_concurrent,
                    max_retries,
                )
                .await;
        }

        anyhow::bail!("Failed to parse m3u8: neither master nor media playlist")
    }

    async fn fetch_m3u8_with_retry(
        &self,
        url: &str,
        referer: &str,
        max_retries: u32,
    ) -> anyhow::Result<String> {
        let mut last_err = None;
        for attempt in 0..max_retries {
            let req = apply_referer_headers(self.client.get(url), referer)
                .header("User-Agent", self.effective_user_agent());
            match req.send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        last_err =
                            Some(anyhow::anyhow!("HTTP {} fetching playlist", resp.status()));
                    } else {
                        match resp.text().await {
                            Ok(text) => return Ok(text),
                            Err(e) => last_err = Some(anyhow::anyhow!(e)),
                        }
                    }
                }
                Err(e) => last_err = Some(anyhow::anyhow!(e)),
            }
            if attempt < max_retries - 1 {
                let base = 500 * (attempt as u64 + 1);
                let jitter = rand::random::<u64>() % (base / 2 + 1);
                tokio::time::sleep(Duration::from_millis(base + jitter)).await;
            }
        }
        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("Failed to fetch m3u8 after {} attempts", max_retries)
        }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_media_playlist(
        &self,
        m3u8_url: &str,
        output_path: &str,
        referer: &str,
        bytes_tx: Option<UnboundedSender<u64>>,
        cancel_token: CancellationToken,
        max_concurrent: u32,
        max_retries: u32,
    ) -> anyhow::Result<HlsDownloadResult> {
        let resp = apply_referer_headers(self.client.get(m3u8_url), referer)
            .header("User-Agent", self.effective_user_agent())
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("HTTP {} fetching playlist", resp.status());
        }

        let text = resp.text().await?;

        let (_, playlist) = parse_media_playlist(text.as_bytes())
            .map_err(|e| anyhow::anyhow!("Parse media playlist: {:?}", e))?;

        let total_segments = playlist.segments.len();

        let encryption = self
            .fetch_encryption_info(&playlist, m3u8_url, referer)
            .await?;
        let protected_media = encryption.protected_media.clone();

        let output = PathBuf::from(output_path);
        let part_path = {
            let mut p = output.as_os_str().to_owned();
            p.push(".part");
            PathBuf::from(p)
        };
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let download_units = build_download_units(&playlist, m3u8_url)?;
        let total_units = download_units.len();

        let (seg_tx, seg_rx) = mpsc::channel::<(usize, DownloadedHlsUnit)>(max_concurrent as usize);

        let writer_output = part_path.clone();
        let media_sequence = playlist.media_sequence;
        let aes128 = encryption.aes128;
        let writer = tokio::spawn(async move {
            write_segments_ordered(seg_rx, &writer_output, &aes128, media_sequence, total_units)
                .await
        });

        let semaphore = Arc::new(Semaphore::new(max_concurrent as usize));
        let completed = Arc::new(AtomicUsize::new(0));
        let downloaded_bytes = Arc::new(AtomicU64::new(0));
        let fail_token = cancel_token.child_token();
        let errors: Arc<tokio::sync::Mutex<HashMap<String, u32>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        let client = &self.client;
        let errors_ref = &errors;
        let completed_ref = &completed;
        let downloaded_ref = &downloaded_bytes;
        let fail_ref = &fail_token;
        let sem_ref = &semaphore;
        let user_agent = self.effective_user_agent().to_string();
        let user_agent_ref = &user_agent;
        let progress_ref = &self.progress_tx;

        stream::iter(download_units)
            .map(|unit| {
                let bytes_tx = bytes_tx.clone();
                let seg_tx = seg_tx.clone();
                let referer = referer.to_string();
                async move {
                    let _permit = sem_ref.acquire().await.unwrap();
                    if fail_ref.is_cancelled() {
                        return;
                    }
                    match download_segment_with_retry(
                        client,
                        &unit.url,
                        unit.byte_range.as_ref(),
                        &referer,
                        user_agent_ref,
                        max_retries,
                        fail_ref,
                    )
                    .await
                    {
                        Ok(data) => {
                            if let Some(ref btx) = bytes_tx {
                                let _ = btx.send(data.len() as u64);
                            }
                            let done = if matches!(unit.kind, HlsOutputUnitKind::Segment { .. }) {
                                completed_ref.fetch_add(1, Ordering::Relaxed) + 1
                            } else {
                                completed_ref.load(Ordering::Relaxed)
                            };
                            let total_dl = downloaded_ref
                                .fetch_add(data.len() as u64, Ordering::Relaxed)
                                + data.len() as u64;
                            if let Some(ptx) = progress_ref {
                                let percent = if total_segments > 0 {
                                    (done as f64 / total_segments as f64) * 100.0
                                } else {
                                    0.0
                                };
                                // try_send: progress is best-effort and must
                                // never stall segment downloads.
                                let _ = ptx.try_send(ProgressUpdate::rich(
                                    percent,
                                    Some(total_dl),
                                    None,
                                    None,
                                    None,
                                ));
                            }
                            let _ = seg_tx
                                .send((
                                    unit.order,
                                    DownloadedHlsUnit {
                                        kind: unit.kind,
                                        data,
                                    },
                                ))
                                .await;
                        }
                        Err(e) => {
                            let key = e.to_string();
                            let mut errs = errors_ref.lock().await;
                            *errs.entry(key).or_insert(0) += 1;
                            drop(errs);
                            fail_ref.cancel();
                        }
                    }
                }
            })
            .buffer_unordered(max_concurrent as usize)
            .collect::<()>()
            .await;

        drop(seg_tx);

        let writer_result = writer
            .await
            .map_err(|e| anyhow::anyhow!("Writer task panicked: {:?}", e))?;

        if cancel_token.is_cancelled() {
            let _ = std::fs::remove_file(&part_path);
            anyhow::bail!("Download cancelled by user");
        }

        let errs = errors.lock().await;
        if !errs.is_empty() {
            let _ = std::fs::remove_file(&part_path);
            let summary: Vec<String> = errs
                .iter()
                .map(|(msg, count)| {
                    if *count > 1 {
                        format!("{} (x{})", msg, count)
                    } else {
                        msg.clone()
                    }
                })
                .collect();
            anyhow::bail!("Segment download failed: {}", summary.join("; "));
        }
        drop(errs);

        writer_result?;

        std::fs::rename(&part_path, &output)?;

        let file_size = std::fs::metadata(&output)?.len();
        if file_size == 0 && protected_media.is_none() {
            let _ = std::fs::remove_file(&output);
            anyhow::bail!("HLS download produced no data (0 bytes)");
        }
        let protection_sidecar_path = if let Some(ref protected) = protected_media {
            Some(write_protection_sidecar(&output, protected)?)
        } else {
            None
        };

        Ok(HlsDownloadResult {
            path: output,
            file_size,
            segments: total_segments,
            protected_passthrough: protected_media.is_some(),
            protected_media,
            protection_sidecar_path,
        })
    }

    async fn fetch_encryption_info(
        &self,
        playlist: &m3u8_rs::MediaPlaylist,
        m3u8_url: &str,
        referer: &str,
    ) -> anyhow::Result<EncryptionDecision> {
        for segment in &playlist.segments {
            if let Some(key) = &segment.key {
                match key.method {
                    m3u8_rs::KeyMethod::AES128 => {
                        if let Some(uri) = &key.uri {
                            let key_url = resolve_url(m3u8_url, uri);
                            let key_bytes = self.fetch_key_with_retry(&key_url, referer, 3).await?;
                            let iv = key.iv.as_ref().map(|iv_str| parse_hex_iv(iv_str));
                            return Ok(EncryptionDecision {
                                aes128: Some(EncryptionInfo { key_bytes, iv }),
                                protected_media: None,
                            });
                        }
                    }
                    m3u8_rs::KeyMethod::SampleAES => {
                        if self.protected_hls_policy == ProtectedHlsPolicy::Fail {
                            anyhow::bail!(
                                "HLS stream uses SAMPLE-AES (DRM-protected), cannot decrypt"
                            );
                        }

                        tracing::warn!(
                            "[hls] SAMPLE-AES protected HLS detected; saving encrypted media"
                        );
                        return Ok(EncryptionDecision {
                            aes128: None,
                            protected_media: Some(ProtectedMediaInfo {
                                marker: "omniget.protected_hls.v1".to_string(),
                                encrypted: true,
                                encryption_method: "SAMPLE-AES".to_string(),
                                source_url: m3u8_url.to_string(),
                                key_uri: key.uri.clone(),
                                key_format: key.keyformat.clone(),
                                decryption_status: "not_decrypted".to_string(),
                                note: "Saved encrypted without built-in decryption. If future decryption support is added, this file is eligible once the rights holder grants a valid key.".to_string(),
                            }),
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(EncryptionDecision::default())
    }

    async fn fetch_key_with_retry(
        &self,
        url: &str,
        referer: &str,
        max_retries: u32,
    ) -> anyhow::Result<Vec<u8>> {
        let mut last_err = None;
        for attempt in 0..max_retries {
            let req = apply_referer_headers(self.client.get(url), referer)
                .header("User-Agent", self.effective_user_agent());
            match req.send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        last_err = Some(anyhow::anyhow!("HTTP {} fetching AES key", resp.status()));
                    } else {
                        match resp.bytes().await {
                            Ok(bytes) => return Ok(bytes.to_vec()),
                            Err(e) => last_err = Some(anyhow::anyhow!(e)),
                        }
                    }
                }
                Err(e) => last_err = Some(anyhow::anyhow!(e)),
            }
            if attempt < max_retries - 1 {
                let base = 500 * (attempt as u64 + 1);
                let jitter = rand::random::<u64>() % (base / 2 + 1);
                tokio::time::sleep(Duration::from_millis(base + jitter)).await;
            }
        }
        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("Failed to fetch AES key after {} attempts", max_retries)
        }))
    }
}

#[derive(Debug)]
struct EncryptionInfo {
    key_bytes: Vec<u8>,
    iv: Option<[u8; 16]>,
}

/// Attach `Referer` (and a matching `Origin`) headers to a request.
/// An empty referer means "send no Referer/Origin at all" — some CDNs
/// reject requests with a wrong Referer but accept ones without any.
fn apply_referer_headers(req: reqwest::RequestBuilder, referer: &str) -> reqwest::RequestBuilder {
    if referer.is_empty() {
        return req;
    }
    let req = req.header("Referer", referer);
    match url_origin(referer) {
        Some(origin) => req.header("Origin", origin),
        None => req,
    }
}

/// Origin (`scheme://host[:port]`, no path, no trailing slash) of a URL.
fn url_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let scheme = parsed.scheme();
    Some(match parsed.port() {
        Some(port) => format!("{}://{}:{}", scheme, host, port),
        None => format!("{}://{}", scheme, host),
    })
}

#[derive(Debug, Default)]
struct EncryptionDecision {
    aes128: Option<EncryptionInfo>,
    protected_media: Option<ProtectedMediaInfo>,
}

#[derive(Clone, Debug)]
struct DownloadHlsUnit {
    order: usize,
    url: String,
    byte_range: Option<ResolvedByteRange>,
    kind: HlsOutputUnitKind,
}

#[derive(Clone, Debug)]
struct DownloadedHlsUnit {
    kind: HlsOutputUnitKind,
    data: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
enum HlsOutputUnitKind {
    InitMap,
    Segment { segment_index: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedByteRange {
    start: u64,
    end: u64,
}

impl ResolvedByteRange {
    fn header_value(&self) -> String {
        format!("bytes={}-{}", self.start, self.end)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct MapIdentity {
    uri: String,
    byte_range: Option<(u64, Option<u64>)>,
}

fn map_identity(map: &m3u8_rs::Map) -> MapIdentity {
    MapIdentity {
        uri: map.uri.clone(),
        byte_range: map.byte_range.as_ref().map(|br| (br.length, br.offset)),
    }
}

fn build_download_units(
    playlist: &m3u8_rs::MediaPlaylist,
    m3u8_url: &str,
) -> anyhow::Result<Vec<DownloadHlsUnit>> {
    let mut units = Vec::new();
    let mut last_map: Option<MapIdentity> = None;
    let mut byte_range_offsets: HashMap<String, u64> = HashMap::new();

    for (segment_index, segment) in playlist.segments.iter().enumerate() {
        if let Some(map) = &segment.map {
            let identity = map_identity(map);
            if last_map.as_ref() != Some(&identity) {
                let map_url = resolve_url(m3u8_url, &map.uri);
                let byte_range =
                    resolve_byte_range(&map_url, map.byte_range.as_ref(), &mut byte_range_offsets)?;
                units.push(DownloadHlsUnit {
                    order: units.len(),
                    url: map_url,
                    byte_range,
                    kind: HlsOutputUnitKind::InitMap,
                });
                last_map = Some(identity);
            }
        } else {
            last_map = None;
        }

        let segment_url = resolve_url(m3u8_url, &segment.uri);
        let byte_range = resolve_byte_range(
            &segment_url,
            segment.byte_range.as_ref(),
            &mut byte_range_offsets,
        )?;
        units.push(DownloadHlsUnit {
            order: units.len(),
            url: segment_url,
            byte_range,
            kind: HlsOutputUnitKind::Segment { segment_index },
        });
    }

    Ok(units)
}

fn resolve_byte_range(
    url: &str,
    byte_range: Option<&m3u8_rs::ByteRange>,
    offsets: &mut HashMap<String, u64>,
) -> anyhow::Result<Option<ResolvedByteRange>> {
    let Some(byte_range) = byte_range else {
        return Ok(None);
    };
    if byte_range.length == 0 {
        anyhow::bail!("HLS byte range length cannot be zero");
    }

    let start = byte_range
        .offset
        .unwrap_or_else(|| *offsets.get(url).unwrap_or(&0));
    let end = start
        .checked_add(byte_range.length - 1)
        .ok_or_else(|| anyhow::anyhow!("HLS byte range overflow"))?;
    let next = end
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("HLS byte range overflow"))?;
    offsets.insert(url.to_string(), next);

    Ok(Some(ResolvedByteRange { start, end }))
}

pub fn protection_sidecar_path(output_path: &std::path::Path) -> PathBuf {
    let mut path = output_path.as_os_str().to_owned();
    path.push(".encrypted.json");
    PathBuf::from(path)
}

pub fn write_protection_sidecar(
    output_path: &std::path::Path,
    protected: &ProtectedMediaInfo,
) -> anyhow::Result<PathBuf> {
    let path = protection_sidecar_path(output_path);
    let json = serde_json::to_vec_pretty(protected)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

fn select_best_variant(master: &MasterPlaylist, max_height: u32) -> Option<&VariantStream> {
    let real: Vec<&VariantStream> = master.variants.iter().filter(|v| !v.is_i_frame).collect();

    if real.is_empty() {
        return None;
    }

    let mut sorted = real;
    sorted.sort_by_key(|v| v.resolution.as_ref().map(|r| r.height).unwrap_or(0));

    let max_h = max_height as u64;
    let mut best: Option<&VariantStream> = None;
    for v in &sorted {
        if v.resolution
            .as_ref()
            .map(|r| r.height <= max_h)
            .unwrap_or(true)
        {
            best = Some(*v);
        }
    }

    best.or_else(|| sorted.first().copied())
}

fn resolve_url(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return relative.to_string();
    }

    let (base_path, query) = match base.find('?') {
        Some(pos) => (&base[..pos], Some(&base[pos..])),
        None => (base, None),
    };

    let resolved = if let Some(pos) = base_path.rfind('/') {
        format!("{}/{}", &base_path[..pos], relative)
    } else {
        relative.to_string()
    };

    match query {
        Some(q) if !relative.contains('?') => format!("{}{}", resolved, q),
        _ => resolved,
    }
}

async fn write_segments_ordered(
    mut rx: mpsc::Receiver<(usize, DownloadedHlsUnit)>,
    output_path: &PathBuf,
    encryption: &Option<EncryptionInfo>,
    media_sequence: u64,
    total_units: usize,
) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file =
        std::io::BufWriter::with_capacity(256 * 1024, std::fs::File::create(output_path)?);
    let mut next_expected: usize = 0;
    let mut pending: BTreeMap<usize, DownloadedHlsUnit> = BTreeMap::new();

    while let Some((idx, unit)) = rx.recv().await {
        pending.insert(idx, unit);

        while let Some(unit) = pending.remove(&next_expected) {
            if let Some(enc) = encryption {
                use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
                type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

                let iv = match unit.kind {
                    HlsOutputUnitKind::Segment { segment_index } => {
                        compute_iv(enc, segment_index, media_sequence)
                    }
                    HlsOutputUnitKind::InitMap => enc.iv.ok_or_else(|| {
                        anyhow::anyhow!("AES-128 encrypted HLS init map requires explicit IV")
                    })?,
                };
                let mut buf = unit.data;
                let decryptor = Aes128CbcDec::new_from_slices(&enc.key_bytes, &iv)
                    .map_err(|e| anyhow::anyhow!("AES init: {:?}", e))?;
                let decrypted = decryptor
                    .decrypt_padded_mut::<Pkcs7>(&mut buf)
                    .map_err(|e| anyhow::anyhow!("AES decrypt: {:?}", e))?;
                file.write_all(decrypted)?;
            } else {
                file.write_all(&unit.data)?;
            }
            next_expected += 1;
        }
    }

    file.flush()?;

    if next_expected < total_units {
        anyhow::bail!(
            "Only {} of {} HLS units were written",
            next_expected,
            total_units
        );
    }

    Ok(())
}

const SEGMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

async fn download_segment_with_retry(
    client: &Client,
    url: &str,
    byte_range: Option<&ResolvedByteRange>,
    referer: &str,
    user_agent: &str,
    max_retries: u32,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<u8>> {
    let mut last_err = None;
    for attempt in 0..max_retries {
        if cancel.is_cancelled() {
            anyhow::bail!("Download cancelled");
        }

        let result = tokio::time::timeout(SEGMENT_TIMEOUT, async {
            let mut request =
                apply_referer_headers(client.get(url), referer).header("User-Agent", user_agent);
            if let Some(byte_range) = byte_range {
                request = request.header(reqwest::header::RANGE, byte_range.header_value());
            }
            let resp = request.send().await?;

            let status = resp.status();
            if !status.is_success() {
                let code = status.as_u16();
                if (400..500).contains(&code) && code != 429 && code != 408 {
                    return Err(anyhow::anyhow!("HTTP {} (fatal) downloading segment", code));
                }
                return Err(anyhow::anyhow!("HTTP {} downloading segment", code));
            }

            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| anyhow::anyhow!(e))
        })
        .await;

        match result {
            Ok(Ok(data)) => return Ok(data),
            Ok(Err(e)) => {
                if e.to_string().contains("(fatal)") {
                    return Err(e);
                }
                last_err = Some(e);
            }
            Err(_) => last_err = Some(anyhow::anyhow!("Timeout downloading segment")),
        }
        if attempt < max_retries - 1 {
            let base = 500 * (attempt as u64 + 1);
            let jitter = rand::random::<u64>() % (base / 2 + 1);
            tokio::time::sleep(std::time::Duration::from_millis(base + jitter)).await;
        }
    }
    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!("Segment download failed after {} attempts", max_retries)
    }))
}

fn compute_iv(encryption: &EncryptionInfo, segment_index: usize, media_sequence: u64) -> [u8; 16] {
    if let Some(iv) = &encryption.iv {
        return *iv;
    }
    let seq = media_sequence + segment_index as u64;
    let mut iv = [0u8; 16];
    iv[8..16].copy_from_slice(&seq.to_be_bytes());
    iv
}

fn parse_hex_iv(iv_str: &str) -> [u8; 16] {
    let hex = iv_str.trim_start_matches("0x").trim_start_matches("0X");
    let mut result = [0u8; 16];
    let padded = format!("{:0>32}", hex);
    for i in 0..16 {
        result[i] = u8::from_str_radix(&padded[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use m3u8_rs::{MasterPlaylist, Resolution, VariantStream};

    fn parse_test_media_playlist(text: &str) -> m3u8_rs::MediaPlaylist {
        parse_media_playlist(text.as_bytes()).unwrap().1
    }

    fn temp_hls_output(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "omniget-hls-{}-{}-{}",
            name,
            std::process::id(),
            nanos
        ))
    }

    #[test]
    fn url_origin_basic() {
        assert_eq!(
            url_origin("https://cdn.example.com/path/master.m3u8?token=abc").as_deref(),
            Some("https://cdn.example.com")
        );
    }

    #[test]
    fn url_origin_with_port() {
        assert_eq!(
            url_origin("http://cdn.example.com:8080/video/seg.ts").as_deref(),
            Some("http://cdn.example.com:8080")
        );
    }

    #[test]
    fn url_origin_invalid_returns_none() {
        assert_eq!(url_origin("not a url"), None);
        assert_eq!(url_origin(""), None);
    }

    #[test]
    fn resolve_url_absolute_passthrough() {
        assert_eq!(
            resolve_url(
                "https://cdn.example.com/path/master.m3u8",
                "https://other.com/video.ts"
            ),
            "https://other.com/video.ts"
        );
    }

    #[test]
    fn resolve_url_relative() {
        assert_eq!(
            resolve_url("https://cdn.example.com/path/master.m3u8", "segment0.ts"),
            "https://cdn.example.com/path/segment0.ts"
        );
    }

    #[test]
    fn resolve_url_propagates_query() {
        assert_eq!(
            resolve_url(
                "https://cdn.example.com/path/master.m3u8?token=abc",
                "segment0.ts"
            ),
            "https://cdn.example.com/path/segment0.ts?token=abc"
        );
    }

    #[test]
    fn resolve_url_relative_with_own_query_skips_base_query() {
        assert_eq!(
            resolve_url(
                "https://cdn.example.com/path/master.m3u8?token=abc",
                "segment0.ts?key=123"
            ),
            "https://cdn.example.com/path/segment0.ts?key=123"
        );
    }

    #[test]
    fn resolve_url_no_slash_in_base() {
        assert_eq!(resolve_url("master.m3u8", "segment0.ts"), "segment0.ts");
    }

    #[test]
    fn build_download_units_includes_init_map_and_byte_ranges() {
        let playlist = parse_test_media_playlist(
            r#"#EXTM3U
#EXT-X-VERSION:7
#EXT-X-TARGETDURATION:4
#EXT-X-MAP:URI="init.mp4",BYTERANGE="8@10"
#EXTINF:4.0,
#EXT-X-BYTERANGE:4@20
media.mp4
#EXTINF:4.0,
#EXT-X-BYTERANGE:4
media.mp4
#EXT-X-ENDLIST
"#,
        );

        let units =
            build_download_units(&playlist, "https://cdn.example.com/course/master.m3u8").unwrap();

        assert_eq!(units.len(), 3);
        assert!(matches!(units[0].kind, HlsOutputUnitKind::InitMap));
        assert_eq!(units[0].url, "https://cdn.example.com/course/init.mp4");
        assert_eq!(
            units[0].byte_range,
            Some(ResolvedByteRange { start: 10, end: 17 })
        );
        assert!(matches!(
            units[1].kind,
            HlsOutputUnitKind::Segment { segment_index: 0 }
        ));
        assert_eq!(
            units[1].byte_range,
            Some(ResolvedByteRange { start: 20, end: 23 })
        );
        assert!(matches!(
            units[2].kind,
            HlsOutputUnitKind::Segment { segment_index: 1 }
        ));
        assert_eq!(
            units[2].byte_range,
            Some(ResolvedByteRange { start: 24, end: 27 })
        );
    }

    #[test]
    fn byte_range_header_uses_inclusive_end() {
        assert_eq!(
            ResolvedByteRange { start: 3, end: 9 }.header_value(),
            "bytes=3-9"
        );
    }

    #[tokio::test]
    async fn sample_aes_fails_by_default() {
        let playlist = parse_test_media_playlist(
            r#"#EXTM3U
#EXT-X-TARGETDURATION:4
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="skd://asset",KEYFORMAT="com.widevine"
#EXTINF:4.0,
seg0.m4s
"#,
        );

        let err = HlsDownloader::new()
            .fetch_encryption_info(&playlist, "https://cdn.example.com/master.m3u8", "")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("SAMPLE-AES"));
    }

    #[tokio::test]
    async fn sample_aes_save_encrypted_marks_protected_media() {
        let playlist = parse_test_media_playlist(
            r#"#EXTM3U
#EXT-X-TARGETDURATION:4
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="skd://asset",KEYFORMAT="com.widevine"
#EXTINF:4.0,
seg0.m4s
"#,
        );

        let decision = HlsDownloader::new()
            .with_protected_hls_policy(ProtectedHlsPolicy::SaveEncrypted)
            .fetch_encryption_info(&playlist, "https://cdn.example.com/master.m3u8", "")
            .await
            .unwrap();

        assert!(decision.aes128.is_none());
        let protected = decision.protected_media.unwrap();
        assert!(protected.encrypted);
        assert_eq!(protected.encryption_method, "SAMPLE-AES");
        assert_eq!(protected.key_uri.as_deref(), Some("skd://asset"));
        assert_eq!(protected.decryption_status, "not_decrypted");
    }

    #[tokio::test]
    async fn write_segments_ordered_writes_init_map_before_segment() {
        let output = temp_hls_output("ordered-units");
        let (tx, rx) = mpsc::channel(2);

        tx.send((
            1,
            DownloadedHlsUnit {
                kind: HlsOutputUnitKind::Segment { segment_index: 0 },
                data: b"segment".to_vec(),
            },
        ))
        .await
        .unwrap();
        tx.send((
            0,
            DownloadedHlsUnit {
                kind: HlsOutputUnitKind::InitMap,
                data: b"init".to_vec(),
            },
        ))
        .await
        .unwrap();
        drop(tx);

        write_segments_ordered(rx, &output, &None, 0, 2)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&output).unwrap(), b"initsegment");
        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn protection_sidecar_path_appends_marker_extension() {
        assert_eq!(
            protection_sidecar_path(std::path::Path::new("/tmp/video.mp4")),
            PathBuf::from("/tmp/video.mp4.encrypted.json")
        );
    }

    #[test]
    fn select_best_variant_picks_720() {
        let master = MasterPlaylist {
            variants: vec![
                VariantStream {
                    uri: "360.m3u8".into(),
                    bandwidth: 800_000,
                    resolution: Some(Resolution {
                        width: 640,
                        height: 360,
                    }),
                    ..Default::default()
                },
                VariantStream {
                    uri: "720.m3u8".into(),
                    bandwidth: 2_500_000,
                    resolution: Some(Resolution {
                        width: 1280,
                        height: 720,
                    }),
                    ..Default::default()
                },
                VariantStream {
                    uri: "1080.m3u8".into(),
                    bandwidth: 5_000_000,
                    resolution: Some(Resolution {
                        width: 1920,
                        height: 1080,
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let best = select_best_variant(&master, 720).unwrap();
        assert_eq!(best.uri, "720.m3u8");
    }

    #[test]
    fn select_best_variant_picks_1080() {
        let master = MasterPlaylist {
            variants: vec![
                VariantStream {
                    uri: "720.m3u8".into(),
                    bandwidth: 2_500_000,
                    resolution: Some(Resolution {
                        width: 1280,
                        height: 720,
                    }),
                    ..Default::default()
                },
                VariantStream {
                    uri: "1080.m3u8".into(),
                    bandwidth: 5_000_000,
                    resolution: Some(Resolution {
                        width: 1920,
                        height: 1080,
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let best = select_best_variant(&master, 1080).unwrap();
        assert_eq!(best.uri, "1080.m3u8");
    }

    #[test]
    fn select_best_variant_empty_returns_none() {
        let master = MasterPlaylist {
            variants: vec![],
            ..Default::default()
        };
        assert!(select_best_variant(&master, 720).is_none());
    }

    #[test]
    fn select_best_variant_skips_iframe() {
        let master = MasterPlaylist {
            variants: vec![
                VariantStream {
                    uri: "iframe.m3u8".into(),
                    bandwidth: 100_000,
                    is_i_frame: true,
                    resolution: Some(Resolution {
                        width: 320,
                        height: 180,
                    }),
                    ..Default::default()
                },
                VariantStream {
                    uri: "720.m3u8".into(),
                    bandwidth: 2_500_000,
                    resolution: Some(Resolution {
                        width: 1280,
                        height: 720,
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let best = select_best_variant(&master, 720).unwrap();
        assert_eq!(best.uri, "720.m3u8");
    }

    #[test]
    fn select_best_variant_fallback_to_lowest_when_all_exceed() {
        let master = MasterPlaylist {
            variants: vec![
                VariantStream {
                    uri: "1080.m3u8".into(),
                    bandwidth: 5_000_000,
                    resolution: Some(Resolution {
                        width: 1920,
                        height: 1080,
                    }),
                    ..Default::default()
                },
                VariantStream {
                    uri: "4k.m3u8".into(),
                    bandwidth: 15_000_000,
                    resolution: Some(Resolution {
                        width: 3840,
                        height: 2160,
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let best = select_best_variant(&master, 360).unwrap();
        assert_eq!(best.uri, "1080.m3u8");
    }

    #[test]
    fn select_best_variant_no_resolution_treated_as_eligible() {
        let master = MasterPlaylist {
            variants: vec![VariantStream {
                uri: "audio.m3u8".into(),
                bandwidth: 128_000,
                resolution: None,
                ..Default::default()
            }],
            ..Default::default()
        };
        let best = select_best_variant(&master, 720).unwrap();
        assert_eq!(best.uri, "audio.m3u8");
    }

    #[test]
    fn parse_hex_iv_full_32_chars() {
        let iv = parse_hex_iv("0x00000000000000000000000000000001");
        let mut expected = [0u8; 16];
        expected[15] = 1;
        assert_eq!(iv, expected);
    }

    #[test]
    fn parse_hex_iv_short_padded() {
        let iv = parse_hex_iv("0xFF");
        let mut expected = [0u8; 16];
        expected[15] = 0xFF;
        assert_eq!(iv, expected);
    }

    #[test]
    fn parse_hex_iv_uppercase_prefix() {
        let iv = parse_hex_iv("0X0A0B0C0D0E0F10111213141516171819");
        assert_eq!(iv[0], 0x0A);
        assert_eq!(iv[7], 0x11);
        assert_eq!(iv[15], 0x19);
    }

    #[test]
    fn parse_hex_iv_no_prefix() {
        let iv = parse_hex_iv("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF");
        assert_eq!(iv, [0xFF; 16]);
    }

    #[test]
    fn compute_iv_returns_explicit_when_present() {
        let explicit_iv = [0xAB; 16];
        let enc = EncryptionInfo {
            key_bytes: vec![0u8; 16],
            iv: Some(explicit_iv),
        };
        assert_eq!(compute_iv(&enc, 5, 100), explicit_iv);
    }

    #[test]
    fn compute_iv_derives_from_sequence() {
        let enc = EncryptionInfo {
            key_bytes: vec![0u8; 16],
            iv: None,
        };
        let result = compute_iv(&enc, 3, 100);
        let mut expected = [0u8; 16];
        expected[8..16].copy_from_slice(&103u64.to_be_bytes());
        assert_eq!(result, expected);
    }

    #[test]
    fn compute_iv_sequence_zero() {
        let enc = EncryptionInfo {
            key_bytes: vec![0u8; 16],
            iv: None,
        };
        let result = compute_iv(&enc, 0, 0);
        assert_eq!(result, [0u8; 16]);
    }
}
