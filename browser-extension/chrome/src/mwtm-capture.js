const MWTM_HOST = "mixwiththemasters.com";
const PLAYLIST_MARKER = "/videos/_/playlist/";

export function isMixWithTheMastersUrl(rawUrl) {
  try {
    const host = new URL(rawUrl).hostname.toLowerCase();
    return host === MWTM_HOST || host.endsWith(`.${MWTM_HOST}`);
  } catch {
    return false;
  }
}

export function getMwtmPartNumber(rawUrl) {
  try {
    const match = new URL(rawUrl).pathname.match(/\/part(?:\/(\d+))?\/?$/i);
    if (!match) return null;
    return match[1] ? Number.parseInt(match[1], 10) : 1;
  } catch {
    return null;
  }
}

export function isMwtmCourseUrl(rawUrl) {
  if (!isMixWithTheMastersUrl(rawUrl)) return false;
  try {
    return /\/videos\/[^/]+\/?$/i.test(new URL(rawUrl).pathname);
  } catch {
    return false;
  }
}

export function isMwtmPlaylistUrl(rawUrl) {
  try {
    const parsed = new URL(rawUrl);
    return isMixWithTheMastersUrl(rawUrl)
      && parsed.pathname.includes(PLAYLIST_MARKER)
      && parsed.pathname.toLowerCase().endsWith(".m3u8");
  } catch {
    return false;
  }
}

// This function is serialized by chrome.scripting.executeScript and therefore
// must stay self-contained: it cannot close over module helpers or constants.
export function readMwtmPlayerSnapshot() {
  const pageUrl = window.location.href;
  const pagePath = window.location.pathname;
  const partMatch = pagePath.match(/\/part(?:\/(\d+))?\/?$/i);
  const currentPart = partMatch ? (partMatch[1] ? Number.parseInt(partMatch[1], 10) : 1) : 1;
  const courseUrl = pageUrl.replace(/\/part(?:\/\d+)?\/?(?:[?#].*)?$/i, "").replace(/[?#].*$/, "");

  const playlistCandidates = [];
  for (const source of document.querySelectorAll("video source, audio source, source")) {
    if (source.src) playlistCandidates.push(source.src);
  }
  for (const media of document.querySelectorAll("video, audio")) {
    if (media.currentSrc) playlistCandidates.push(media.currentSrc);
    if (media.src) playlistCandidates.push(media.src);
  }
  const playlistUrl = playlistCandidates.find((candidate) => {
    try {
      const parsed = new URL(candidate, pageUrl);
      return parsed.pathname.includes("/videos/_/playlist/")
        && parsed.pathname.toLowerCase().endsWith(".m3u8");
    } catch {
      return false;
    }
  }) || null;

  const partUrls = [];
  for (const anchor of document.querySelectorAll('a[href*="/videos/"][href*="/part"]')) {
    try {
      const target = new URL(anchor.href, pageUrl);
      if (target.hostname !== window.location.hostname) continue;
      const match = target.pathname.match(/\/part(?:\/(\d+))?\/?$/i);
      if (!match) continue;
      const part = match[1] ? Number.parseInt(match[1], 10) : 1;
      if (!Number.isFinite(part) || part === 0) continue;
      target.hash = "";
      target.search = "";
      partUrls.push({ part, pageUrl: target.href });
    } catch {}
  }

  const title = document.title
    .replace(/\s+-\s+Videos\s+-\s+Mix with the Masters.*$/i, "")
    .trim() || "Mix With The Masters";
  const thumbnail = document.querySelector('meta[property="og:image"]')?.content
    || document.querySelector('meta[name="twitter:image"]')?.content
    || "";

  return {
    pageUrl,
    courseUrl,
    currentPart,
    playlistUrl,
    partUrls,
    title,
    thumbnail,
  };
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function readSnapshot(
  chromeApi,
  tabId,
  pageUrl,
  attempts,
  intervalMs,
  getDetectedPlaylist,
) {
  let lastError = null;
  let lastSnapshot = null;
  for (let attempt = 0; attempt < attempts; attempt++) {
    try {
      const results = await chromeApi.scripting.executeScript({
        target: { tabId },
        func: readMwtmPlayerSnapshot,
      });
      lastSnapshot = results?.[0]?.result || lastSnapshot;
      if (lastSnapshot?.playlistUrl && isMwtmPlaylistUrl(lastSnapshot.playlistUrl)) {
        return lastSnapshot;
      }
    } catch (error) {
      lastError = error;
    }

    if (getDetectedPlaylist) {
      const detected = await getDetectedPlaylist(pageUrl);
      if (isMwtmPlaylistUrl(detected)) {
        return { ...(lastSnapshot || {}), pageUrl, playlistUrl: detected };
      }
    }
    await delay(intervalMs);
  }
  if (lastError) throw lastError;
  throw new Error("The MWTM player did not expose its authorized playlist.");
}

function makePart(snapshot, part, pageUrl) {
  return {
    part,
    pageUrl,
    playlistUrl: snapshot.playlistUrl,
    title: `${snapshot.title} - ${part === 0 ? "Trailer" : `Part ${part}`}`,
    thumbnail: snapshot.thumbnail || "",
  };
}

export async function captureMwtmParts({
  chromeApi,
  tabId,
  pageUrl,
  getDetectedPlaylist = null,
  attempts = 30,
  intervalMs = 400,
}) {
  if (!chromeApi?.scripting?.executeScript || !chromeApi?.tabs) {
    throw new Error("This browser does not support authenticated MWTM capture.");
  }

  const current = await readSnapshot(
    chromeApi,
    tabId,
    pageUrl,
    attempts,
    intervalMs,
    getDetectedPlaylist,
  );
  if (!isMwtmCourseUrl(pageUrl)) {
    const part = getMwtmPartNumber(pageUrl) ?? current.currentPart ?? 1;
    return [makePart(current, part, pageUrl)];
  }

  const partMap = new Map();
  for (const item of current.partUrls || []) {
    if (Number.isFinite(item.part) && item.part > 0 && isMixWithTheMastersUrl(item.pageUrl)) {
      partMap.set(item.part, item.pageUrl);
    }
  }
  if (!partMap.has(1)) {
    partMap.set(1, `${current.courseUrl}/part`);
  }

  const captures = [makePart(current, 1, partMap.get(1))];
  const remaining = [...partMap.entries()]
    .filter(([part]) => part !== 1)
    .sort(([a], [b]) => a - b);

  for (const [part, targetUrl] of remaining) {
    let temporaryTab = null;
    try {
      temporaryTab = await chromeApi.tabs.create({ url: targetUrl, active: false });
      if (temporaryTab?.id === undefined) {
        throw new Error(`Could not open MWTM Part ${part}.`);
      }
      const snapshot = await readSnapshot(
        chromeApi,
        temporaryTab.id,
        targetUrl,
        attempts,
        intervalMs,
        getDetectedPlaylist,
      );
      captures.push(makePart(snapshot, part, targetUrl));
    } finally {
      if (temporaryTab?.id !== undefined) {
        await chromeApi.tabs.remove(temporaryTab.id).catch(() => {});
      }
    }
  }

  return captures.sort((a, b) => a.part - b.part);
}
