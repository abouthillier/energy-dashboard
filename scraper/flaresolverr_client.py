from __future__ import annotations

import httpx

from config import settings


async def solve_cloudflare(url: str) -> dict:
    """Use existing FlareSolverr to clear Cloudflare and return cookies + userAgent."""
    base = settings.flaresolverr_url.rstrip("/")
    if not base:
        raise RuntimeError("FLARESOLVERR_URL not configured")

    payload = {
        "cmd": "request.get",
        "url": url,
        "maxTimeout": settings.flaresolverr_timeout_ms,
    }
    async with httpx.AsyncClient(timeout=(settings.flaresolverr_timeout_ms / 1000) + 30) as client:
        response = await client.post(f"{base}/v1", json=payload)
        response.raise_for_status()
        data = response.json()

    if data.get("status") != "ok":
        raise RuntimeError(f"FlareSolverr failed: {data.get('message') or data}")

    solution = data.get("solution") or {}
    cookies = solution.get("cookies") or []
    user_agent = solution.get("userAgent")
    if not cookies:
        raise RuntimeError("FlareSolverr returned no cookies (challenge not cleared)")
    return {"cookies": cookies, "user_agent": user_agent, "url": solution.get("url") or url}


def playwright_cookies(flaresolverr_cookies: list[dict]) -> list[dict]:
    converted = []
    for cookie in flaresolverr_cookies:
        item = {
            "name": cookie["name"],
            "value": cookie["value"],
            "domain": cookie.get("domain") or "",
            "path": cookie.get("path") or "/",
        }
        if cookie.get("expires") not in (None, -1):
            # FlareSolverr expires is ms; Playwright wants seconds.
            expires = cookie["expires"]
            item["expires"] = expires / 1000 if expires > 10_000_000_000 else expires
        if "httpOnly" in cookie:
            item["httpOnly"] = bool(cookie["httpOnly"])
        if "secure" in cookie:
            item["secure"] = bool(cookie["secure"])
        if cookie.get("sameSite") in ("Strict", "Lax", "None"):
            item["sameSite"] = cookie["sameSite"]
        converted.append(item)
    return converted
