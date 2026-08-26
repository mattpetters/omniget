import test from "node:test";
import assert from "node:assert/strict";
import {
  captureMwtmParts,
  getMwtmPartNumber,
  isMixWithTheMastersUrl,
  isMwtmCourseUrl,
  isMwtmPlaylistUrl,
} from "../src/mwtm-capture.js";

const ROOT = "https://mixwiththemasters.com/videos/example-course";

test("recognizes MWTM course, part, and signed playlist URLs", () => {
  assert.equal(isMixWithTheMastersUrl(ROOT), true);
  assert.equal(isMwtmCourseUrl(ROOT), true);
  assert.equal(isMwtmCourseUrl(`${ROOT}/part/2`), false);
  assert.equal(getMwtmPartNumber(`${ROOT}/part`), 1);
  assert.equal(getMwtmPartNumber(`${ROOT}/part/3`), 3);
  assert.equal(isMwtmPlaylistUrl("https://mixwiththemasters.com/videos/_/playlist/example-course/part/1/Index.m3u8?_hash=x"), true);
  assert.equal(isMwtmPlaylistUrl("https://example.com/video.m3u8"), false);
});

test("captures every paid part and closes temporary tabs", async () => {
  const created = [];
  const removed = [];
  const snapshots = new Map([
    [10, {
      pageUrl: ROOT,
      courseUrl: ROOT,
      currentPart: 1,
      playlistUrl: "https://mixwiththemasters.com/videos/_/playlist/example-course/part/1/Index.m3u8?_hash=one",
      partUrls: [
        { part: 1, pageUrl: `${ROOT}/part` },
        { part: 2, pageUrl: `${ROOT}/part/2` },
        { part: 3, pageUrl: `${ROOT}/part/3` },
        { part: 0, pageUrl: `${ROOT}/part/0` },
      ],
      title: "Example Course",
      thumbnail: "https://example.com/thumb.jpg",
    }],
    [101, {
      playlistUrl: "https://mixwiththemasters.com/videos/_/playlist/example-course/part/2/Index.m3u8?_hash=two",
      title: "Example Course",
      thumbnail: "",
    }],
    [102, {
      playlistUrl: "https://mixwiththemasters.com/videos/_/playlist/example-course/part/3/Index.m3u8?_hash=three",
      title: "Example Course",
      thumbnail: "",
    }],
  ]);
  let nextId = 101;
  const chromeApi = {
    scripting: {
      executeScript: async ({ target }) => [{ result: snapshots.get(target.tabId) }],
    },
    tabs: {
      create: async ({ url, active }) => {
        created.push({ url, active });
        return { id: nextId++ };
      },
      remove: async (id) => { removed.push(id); },
    },
  };

  const parts = await captureMwtmParts({
    chromeApi,
    tabId: 10,
    pageUrl: ROOT,
    attempts: 1,
    intervalMs: 0,
  });

  assert.deepEqual(parts.map((part) => part.part), [1, 2, 3]);
  assert.deepEqual(created, [
    { url: `${ROOT}/part/2`, active: false },
    { url: `${ROOT}/part/3`, active: false },
  ]);
  assert.deepEqual(removed, [101, 102]);
});

test("a specific part captures only the active lesson", async () => {
  const chromeApi = {
    scripting: {
      executeScript: async () => [{ result: {
        currentPart: 2,
        playlistUrl: "https://mixwiththemasters.com/videos/_/playlist/example-course/part/2/Index.m3u8?_hash=two",
        title: "Example Course",
        thumbnail: "",
      } }],
    },
    tabs: {
      create: async () => { throw new Error("must not create a tab"); },
      remove: async () => {},
    },
  };

  const parts = await captureMwtmParts({
    chromeApi,
    tabId: 10,
    pageUrl: `${ROOT}/part/2`,
    attempts: 1,
    intervalMs: 0,
  });
  assert.deepEqual(parts.map((part) => part.part), [2]);
});

test("falls back to the network detector when the player hides its source", async () => {
  const playlist = "https://mixwiththemasters.com/videos/_/playlist/example-course/part/1/Index.m3u8?_hash=one";
  const chromeApi = {
    scripting: {
      executeScript: async () => [{ result: {
        currentPart: 1,
        playlistUrl: null,
        title: "Example Course",
        thumbnail: "",
      } }],
    },
    tabs: {
      create: async () => { throw new Error("must not create a tab"); },
      remove: async () => {},
    },
  };

  const parts = await captureMwtmParts({
    chromeApi,
    tabId: 10,
    pageUrl: `${ROOT}/part`,
    getDetectedPlaylist: async () => playlist,
    attempts: 1,
    intervalMs: 0,
  });
  assert.equal(parts[0].playlistUrl, playlist);
});
