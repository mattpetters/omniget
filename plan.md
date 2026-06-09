# Execution Plan: stop empty downloads from reporting success; make them retryable & clearable

> Audience: an implementing model. Follow steps in order. Each edit gives the EXACT file,
> an anchor (unique existing text), and the replacement. After each phase run the stated
> build/check command and do not proceed if it fails. Do not refactor anything not listed.

## Problem (verified root causes)

The Downloads page shows course/lecture cards as **COMPLETE / 100%** with **0 bytes** and an
empty log, the header reads **"0 B saved"**, and the filter chips all read **0**. There are
**three** places an item is marked complete with no zero-byte check, plus two frontend display
bugs:

1. `src-tauri/src/core/queue.rs` ~1604-1624 — internal downloads: `Ok(dl)` → `mark_complete(..true.., Some(dl.file_size_bytes))` with no `> 0` check.
2. `src-tauri/src/commands/host_queue.rs` 208-216 — external/course-lesson downloads (the study plugin): `if args.success { percent=100; status=Complete }` with no `file_size_bytes > 0` check.
3. `src/lib/stores/download-listener.ts` 191-208 & 234-236 — course aggregate: marks complete on the plugin's `success` flag; line 200 hardcodes `recordDownloadComplete(0)` → "0 B saved".
4. `src/routes/downloads/+page.svelte` 107-113 — filter chip counts derive only from `genericList`; course items (`kind:"course"`) are counted nowhere → all chips read 0.
5. `src/routes/downloads/+page.svelte` 450-451 — header bytes come from a lifetime stats store that courses always feed `0`.

Fix every seam where success is declared so 0-byte output becomes a **retryable Error**, and
make the counts/header reflect what is actually on screen.

---

## Phase 1 — Backend: internal download path (queue.rs)

File: `src-tauri/src/core/queue.rs`

Find this block (inside the `Ok(dl) => {` arm, around line 1604):

```rust
            let state = {
                let mut q = queue.lock().await;
                if platform_name == "magnet" && dl.torrent_id.is_some() {
                    q.mark_seeding(
                        item_id,
                        Some(dl.file_path.to_string_lossy().to_string()),
                        Some(dl.file_size_bytes),
                        dl.torrent_id,
                    );
                } else {
                    q.mark_complete(
                        item_id,
                        true,
                        None,
                        Some(dl.file_path.to_string_lossy().to_string()),
                        Some(dl.file_size_bytes),
                    );
                }
                q.get_state()
            };
            emit_queue_state_from_state(&app, state);
```

Replace it with:

```rust
            let is_magnet_seed = platform_name == "magnet" && dl.torrent_id.is_some();
            // An exit-0 download that produced no bytes (e.g. a skipped DRM/region-locked
            // stream) must NOT be reported as success — that hides the failure and blocks
            // retry. Treat 0 bytes or a missing output file as a retryable failure.
            let empty_output =
                !is_magnet_seed && (dl.file_size_bytes == 0 || !dl.file_path.exists());
            if empty_output {
                append_download_log(
                    &app,
                    item_id,
                    "[omniget] download produced no data (0 bytes) — marking as failed".to_string(),
                );
            }
            let state = {
                let mut q = queue.lock().await;
                if is_magnet_seed {
                    q.mark_seeding(
                        item_id,
                        Some(dl.file_path.to_string_lossy().to_string()),
                        Some(dl.file_size_bytes),
                        dl.torrent_id,
                    );
                } else if empty_output {
                    q.mark_complete(
                        item_id,
                        false,
                        Some(EMPTY_DOWNLOAD_ERROR.to_string()),
                        None,
                        None,
                    );
                } else {
                    q.mark_complete(
                        item_id,
                        true,
                        None,
                        Some(dl.file_path.to_string_lossy().to_string()),
                        Some(dl.file_size_bytes),
                    );
                }
                q.get_state()
            };
            emit_queue_state_from_state(&app, state);
```

Then add this constant near the top of `queue.rs` (just after the existing `use` lines, before
the first `pub fn`/`pub struct`):

```rust
/// Message used when a download exits cleanly but wrote 0 bytes. Wording is deliberate: it must
/// NOT contain any keyword that `classify_download_error` maps to a non-retryable category
/// (e.g. "login", "cookie", "403", "404", "not found", "private", "ffmpeg", "yt-dlp"), so that
/// `is_retryable_error_message` returns true and the UI shows a Retry button.
pub const EMPTY_DOWNLOAD_ERROR: &str =
    "Download produced no data (0 bytes). The stream may be DRM-protected or temporarily blocked. Retry after re-checking access.";
```

Verify: `is_retryable_error_message(EMPTY_DOWNLOAD_ERROR)` must return `true`
(`classify_download_error` returns `("unknown", _)` for this string → retryable). Do not change
the wording without re-checking `src-tauri/omniget-core/src/core/errors.rs`.

**Check:** `cd src-tauri && cargo check`

---

## Phase 2 — Backend: external / course-lesson path (host_queue.rs)

File: `src-tauri/src/commands/host_queue.rs`, function `report_complete_inner`.

Find (around line 208):

```rust
                if args.success {
                    it.percent = 100.0;
                    if let Some(ref p) = args.file_path {
                        it.file_path = Some(p.to_string_lossy().to_string());
                    }
                    if let Some(sz) = args.file_size_bytes {
                        it.file_size_bytes = Some(sz);
                    }
                    it.status = QueueStatus::Complete { success: true };
                } else {
```

Replace the `if args.success {` line and its body so a success report carrying 0 bytes is
downgraded to a retryable failure. New version:

```rust
                // A "success" report that carries no bytes is not a success — the external
                // downloader skipped or produced an empty file. Downgrade to a retryable error
                // so the user can retry instead of seeing a false COMPLETE.
                let reported_empty = args.success
                    && args.file_size_bytes.unwrap_or(0) == 0
                    && args
                        .file_path
                        .as_ref()
                        .map(|p| std::fs::metadata(p).map(|m| m.len() == 0).unwrap_or(true))
                        .unwrap_or(true);
                if args.success && !reported_empty {
                    it.percent = 100.0;
                    if let Some(ref p) = args.file_path {
                        it.file_path = Some(p.to_string_lossy().to_string());
                    }
                    if let Some(sz) = args.file_size_bytes {
                        it.file_size_bytes = Some(sz);
                    }
                    it.status = QueueStatus::Complete { success: true };
                } else if reported_empty {
                    it.status = QueueStatus::Error {
                        message: crate::core::queue::EMPTY_DOWNLOAD_ERROR.to_string(),
                        retryable: true,
                    };
                } else {
```

(The original `else {` branch that handled `args.success == false` becomes this final `else`.
Its body — building `msg`/`retryable` and setting `QueueStatus::Error` — stays unchanged.)

**Check:** `cd src-tauri && cargo check`

---

## Phase 3 — Backend: defense-in-depth in leaf downloaders

These make a 0-byte result fail at the source with a clearer message. The Phase 1/2 guards are
authoritative; these are belt-and-suspenders.

### 3a. `src-tauri/omniget-core/src/core/direct_downloader.rs` (~line 227)

Find:

```rust
    std::fs::rename(&part_path, output)?;
    let _ = progress_tx.send(ProgressUpdate::percent(100.0)).await;

    let size = std::fs::metadata(output)?.len();
    Ok(size)
```

Replace with:

```rust
    std::fs::rename(&part_path, output)?;

    let size = std::fs::metadata(output)?.len();
    if size == 0 {
        let _ = std::fs::remove_file(output);
        anyhow::bail!("direct download produced no data (0 bytes)");
    }
    let _ = progress_tx.send(ProgressUpdate::percent(100.0)).await;
    Ok(size)
```

### 3b. `src-tauri/omniget-core/src/core/hls_downloader.rs` (~line 352)

Find:

```rust
        std::fs::rename(&part_path, &output)?;

        let file_size = std::fs::metadata(&output)?.len();
        let protection_sidecar_path = if let Some(ref protected) = protected_media {
```

Replace the first two lines so an empty mux fails — but only when this is NOT a protected
passthrough (a protected sidecar legitimately produces a tiny/zero main file):

```rust
        std::fs::rename(&part_path, &output)?;

        let file_size = std::fs::metadata(&output)?.len();
        if file_size == 0 && protected_media.is_none() {
            let _ = std::fs::remove_file(&output);
            anyhow::bail!("HLS download produced no data (0 bytes)");
        }
        let protection_sidecar_path = if let Some(ref protected) = protected_media {
```

**Check:** `cd src-tauri && cargo check` (this compiles omniget-core too).

---

## Phase 4 — Frontend: course aggregate completion guard + honest saved-bytes

File: `src/lib/stores/download-listener.ts`

### 4a. `download-complete` listener (around line 191-208)

Find:

```ts
  const unlistenComplete = await listen<CompletePayload>("download-complete", (event) => {
    const d = event.payload;
    markComplete(d.course_name, d.success, d.error ?? undefined);

    const tr = get(t);
    if (d.success) {
      showToast("success", tr("toast.download_complete", { name: d.course_name }));
      void notifyComplete(d.course_name);
      addLog("info", "download", `Course download complete: ${d.course_name}`);
      recordDownloadComplete(0);
      void rpcSyncIdleStats();
    } else {
```

Replace with (treat a "success" with 0 downloaded bytes as failure; record real bytes):

```ts
  const unlistenComplete = await listen<CompletePayload>("download-complete", (event) => {
    const d = event.payload;
    const item = getDownloads().get(d.course_id);
    const bytes = item && item.kind === "course" ? item.bytesDownloaded : 0;
    const reallySucceeded = d.success && bytes > 0;
    markComplete(
      d.course_name,
      reallySucceeded,
      reallySucceeded
        ? undefined
        : (d.error ?? "Download produced no files (0 bytes). Retry after checking access."),
    );

    const tr = get(t);
    if (reallySucceeded) {
      showToast("success", tr("toast.download_complete", { name: d.course_name }));
      void notifyComplete(d.course_name);
      addLog("info", "download", `Course download complete: ${d.course_name}`);
      recordDownloadComplete(bytes);
      void rpcSyncIdleStats();
    } else {
```

> Note: the original `else {` branch (the failure toast/log) stays as-is; it now also catches
> the downgraded "empty success" case.

If `getDownloads` is not already imported in this file, add it to the existing import from
`download-store.svelte` (the file already imports `upsertProgress`, `markComplete`).

### 4b. `udemy-download-complete` listener (around line 234-236)

Find:

```ts
  const unlistenUdemyComplete = await listen<UdemyCompletePayload>("udemy-download-complete", (event) => {
    const d = event.payload;
    markComplete(d.course_name, d.success, d.error ?? undefined);
```

Replace the `markComplete(...)` line with the same byte-aware guard:

```ts
  const unlistenUdemyComplete = await listen<UdemyCompletePayload>("udemy-download-complete", (event) => {
    const d = event.payload;
    const uItem = getDownloads().get(d.course_id);
    const uBytes = uItem && uItem.kind === "course" ? uItem.bytesDownloaded : 0;
    const uSucceeded = d.success && uBytes > 0;
    markComplete(
      d.course_name,
      uSucceeded,
      uSucceeded ? undefined : (d.error ?? "Download produced no files (0 bytes). Retry after checking access."),
    );
```

If this listener also calls `recordDownloadComplete(0)` on success below, change that argument
to `uBytes` (search the rest of this listener block for `recordDownloadComplete`).

**Check:** `pnpm check`

---

## Phase 5 — Frontend: filter chip counts include all items

File: `src/routes/downloads/+page.svelte`

Find (around line 107):

```svelte
  let filterCounts = $derived({
    all: genericList.length,
    active: grouped.active.length + grouped.paused.length,
    queued: grouped.queued.length,
    completed: grouped.completed.length,
    failed: grouped.errored.length,
  });
```

Replace with a single source of truth that counts BOTH course and generic items by status:

```svelte
  let filterCounts = $derived.by(() => {
    let all = 0, active = 0, queued = 0, completed = 0, failed = 0;
    for (const d of downloads.values()) {
      all++;
      switch (d.status) {
        case "downloading":
        case "seeding":
        case "paused":
          active++;
          break;
        case "queued":
          queued++;
          break;
        case "complete":
          completed++;
          break;
        case "error":
          failed++;
          break;
      }
    }
    return { all, active, queued, completed, failed };
  });
```

(`downloads` is the existing `$derived(getDownloads())` at line 76 and already includes course
items. This mirrors the logic in `getCounts()` in the store, which is the canonical counter.)

**Check:** `pnpm check`

---

## Phase 6 — Tests

### 6a. Rust (queue guard) — add to the `#[cfg(test)]` module in `src-tauri/src/core/queue.rs`:

- A test asserting `is_retryable_error_message(EMPTY_DOWNLOAD_ERROR) == true`.
- If there is an existing test that drives `mark_complete`, add one that calls
  `mark_complete(id, false, Some(EMPTY_DOWNLOAD_ERROR.into()), None, None)` and asserts the
  item's status is `QueueStatus::Error { retryable: true, .. }`.

### 6b. Rust (leaf downloaders): mirror the existing `verify_nonempty_output` test style in
`omniget-core/src/core/ytdlp.rs` — a small test writing a 0-byte file and asserting the new
`bail!`s fire. Only add if a straightforward unit hook exists; otherwise rely on 6a.

**Check:** `cd src-tauri && cargo test`

---

## Final verification (end-to-end — required, not synthetic-only)

1. `cd src-tauri && cargo test` passes; `pnpm check` clean.
2. `source .venv/bin/activate && pnpm tauri dev`.
3. Trigger a download that yields 0 bytes (a DRM lecture without valid `.wvd`/cookies):
   - Card shows **FAILED** with the empty-download message and a **Retry** button — NOT
     COMPLETE/100%.
4. Trigger a working download:
   - Card shows COMPLETE with real bytes; header "N downloads · X saved" shows non-zero X.
5. Filter chips (`All/Active/Queued/Completed/Failed`) show non-zero counts matching the
   visible cards.
6. Click **Clear finished** → the failed/empty items are removed. A stuck `DOWNLOADING 0%`
   item can be cancelled (X) and then cleared.
7. No `NaN%` anywhere; previously-working platforms still download (no regression).

## Files touched (summary)
- `src-tauri/src/core/queue.rs` — empty-output guard + `EMPTY_DOWNLOAD_ERROR` const + test.
- `src-tauri/src/commands/host_queue.rs` — empty-success downgrade in `report_complete_inner`.
- `src-tauri/omniget-core/src/core/direct_downloader.rs` — 0-byte bail.
- `src-tauri/omniget-core/src/core/hls_downloader.rs` — 0-byte bail (non-protected only).
- `src/lib/stores/download-listener.ts` — course completion byte-guard + real `recordDownloadComplete`.
- `src/routes/downloads/+page.svelte` — `filterCounts` counts all items.

## Out of scope
- The Udemy Widevine pipeline itself (already ported).
- Study-tab Portuguese localization.
- Any new multi-select/bulk-delete UI beyond the existing "Clear finished".
