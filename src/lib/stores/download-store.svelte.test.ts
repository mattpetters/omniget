import { beforeEach, describe, expect, it, vi } from "vitest";

let store: any;

beforeEach(() => {
  vi.resetModules();
  vi.stubGlobal("$state", <T>(value: T) => value);
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    cb(0);
    return 0;
  });
});

describe("course completion contract", () => {
  beforeEach(async () => {
    store = await import("./download-store.svelte");
  });

  it("course_completion_requires_course_id", () => {
    store.upsertProgress(1, "Duplicate course", 12, "Module A", "Lesson A", 100, 2, 0, 1, 1);
    store.upsertProgress(2, "Duplicate course", 40, "Module B", "Lesson B", 200, 2, 1, 1, 1);

    store.markCompleteById(2, true);

    expect(store.getDownloads().get(1)?.status).toBe("downloading");
    expect(store.getDownloads().get(2)?.status).toBe("complete");
    expect(store.getDownloads().get(1)?.percent).toBe(12);
    expect(store.getDownloads().get(2)?.percent).toBe(100);
  });

  it("course_completion_without_course_id_is_ignored", () => {
    store.upsertProgress(1, "Duplicate course", 12, "Module A", "Lesson A", 100, 2, 0, 1, 1);
    store.upsertProgress(2, "Duplicate course", 40, "Module B", "Lesson B", 200, 2, 1, 1, 1);

    store.markComplete("Duplicate course", true);

    expect(store.getDownloads().get(1)?.status).toBe("downloading");
    expect(store.getDownloads().get(2)?.status).toBe("downloading");
  });

  it("downloads_filter_counts_include_courses_and_needs_decryption", () => {
    store.upsertProgress(1, "Complete course", 100, "Module", "Lesson", 100, 1, 1, 1, 1);
    store.upsertProgress(2, "Active course", 20, "Module", "Lesson", 25, 1, 0, 1, 1);
    store.markCompleteById(1, true);
    store.syncQueueState([
      {
        id: 10,
        url: "https://example.test/encrypted.m3u8",
        platform: "generic_ytdlp",
        title: "Encrypted HLS",
        status: {
          type: "NeedsDecryption",
          data: {
            message: "Protected media was saved encrypted and needs decryption",
            sidecar_path: "/tmp/encrypted.mp4.protection.json",
          },
        },
        percent: 0,
        speed_bytes_per_sec: 0,
        downloaded_bytes: 4096,
        total_bytes: null,
        file_path: "/tmp/encrypted.mp4",
        file_size_bytes: 4096,
        file_count: null,
        protection_sidecar_path: "/tmp/encrypted.mp4.protection.json",
        thumbnail_url: null,
      },
    ]);

    const counts = store.getCounts();
    expect(counts.active).toBe(1);
    expect(counts.finished).toBe(2);
    expect(store.getDownloads().get(10)?.status).toBe("needs_decryption");
  });

  it("clear_finished_clears_frontend_course_terminal_items", () => {
    store.upsertProgress(1, "Complete course", 100, "Module", "Lesson", 100, 1, 1, 1, 1);
    store.upsertProgress(2, "Failed course", 45, "Module", "Lesson", 50, 1, 0, 1, 1);
    store.upsertProgress(3, "Encrypted course", 45, "Module", "Lesson", 50, 1, 0, 1, 1);
    store.upsertProgress(4, "Active course", 20, "Module", "Lesson", 25, 1, 0, 1, 1);

    store.markCompleteById(1, true);
    store.markCompleteById(2, false, "Network failed");
    store.markCompleteById(3, false, "Protected media needs decryption", "needs_decryption");

    store.clearFinished();

    expect([...store.getDownloads().keys()]).toEqual([4]);
    expect(store.getDownloads().get(4)?.status).toBe("downloading");
  });
});
