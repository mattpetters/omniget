# Implementation Plan: Download DRM-Protected Udemy Content

## Problem Statement

The OmniGet app skips or fails to download DRM-protected (Widevine encrypted) courses from Udemy. Users cannot obtain the video files even if they're willing to decrypt them later with a Widevine key.

## Root Cause Analysis

### 1. HLS Downloader Blocks SampleAES DRM

The HLS downloader explicitly rejects streams using `SampleAES` encryption (which includes both FairPlay and Widevine):

**File:** `src-tauri/omniget-core/src/core/hls_downloader.rs:348-349`

```rust
m3u8_rs::KeyMethod::SampleAES => {
    anyhow::bail!("HLS stream uses SAMPLE-AES (FairPlay DRM), cannot decrypt");
}
```

This causes an immediate failure when the playlist uses SampleAES encryption.

### 2. No Widevine Decryption Support in yt-dlp Integration

The yt-dlp downloader in `generic_ytdlp/mod.rs` and `ytdlp.rs` does not pass Widevine decryption arguments to yt-dlp. Relevant options that could be used:

- `--widevine-key` - Provide a Widevine key directly
- `--widevine-cdm` - Path to Widevine CDM extension
- `--extractor-args "udemy:widevine_pssh_data=..."` - Udemy-specific Widevine PSSH data

### 3. No "Download Encrypted" Fallback Option

When decryption fails, the download is aborted entirely rather than saving the encrypted file for later decryption.

## Architecture Overview

```
┌─────────────┐     ┌──────────────────┐     ┌─────────────────────┐
│   Frontend   │────▶│  Download Queue   │────▶│   PlatformRegistry  │
│  (SvelteKit) │     │  (queue.rs)      │     │                     │
└─────────────┘     └──────────────────┘     │ ┌─────────────────┐ │
                                              │ │ udemy.com?       │ │
                                              │ │ └─► GenericYtdlp │ │
                                              │ └─────────────────┘ │
                                              └─────────────────────┘
                                                        │
                                              ┌─────────▼─────────┐
                                              │  GenericYtdlp     │
                                              │  (generic_ytdlp/  │
                                              │    mod.rs:443)    │
                                              └─────────┬─────────┘
                                                        │
                                              ┌─────────▼─────────┐
                                              │  yt-dlp            │
                                              │  (ytdlp.rs:1856)  │
                                              │                    │
                                              │  download_video() │
                                              └─────────┬─────────┘
                                                        │
                                              ┌─────────▼─────────┐
                                              │  HLS Downloader   │
                                              │  (hls_downloader.rs)│
                                              │                    │
                                              │  Blocks at         │
                                              │  line 349           │
                                              └───────────────────┘
```

## Implementation Plan

### Phase 1: Add Widevine Settings

Add new settings to the settings schema for Widevine CDM configuration and a "download encrypted" option.

**File:** `src-tauri/omniget-core/src/models/settings.rs`

Add to `DownloadSettings` struct (line ~80):

```rust
/// Widevine CDM extension directory (e.g., from Chrome at
/// Chrome //Extensions/content decryption module path).
/// Allows yt-dlp to decrypt Widevine-protected streams.
pub widevine_cdm: Option<String>,

/// Widevine PSSH data for EME-protected streams.
/// Stored as hex string.
pub widevine_pssh: Option<String>,

/// Force download even if decryption fails.
/// Saves encrypted file for later decryption.
pub download_encrypted: bool,
```

### Phase 2: Extend yt-dlp Arguments for Widevine

Add Widevine-specific arguments to yt-dlp when downloading from Udemy.

**File:** `src-tauri/omniget-core/src/core/ytdlp.rs`

1. Add Widevine argument builder function (near line 500):

```rust
/// Build Widevine-specific arguments for yt-dlp.
/// Requires platform == "udemy" and valid Widevine configuration.
fn build_widevine_args(platform: &str, settings: &Settings) -> Vec<String> {
    if platform != "udemy" {
        return Vec::new();
    }

    let mut args = Vec::new();

    // Add CDM path if available
    if let Some(ref cdm) = settings.widevine_cdm {
        if !cdm.is_empty() {
            args.push("--widevine-cdm".to_string());
            args.push(cdm.clone());
        }
    }

    // Add PSSH data if available
    if let Some(ref pssh) = settings.widevine_pssh {
        if !pssh.is_empty() {
            args.push("--extractor-args".to_string());
            args.push(format!("udemy:widevine_pssh_data={}", pssh));
        }
    }

    args
}
```

2. Integrate Widevine args into `download_video()` function (line 1856)

After building `base_args`, add:

```rust
// Add Widevine decryption args for Udemy
let widevine_args = build_widevine_args(&platform_name, &settings);
base_args.extend(widevine_args);
```

### Phase 3: Modify HLS Downloader for "Download Encrypted" Option

Update the HLS downloader to support downloading encrypted content when decryption is unavailable.

**File:** `src-tauri/omniget-core/src/core/hls_downloader.rs`

1. Add new enum for DRM handling behavior:

```rust
pub enum DrmStrategy {
    /// Decrypt on-the-fly, fail if no key is available.
    DecryptOrFail,
    /// Try to decrypt, but save encrypted file if key is unavailable.
    DownloadEncrypted,
}
```

2. Update `HlsDownloader` struct:

```rust
pub struct HlsDownloader {
    client: Client,
    user_agent_override: Option<String>,
    /// Strategy for handling DRM-protected streams.
    drm_strategy: DrmStrategy,
}
```

3. Add builder method:

```rust
pub fn with_drm_strategy(mut self, strategy: DrmStrategy) -> Self {
    self.drm_strategy = strategy;
    self
}
```

4. Modify `fetch_encryption_info()` (line 338) to not bail on SampleAES:

```rust
m3u8_rs::KeyMethod::SampleAES => {
    if self.drm_strategy == DrmStrategy::DecryptOrFail {
        anyhow::bail!("HLS stream uses SAMPLE-AES (Widevine/FairPlay DRM), cannot decrypt");
    }
    // For DownloadEncrypted: return None, write unencrypted data
    tracing::warn!("[hls] SampleAES detected, downloading encrypted content");
}
```

5. Update `write_segments_ordered()` to handle unencrypted writes (line 448):

```rust
// If encryption is detected but no key is available (SampleAES without decryption)
// Write segments as-is (encrypted)
if let Some(enc) = encryption {
    // Attempt AES-128 decryption
    // ... existing code ...
} else {
    // No encryption or could not obtain key - write as-is
    file.write_all(&segment_data)?;
}
```

### Phase 4: Update GenericYtdlp Platform Downloader

Connect the Widevine settings to the HLS downloader.

**File:** `src-tauri/src/platforms/generic_ytdlp/mod.rs`

1. Import settings accessor (around line 1):

```rust
use crate::storage::config;
```

2. In the `download()` method (line 443), check for encrypted download:

```rust
// After checking for HLS format
if selected.format == "hls" {
    // Check if user wants to download encrypted content
    let settings = config::load_settings(app_handle); // Get app handle from context
    let drm_strategy = if settings.widevine_cdm.is_some() {
        DrmStrategy::DecryptOrFail
    } else if settings.download_encrypted {
        DrmStrategy::DownloadEncrypted
    } else {
        // Try Widevine first, then fall back to encrypted download
        DrmStrategy::DownloadEncrypted
    };

    let downloader = HlsDownloader::with_client(client)
        .with_user_agent_override(opts.user_agent.clone())
        .with_drm_strategy(drm_strategy);
    
    // ... rest of HLS download code
}
```

### Phase 5: Add Settings UI

Add Widevine CDM configuration options to the settings UI.

**Files to modify:**
- Frontend settings components in `src/components/settings/`
- Add new i18n strings for Widevine options

### Phase 6: Add Widevine CDM Auto-detection

Create helper function to auto-detect Widevine CDM path from installed Chrome/Chromium browsers.

**File:** `src-tauri/omniget-core/src/core/widevine.rs` (new file)

```rust
use std::path::PathBuf;
use std::env;

/// Try to auto-detect Widevine CDM path from browser installations.
pub fn auto_detect_widevine_cdm() -> Option<PathBuf> {
    let os = env::consts::OS;
    
    match os {
        "macos" => detect_macos_widevine(),
        "windows" => detect_windows_widevine(),
        "linux" => detect_linux_widevine(),
        _ => None,
    }
}

fn detect_macos_widevine() -> Option<PathBuf> {
    // Check Chrome locations
    let chrome_dirs = vec![
        PathBuf::from("/Applications/Google Chrome.app"),
        PathBuf::from("/Applications/Chromium.app"),
        PathBuf::from(format!(
            "{}/Applications/Google Chrome Canary.app",
            env::var("HOME").unwrap_or_default()
        ))
    ];
    
    for chrome in chrome_dirs {
        // Widevine is typically at:
        // Chrome.app/Contents/Versions/<version>/WidevineCdm/<version>_<build>/
        let versions = chrome.join("Contents/Versions");
        if versions.exists() {
            // Find latest version
            for entry in std::fs::read_dir(&versions).ok()? {
                let version = entry.ok()?.path();
                let widevine = version.join("WidevineCdm");
                for entry in std::fs::read_dir(&widevine).ok()? {
                    let candidate = entry.ok()?.path();
                    if candidate.is_dir() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

// Similar functions for Windows and Linux...
```

## Testing Strategy

1. **Unit tests for Widevine argument building**
   - Test argument generation with/without CDM path
   - Test argument generation with/without PSSH data

2. **Integration tests for HLS downloader**
   - Test with SampleAES playlist (should download encrypted)
   - Test with AES128 playlist (should decrypt if key available)
   - Test fallback to encrypted download

3. **End-to-end tests with Udemy courses**
   - Test download of DRM-protected course
   - Verify file is downloaded (encrypted if no key)
   - Verify file can be decrypted later with external tool

## Risks and Mitigations

1. **Widevine CDM changes version**
   - Mitigation: Use latest version auto-detection, allow manual path override

2. **Udemy changes DRM implementation**
   - Mitigation: Keep yt-dlp updated, use nightly builds

3. **License issues with Widevine CDM**
   - Mitigation: Use system CDM, don't bundle it

## Implementation Timeline

| Phase | Description | Estimated Time |
|-------|-------------|----------------|
| 1 | Add settings for Widevine | 2 hours |
| 2 | Extend yt-dlp arguments | 3 hours |
| 3 | Modify HLS downloader | 4 hours |
| 4 | Update GenericYtdlp platform | 2 hours |
| 5 | Add settings UI | 2 hours |
| 6 | Add auto-detection | 2 hours |
| Testing | Unit/integration tests | 4 hours |
| **Total** | | **~20 hours** |

## Key Source Files

| File | Purpose |
|------|---------|
| `src-tauri/omniget-core/src/core/ytdlp.rs` | yt-dlp wrapper, download_video() |
| `src-tauri/omniget-core/src/core/hls_downloader.rs` | HLS downloader, encryption handling |
| `src-tauri/src/platforms/generic_ytdlp/mod.rs` | Generic platform downloader |
| `src-tauri/omniget-core/src/core/registry.rs` | Platform registry |
| `src-tauri/src/core/queue.rs` | Download queue |
| `src-tauri/omniget-core/src/platforms/mod.rs` | Platform enum (Udemy variant) |
| `src-tauri/omniget-core/src/models/settings.rs` | Settings schema |
