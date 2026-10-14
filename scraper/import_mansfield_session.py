#!/usr/bin/env python3
"""Build mansfield-storage-state.json from cookies exported by a real browser.

Xvfb / Docker Chromium cannot pass InvoiceCloud's reCAPTCHA Enterprise score check.
Log in once on any normal desktop browser, export cookies, then import them here.

1. On your laptop/desktop browser, open the Mansfield InvoiceCloud portal and Sign In.
2. Install "Cookie-Editor" (Chrome/Firefox) → export cookies for invoicecloud.com as JSON.
3. Save the file as ~/energy-invoices/mansfield-cookies.json
4. Run:

  docker compose -f ~/docker-compose.yml run --rm --entrypoint '' \\
    -e ENERGY_INVOICE_DIR=/invoices \\
    energy-scraper python import_mansfield_session.py /invoices/mansfield-cookies.json
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
from pathlib import Path

from playwright.async_api import async_playwright

from config import settings
from portals import _is_mansfield_login_page, _mansfield_state_path


def _invoice_dir() -> Path:
    if os.environ.get("ENERGY_INVOICE_DIR"):
        return Path(os.environ["ENERGY_INVOICE_DIR"])
    if settings.energy_invoice_dir.exists():
        return settings.energy_invoice_dir
    host = Path.home() / "energy-invoices"
    return host if host.is_dir() else settings.energy_invoice_dir


def _same_site(raw: object) -> str:
    text = str(raw or "Lax").strip().lower().replace("_", "-")
    if text in ("no-restriction", "none", "unspecified"):
        return "None"
    if text == "strict":
        return "Strict"
    return "Lax"


def _as_cookie_list(payload: object) -> list[dict]:
    if isinstance(payload, list):
        return payload
    if isinstance(payload, dict):
        if isinstance(payload.get("cookies"), list):
            return payload["cookies"]
        # Single cookie object
        if "name" in payload and "value" in payload:
            return [payload]
    raise ValueError("Expected a Cookie-Editor JSON array, or Playwright storage_state object")


def cookies_to_storage_state(payload: object) -> dict:
    cookies_out: list[dict] = []
    for raw in _as_cookie_list(payload):
        name = raw.get("name")
        value = raw.get("value")
        if not name or value is None:
            continue
        domain = raw.get("domain") or ".invoicecloud.com"
        path = raw.get("path") or "/"
        expires = raw.get("expirationDate", raw.get("expires", -1))
        try:
            expires = int(float(expires))
        except (TypeError, ValueError):
            expires = -1
        # Cookie-Editor uses seconds; leave -1 for session cookies.
        if expires > 10_000_000_000:  # ms → s
            expires //= 1000
        cookies_out.append(
            {
                "name": name,
                "value": value,
                "domain": domain,
                "path": path,
                "expires": expires,
                "httpOnly": bool(raw.get("httpOnly", False)),
                "secure": bool(raw.get("secure", True)),
                "sameSite": _same_site(raw.get("sameSite", "Lax")),
            }
        )
    if not cookies_out:
        raise ValueError("No cookies found in export")
    origins = []
    if isinstance(payload, dict) and isinstance(payload.get("origins"), list):
        origins = payload["origins"]
    return {"cookies": cookies_out, "origins": origins}


async def _verify(state_path: Path) -> bool:
    """Headless check: still logged in? (no captcha solve attempted)."""
    playwright = await async_playwright().start()
    browser = await playwright.chromium.launch(
        headless=True,
        args=["--disable-blink-features=AutomationControlled", "--no-sandbox"],
    )
    try:
        context = await browser.new_context(storage_state=str(state_path), locale="en-US")
        page = await context.new_page()
        await page.goto(settings.mansfield_portal_url, wait_until="domcontentloaded", timeout=90_000)
        await page.wait_for_timeout(2_000)
        ok = not _is_mansfield_login_page(page.url)
        print(f"Verify: url={page.url}")
        print("Verify: session looks logged in" if ok else "Verify: still on login page (cookies incomplete or expired)")
        return ok
    finally:
        await browser.close()
        await playwright.stop()


def main() -> int:
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else _invoice_dir() / "mansfield-cookies.json"
    if not src.is_file():
        print(f"Cookie file not found: {src}", file=sys.stderr)
        print(__doc__)
        return 1

    payload = json.loads(src.read_text())
    state = cookies_to_storage_state(payload)
    out_dir = _invoice_dir()
    out_dir.mkdir(parents=True, exist_ok=True)
    state_path = _mansfield_state_path(out_dir)
    state_path.write_text(json.dumps(state, indent=2))
    print(f"Wrote {len(state['cookies'])} cookies → {state_path}")

    skip_verify = os.environ.get("SKIP_VERIFY", "").lower() in ("1", "true", "yes")
    if skip_verify:
        return 0
    ok = asyncio.run(_verify(state_path))
    return 0 if ok else 2


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--self-check":
        sample = [
            {
                "name": "a",
                "value": "b",
                "domain": ".invoicecloud.com",
                "path": "/",
                "expirationDate": 2000000000,
                "httpOnly": True,
                "secure": True,
                "sameSite": "no_restriction",
            }
        ]
        state = cookies_to_storage_state(sample)
        assert state["cookies"][0]["sameSite"] == "None"
        assert state["cookies"][0]["expires"] == 2000000000
        print("self-check ok")
        raise SystemExit(0)
    raise SystemExit(main())
