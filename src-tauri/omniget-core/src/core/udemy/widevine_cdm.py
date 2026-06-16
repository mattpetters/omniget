#!/usr/bin/env python3
"""
Widevine CDM challenge/license exchange helper for omniget's Udemy DRM pipeline.
Ported from mattpetters/udemy-dl (scripts/widevine_cdm.py); the only change is
that the license-server URL is parameterized (--license-url) so it works for any
Udemy portal host (www.udemy.com or an enterprise {org}.udemy.com), not just a
single hardcoded host.

Usage:
    python3 widevine_cdm.py --pssh <base64-pssh> --token <media_license_token> \
        [--license-url <url>] [--wvd <path>]

Outputs one kid:key pair per line to stdout. Exits non-zero on failure.

Requires:
    pip install pywidevine
(pywidevine pulls a compatible protobuf automatically — do NOT pin protobuf, no
PyPI pywidevine release is compatible with a hand-pinned protobuf==5.29.4.)

The .wvd (Widevine Device) file is a user-supplied L3 CDM blob created with:
    pywidevine create-device -t android -l 3 -k private_key.pem -c client_id.bin
"""

import argparse
import os
import random
import sys
import time
import urllib.error
import urllib.request

try:
    from pywidevine.cdm import Cdm
    from pywidevine.device import Device
    from pywidevine.pssh import PSSH
except ImportError:
    print("pywidevine not installed. Run: pip install pywidevine", file=sys.stderr)
    sys.exit(1)

DEFAULT_LICENSE_URL = "https://www.udemy.com/media-license-server/validate-auth-token"

# License-server retry policy. When downloading many DRM lectures back-to-back,
# Udemy's media-license endpoint throttles the burst of Widevine license
# requests and replies with HTTP 401 (NOT 429 — which is what makes it look
# like an auth/CDM problem; it isn't). The same token + CDM succeed once the
# burst window passes. Treat 401/429/5xx as retryable with exponential backoff;
# 403 is NOT retryable (that's the WAF/UA case).
RETRYABLE_STATUSES = {401, 408, 429, 500, 502, 503, 504}
MAX_ATTEMPTS = 6
BASE_DELAY = 4.0  # seconds; doubles each retry → 4, 8, 16, 32, 64 (~124s max)

DEFAULT_WVD_SEARCH_PATHS = [
    os.path.expanduser("~/.config/udemy-dl/device.wvd"),
    os.path.expanduser("~/Library/Application Support/udemy-dl/device.wvd"),
    os.path.expanduser("~/.config/omniget/device.wvd"),
    os.path.join(os.path.dirname(__file__), "device.wvd"),
    "device.wvd",
]


def find_wvd(explicit=None):
    if explicit:
        if os.path.exists(explicit):
            return explicit
        print(f"WVD file not found: {explicit}", file=sys.stderr)
        sys.exit(1)
    for path in DEFAULT_WVD_SEARCH_PATHS:
        if os.path.exists(path):
            return path
    print(
        "No .wvd device file found. Tried:\n" + "\n".join(f"  {p}" for p in DEFAULT_WVD_SEARCH_PATHS),
        file=sys.stderr,
    )
    print(
        "\nCreate one with: pywidevine create-device -t android -l 3 -k private_key.pem -c client_id.bin",
        file=sys.stderr,
    )
    sys.exit(1)


def fetch_license(challenge: bytes, auth_token: str, license_url: str) -> bytes:
    url = f"{license_url}?drm_type=widevine&auth_token={auth_token}"
    headers = {
        "Content-Type": "application/octet-stream",
        # Udemy's CloudFront WAF rejects the default "Python-urllib/x.y" UA with
        # 403 (and browser UAs too). A plain curl UA is accepted. This MUST also
        # match the UA the API used to fetch the token (embedded in the JWT) or
        # the server returns 401. See module docs in core/udemy/mod.rs.
        "User-Agent": "curl/8.7.1",
    }

    last_exc = None
    for attempt in range(MAX_ATTEMPTS):
        req = urllib.request.Request(url, data=challenge, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req) as resp:
                return resp.read()
        except urllib.error.HTTPError as e:
            last_exc = e
            retryable = e.code in RETRYABLE_STATUSES
        except urllib.error.URLError as e:
            last_exc = e
            retryable = True

        if not retryable or attempt == MAX_ATTEMPTS - 1:
            break

        delay = BASE_DELAY * (2 ** attempt) + random.uniform(0, 1.5)
        print(
            f"License request transient failure ({last_exc}); backing off "
            f"{delay:.1f}s then retrying (attempt {attempt + 1}/{MAX_ATTEMPTS})",
            file=sys.stderr,
        )
        time.sleep(delay)

    raise last_exc


def main():
    parser = argparse.ArgumentParser(description="Widevine CDM helper for omniget Udemy DRM")
    parser.add_argument("--pssh", required=True, help="Base64-encoded Widevine PSSH box")
    parser.add_argument("--token", required=True, help="Udemy media_license_token")
    parser.add_argument(
        "--license-url",
        default=DEFAULT_LICENSE_URL,
        help="Udemy media-license-server validate-auth-token URL",
    )
    parser.add_argument("--wvd", help="Path to .wvd device file")
    args = parser.parse_args()

    wvd_path = find_wvd(args.wvd)

    device = Device.load(wvd_path)
    cdm = Cdm.from_device(device)
    session_id = cdm.open()

    try:
        pssh = PSSH(args.pssh)
        challenge = cdm.get_license_challenge(session_id, pssh, privacy_mode=False)
    except Exception as e:
        print(f"Failed to build license challenge: {e}", file=sys.stderr)
        sys.exit(1)

    try:
        license_bytes = fetch_license(challenge, args.token, args.license_url)
    except Exception as e:
        print(f"License server request failed: {e}", file=sys.stderr)
        sys.exit(1)

    try:
        cdm.parse_license(session_id, license_bytes)
    except Exception as e:
        print(f"Failed to parse license response: {e}", file=sys.stderr)
        sys.exit(1)

    # Access sessions directly — cdm.get_keys() is broken in Python 3.14+
    session = cdm._Cdm__sessions.get(session_id)
    if session is None:
        print("No session found after parse_license", file=sys.stderr)
        sys.exit(1)

    keys = [
        f"{k.kid.hex}:{k.key.hex()}"
        for k in session.keys
        if k.type == "CONTENT"
    ]

    if not keys:
        print("No CONTENT keys found in license response", file=sys.stderr)
        sys.exit(1)

    for key in keys:
        print(key)

    cdm.close(session_id)


if __name__ == "__main__":
    main()
