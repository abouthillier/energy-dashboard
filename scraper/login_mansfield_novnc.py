#!/usr/bin/env python3
"""Optional noVNC Mansfield login — usually fails reCAPTCHA Enterprise on Xvfb.

Prefer: python import_mansfield_session.py /invoices/mansfield-cookies.json
Force this UI: MANSFIELD_FORCE_NOVNC=1 python login_mansfield.py
"""

from __future__ import annotations

import asyncio
import os
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from playwright.async_api import async_playwright

from config import settings
from flaresolverr_client import playwright_cookies, solve_cloudflare
from portals import (
    _assert_mansfield_logged_in,
    _is_mansfield_login_page,
    _mansfield_fill_credentials,
    _mansfield_state_path,
)

_CHILDREN: list[subprocess.Popen] = []
_CDP_PORT = int(os.environ.get("MANSFIELD_CDP_PORT", "9222"))


def _invoice_dir() -> Path:
    if os.environ.get("ENERGY_INVOICE_DIR"):
        return Path(os.environ["ENERGY_INVOICE_DIR"])
    if settings.energy_invoice_dir.exists():
        return settings.energy_invoice_dir
    host_fallback = Path.home() / "energy-invoices"
    if host_fallback.is_dir():
        return host_fallback
    return settings.energy_invoice_dir


def _have_x_display() -> bool:
    display = os.environ.get("DISPLAY")
    if not display:
        return False
    if display.startswith(":"):
        num = display[1:].split(".")[0]
        return Path(f"/tmp/.X11-unix/X{num}").exists()
    return True


def _apt_install() -> None:
    need: list[str] = []
    if not shutil.which("Xvfb"):
        need.append("xvfb")
    if not shutil.which("x11vnc"):
        need.append("x11vnc")
    if not shutil.which("websockify"):
        need.append("websockify")
    if not (
        Path("/usr/share/novnc/vnc.html").exists() or Path("/usr/share/novnc/index.html").exists()
    ):
        need.append("novnc")
    if not need:
        return
    print(f"Installing {' '.join(need)} for noVNC login…")
    subprocess.check_call(["apt-get", "update", "-qq"])
    subprocess.check_call(
        ["apt-get", "install", "-y", "-qq", "--no-install-recommends", *need],
    )


def _novnc_web_root() -> Path:
    root = Path("/usr/share/novnc").resolve()
    if (root / "vnc.html").exists() or (root / "index.html").exists():
        return root
    raise RuntimeError("noVNC web root not found after install")


def _start(cmd: list[str], *, pipe: bool = True) -> subprocess.Popen:
    kwargs: dict = {}
    if pipe:
        kwargs["stdout"] = subprocess.DEVNULL
        kwargs["stderr"] = subprocess.DEVNULL
    proc = subprocess.Popen(cmd, **kwargs)
    _CHILDREN.append(proc)
    return proc


def _cleanup(_: int | None = None, __: object | None = None) -> None:
    for proc in reversed(_CHILDREN):
        try:
            proc.send_signal(signal.SIGTERM)
        except Exception:
            pass
    for proc in reversed(_CHILDREN):
        try:
            proc.wait(timeout=3)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass


def ensure_display() -> str:
    if _have_x_display():
        display = os.environ["DISPLAY"]
        print(f"Using existing display {display}")
        return display

    _apt_install()
    display = os.environ.get("LOGIN_DISPLAY", ":99")
    os.environ["DISPLAY"] = display
    print(f"No GUI display — starting Xvfb on {display}")
    _start(["Xvfb", display, "-screen", "0", "1280x900x24", "-ac"])
    time.sleep(0.5)
    _start(
        [
            "x11vnc",
            "-display",
            display,
            "-forever",
            "-shared",
            "-nopw",
            "-rfbport",
            "5900",
            "-localhost",
        ]
    )
    web = _novnc_web_root()
    _start(["websockify", "--web", str(web), "6080", "127.0.0.1:5900"])
    time.sleep(0.5)
    print()
    print("Open: http://127.0.0.1:6080/vnc.html?autoconnect=1&resize=scale")
    print()
    return display


def _chrome_binary() -> str:
    env = os.environ.get("CHROME_BIN")
    if env and Path(env).is_file():
        return env
    matches = sorted(Path("/ms-playwright").glob("chromium-*/chrome-linux/chrome"))
    if matches:
        return str(matches[-1])
    for name in ("chromium", "chromium-browser", "google-chrome", "google-chrome-stable"):
        found = shutil.which(name)
        if found:
            return found
    raise RuntimeError("No Chromium binary found")


def _wait_cdp(port: int, timeout_s: float = 30.0) -> None:
    url = f"http://127.0.0.1:{port}/json/version"
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1) as resp:
                if resp.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError):
            time.sleep(0.25)
    raise RuntimeError(f"Chromium CDP did not come up on port {port}")


def _start_real_chrome(profile: Path) -> None:
    profile.mkdir(parents=True, exist_ok=True)
    chrome = _chrome_binary()
    print(f"Starting Chromium ({chrome}) on CDP {_CDP_PORT}…")
    _start(
        [
            chrome,
            f"--remote-debugging-port={_CDP_PORT}",
            "--remote-debugging-address=127.0.0.1",
            f"--user-data-dir={profile}",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-blink-features=AutomationControlled",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--window-size=1280,900",
            "about:blank",
        ],
        pipe=False,
    )
    _wait_cdp(_CDP_PORT)


async def _async_main() -> int:
    if not settings.mansfield_username or not settings.mansfield_password:
        print("Set MANSFIELD_USERNAME and MANSFIELD_PASSWORD in .env", file=sys.stderr)
        return 1

    out_dir = _invoice_dir()
    out_dir.mkdir(parents=True, exist_ok=True)
    state_path = _mansfield_state_path(out_dir)
    profile = out_dir / ".mansfield-chrome-profile"

    ensure_display()
    print(f"Clearing Cloudflare via FlareSolverr ({settings.flaresolverr_url})…")
    solved = await solve_cloudflare(settings.mansfield_portal_url)

    _start_real_chrome(profile)
    playwright = await async_playwright().start()
    browser = await playwright.chromium.connect_over_cdp(f"http://127.0.0.1:{_CDP_PORT}")
    try:
        context = browser.contexts[0] if browser.contexts else await browser.new_context()
        page = context.pages[0] if context.pages else await context.new_page()
        await context.add_cookies(playwright_cookies(solved["cookies"]))
        await page.goto(
            solved.get("url") or settings.mansfield_portal_url,
            wait_until="domcontentloaded",
            timeout=90_000,
        )
        if _is_mansfield_login_page(page.url) or await page.locator("input[type='password']").count():
            await _mansfield_fill_credentials(page, submit=False)
        print("Complete captcha + Sign In in noVNC (often still rejected on Xvfb)…")
        for _ in range(150):
            if not _is_mansfield_login_page(page.url):
                break
            await page.wait_for_timeout(2_000)
        else:
            await page.screenshot(path=str(out_dir / "mansfield-login-stuck.png"), full_page=True)
            print("Still on login — use import_mansfield_session.py instead.", file=sys.stderr)
            return 1
        await _assert_mansfield_logged_in(page, out_dir, using_saved_session=False)
        await context.storage_state(path=str(state_path))
        print(f"Saved session → {state_path}")
        return 0
    finally:
        try:
            await browser.close()
        except Exception:
            pass
        await playwright.stop()
        _cleanup()


def main() -> int:
    signal.signal(signal.SIGTERM, _cleanup)
    signal.signal(signal.SIGINT, _cleanup)
    try:
        return asyncio.run(_async_main())
    finally:
        _cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
