import test from "node:test";
import assert from "node:assert/strict";

import {
  captureCookiesForTab,
  detectPlatformKind,
} from "../src/cookie-capture.js";

test("Patreon root and subdomains map to the Patreon cookie kind", () => {
  assert.equal(detectPlatformKind("patreon.com"), "patreon");
  assert.equal(detectPlatformKind("www.patreon.com"), "patreon");
  assert.equal(detectPlatformKind("creator.patreon.com"), "patreon");
  assert.equal(detectPlatformKind("c10.patreonusercontent.com"), "patreon");
});

test("MWTM maps to its dedicated cookie kind", () => {
  assert.equal(detectPlatformKind("mixwiththemasters.com"), "mixwiththemasters");
  assert.equal(detectPlatformKind("www.mixwiththemasters.com"), "mixwiththemasters");
});

test("manual Patreon capture queries the root domain and forwards metadata", async () => {
  const queries = [];
  let sent = null;
  const cookiesApi = {
    getAll(details, callback) {
      queries.push(details);
      callback([
        {
          domain: ".patreon.com",
          name: "session",
          value: "synthetic-session",
          path: "/",
          secure: true,
          httpOnly: true,
          hostOnly: false,
          sameSite: "lax",
          expirationDate: 123.9,
        },
      ]);
    },
  };

  const result = await captureCookiesForTab(
    {
      url: "https://www.patreon.com/FanuFatGyver/posts/mixing-drums-up-163067112",
      title: "Mixing Drums",
    },
    {
      cookiesApi,
      send: async (cookies, metadata) => {
        sent = { cookies, metadata };
        return { ok: true };
      },
    },
  );

  assert.deepEqual(queries, [{ domain: "patreon.com" }]);
  assert.equal(result.ok, true);
  assert.equal(result.domain, "patreon.com");
  assert.equal(result.cookie_count, 1);
  assert.equal(result.platform_kind, "patreon");
  assert.equal(sent.cookies[0].expires, 123);
  assert.equal(sent.metadata.sourceUrl, "https://www.patreon.com/FanuFatGyver/posts/mixing-drums-up-163067112");
  assert.equal(sent.metadata.alias, "Mixing Drums (patreon.com)");
});

test("manual Patreon capture reports an empty root-domain cookie jar", async () => {
  const result = await captureCookiesForTab(
    { url: "https://creator.patreon.com/posts/example-123" },
    {
      cookiesApi: { getAll: (_details, callback) => callback([]) },
      send: async () => ({ ok: true }),
    },
  );

  assert.deepEqual(result, {
    ok: false,
    reason: "no-cookies-for-domain",
    domain: "patreon.com",
  });
});
