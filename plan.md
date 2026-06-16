# Handoff Plan: DRM Completion, Empty Output, and Course-ID Fixes

Audience: smaller implementation models working from this repo. Treat this file as the current
source of truth. The red-test checkpoint is already committed:

```sh
git show --name-only --oneline 4cd95f90
```

Expected commit:

```text
4cd95f90 test(downloads): add red DRM completion contracts
```

Do not remove or weaken the tests in that commit. Implement until those tests pass, then run the
full verification commands at the end.

## Current Red Tests

The red tests intentionally describe the contract before implementation.

Backend:

- `queue_empty_output_is_retryable_error`
- `host_queue_empty_success_is_retryable_error`
- `protected_hls_saved_encrypted_becomes_needs_decryption`
- `protected_hls_saved_encrypted_becomes_needs_decryption_history_row`

Frontend:

- `course_completion_requires_course_id`
- `course_completion_without_course_id_is_ignored`
- `downloads_filter_counts_include_courses_and_needs_decryption`
- `clear_finished_clears_frontend_course_terminal_items`

Run the red checks before implementing:

```sh
cd src-tauri && cargo test completion_contract_tests
cargo test host_queue_empty_success_is_retryable_error
cargo test protected_hls_saved_encrypted_becomes_needs_decryption_history_row
cd .. && pnpm test src/lib/stores/download-store.svelte.test.ts
```

Known red failures on the checkpoint:

- Rust compile failures for missing `QueueStatus::NeedsDecryption`,
  `DownloadQueue::mark_needs_decryption`, `QueueItem.protection_sidecar_path`,
  `HistoryEntry.status`, `HistoryEntry.protection_sidecar_path`,
  `ReportCompleteArgs.decryption_status`, `ReportCompleteArgs.protection_sidecar_path`, and
  host completion classification helper.
- Frontend Vitest failures for missing `markCompleteById` and current name-based completion.

These failures are good. They prove the tests are pointed at the missing contract.

## Contract To Implement

### Queue Status

Add a distinct terminal attention state:

```rust
QueueStatus::NeedsDecryption { message, sidecar_path }
```

Add TypeScript status:

```ts
"needs_decryption"
```

This is not `complete` and not retryable `error`. It should be clearable as finished/terminal, but
should not show Retry.

### Empty Output

Unprotected successful downloads with a missing output file or `file_size_bytes == 0` must become a
retryable error:

- Backend internal queue completion: retryable `Error`, percent not forced to `100`.
- Host/external queue completion: `report_complete_inner(success: true, file_size_bytes: Some(0))`
  becomes retryable `Error`, not `Complete`.
- Do not record a successful history row for empty unprotected output.
- Do not increment saved-byte stats for empty failures.

Use one shared message/constant for this path so tests and UI behavior remain stable.

### Protected HLS / DRM

Protected HLS saved encrypted is not playable completion. It should become `needs_decryption`:

- Preserve encrypted sidecar behavior.
- Carry `protected_media` and `protection_sidecar_path` from HLS result into the queue decision.
- Persist history with `status = "needs_decryption"` and `protection_sidecar_path`.
- Hydrate history back into `QueueStatus::NeedsDecryption`.
- UI should label it “Needs decryption” and show the message/sidecar path.

Udemy Widevine decrypted MP4 must still be normal `complete`, but only when the final playable file
exists and has nonzero size.

### Course Completion Events

Course completion must be ID-based:

- Add/require `course_id` on completion event payload types:
  `download-complete`, `udemy-download-complete`, and platform-specific equivalents if found.
- Frontend must call `markCompleteById(courseId, ...)`, never match by course name.
- Malformed completion without `course_id` must be ignored/logged, not name-matched.

### Downloads Store / UI

- Add `markCompleteById`.
- `clearFinished()` must remove `complete`, `error`, and `needs_decryption` course/generic items.
- Counts must include both generic queue items and course items.
- Count `needs_decryption` under finished/attention/failed-style counts, not completed.
- Retry button only for retryable errors, not `needs_decryption`.
- Clear finished should call the backend and then clear frontend-only terminal course items.
- Avoid `NaN%`; do not force empty/protected pending-decryption items to `100%`.

### History / Database

Add explicit persisted status while keeping legacy `success` compatibility:

- `success=true` legacy rows map to `complete`.
- `success=false` legacy rows map to `error`.
- New protected encrypted rows persist `needs_decryption`.
- Add `protection_sidecar_path`.
- UI should read explicit `status`, not infer everything from `success`.

## Suggested Implementation Order

1. Add backend types/fields:
   - `QueueStatus::NeedsDecryption`.
   - `QueueItem.protection_sidecar_path`.
   - `QueueItemInfo.protection_sidecar_path`.
   - History `status` and `protection_sidecar_path` with SQLite migration.
2. Add queue helpers:
   - Shared empty-output message.
   - `mark_needs_decryption`.
   - Completion classification for native queue path.
3. Add host/external completion classification:
   - Extend `ReportCompleteArgs` with optional protected metadata.
   - Empty success -> retryable `Error`.
   - Protected metadata -> `NeedsDecryption`.
4. Carry HLS protected metadata:
   - Make `ProtectedMediaInfo` serializable/deserializable if needed.
   - Extend `DownloadResult`.
   - Populate fields from protected HLS SaveEncrypted paths.
   - Keep Udemy Widevine decrypted output plain complete.
5. Update frontend store/listener/UI:
   - `markCompleteById`.
   - completion payloads with `course_id`.
   - `needs_decryption` status mapping and sidecar display.
   - counts and clear-finished behavior.
6. Update i18n keys for:
   - `downloads.status.needs_decryption`
   - any detail/label text used by UI.

Keep changes tightly scoped. Do not refactor unrelated queue, history, or course UI code.

## Verification Commands

Run these after implementation:

```sh
cd src-tauri && cargo test
cd src-tauri && cargo check
pnpm test src/lib/stores/download-store.svelte.test.ts
pnpm check
```

If a command fails because dependencies/tooling need network access, request approval rather than
skipping it.

Manual acceptance:

- Empty skipped downloads show retryable Error, never Complete/100%.
- Protected HLS saved encrypted shows Needs decryption with sidecar metadata.
- Udemy Widevine successful decrypt produces playable nonzero MP4 and shows Complete.
- Course completion updates by `course_id`, not name.
- Completion without `course_id` is ignored/logged as malformed.
- Counts include visible course and generic items.
- Saved bytes count playable completed downloads only.
- Clear finished removes backend terminal items and frontend course terminal items.
- Retry appears for retryable Error, not for Needs decryption.
- No `NaN%`.

## Worktree Setup For Multiple Models

From the main repo:

```sh
cd /Users/mattpetters/code/omniget
git fetch --all --prune
```

Create one worktree per model from the red-test checkpoint:

```sh
git worktree add ../omniget-model-a 4cd95f90 -b impl/drm-completion-model-a
git worktree add ../omniget-model-b 4cd95f90 -b impl/drm-completion-model-b
git worktree add ../omniget-model-c 4cd95f90 -b impl/drm-completion-model-c
```

Then hand each model a separate directory:

```sh
cd ../omniget-model-a
git status --short
cd src-tauri && cargo test completion_contract_tests
cd .. && pnpm test src/lib/stores/download-store.svelte.test.ts
```

Those tests should be red before the model starts implementing.

To compare a model’s solution later:

```sh
cd /Users/mattpetters/code/omniget
git diff 4cd95f90..impl/drm-completion-model-a --stat
git diff 4cd95f90..impl/drm-completion-model-a
```

To remove a completed/abandoned worktree:

```sh
git worktree remove ../omniget-model-a
git branch -D impl/drm-completion-model-a
```
