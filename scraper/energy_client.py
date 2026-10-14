from __future__ import annotations

import httpx

from config import settings


async def fetch_bills() -> list[dict]:
    url = f"{settings.energy_api_url.rstrip('/')}/energy/monthly"
    async with httpx.AsyncClient(timeout=30.0) as client:
        response = await client.get(url)
        response.raise_for_status()
        payload = response.json()
        return payload.get("bills") or []


async def trigger_import() -> dict:
    url = f"{settings.energy_api_url.rstrip('/')}/energy/import"
    async with httpx.AsyncClient(timeout=120.0) as client:
        response = await client.post(url, json={})
        response.raise_for_status()
        return response.json()
