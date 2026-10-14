from __future__ import annotations

import logging
import re
from pathlib import Path
from typing import Any

from age import PROVIDERS, provider_status
from config import settings
from energy_client import fetch_bills, trigger_import
from job import CancelledFetch, FetchJob
from portals import download_eversource, download_mansfield

logger = logging.getLogger(__name__)

_SCREENSHOT_RE = re.compile(r"Screenshot:\s*([A-Za-z0-9._-]+\.png)", re.I)


def _error_entry(key: str, exc: BaseException) -> dict[str, Any]:
    message = str(exc)
    match = _SCREENSHOT_RE.search(message)
    entry: dict[str, Any] = {"provider": key, "error": message}
    if match:
        entry["screenshot"] = match.group(1)
    return entry


DOWNLOADERS = {
    "mansfield": download_mansfield,
    "eversource": download_eversource,
}


async def status_payload(provider_keys: list[str] | None = None) -> dict[str, Any]:
    bills = await fetch_bills()
    providers = provider_status(
        bills,
        min_age_days=settings.min_bill_age_days,
        provider_keys=provider_keys,
    )
    return {
        "status": "ok",
        "min_bill_age_days": settings.min_bill_age_days,
        "providers": providers,
    }


def _resolve_targets(
    statuses: list[dict[str, Any]],
    *,
    force: bool,
    scheduled: bool,
) -> tuple[list[str] | None, dict[str, Any] | None]:
    """Returns (to_run, early_response). early_response set means stop without scraping."""
    ineligible = [row for row in statuses if not row["eligible"]]
    if ineligible and not force:
        if scheduled:
            return [row["key"] for row in statuses if row["eligible"]], None
        return None, {
            "status": "needs_confirmation",
            "message": "One or more invoices are newer than 28 days; confirm to force scrape.",
            "providers": statuses,
        }
    return [row["key"] for row in statuses], None


async def plan_fetch(
    *,
    force: bool = False,
    providers: list[str] | None = None,
    scheduled: bool = False,
) -> dict[str, Any]:
    keys = providers or list(PROVIDERS.keys())
    for key in keys:
        if key not in PROVIDERS:
            raise ValueError(f"Unknown provider: {key}")

    bills = await fetch_bills()
    statuses = provider_status(
        bills,
        min_age_days=settings.min_bill_age_days,
        provider_keys=keys,
    )
    to_run, early = _resolve_targets(statuses, force=force, scheduled=scheduled)
    if early:
        return early
    assert to_run is not None
    return {
        "status": "ready",
        "providers": statuses,
        "to_run": to_run,
    }


async def run_fetch(
    *,
    force: bool = False,
    providers: list[str] | None = None,
    scheduled: bool = False,
    job: FetchJob | None = None,
) -> dict[str, Any]:
    async def step(message: str) -> None:
        if job:
            await job.step(message)

    def raise_if_cancelled() -> None:
        if job:
            job.raise_if_cancelled()

    await step("Checking latest invoices")
    plan = await plan_fetch(force=force, providers=providers, scheduled=scheduled)
    if plan.get("status") == "needs_confirmation":
        return plan

    statuses = plan["providers"]
    to_run: list[str] = plan.get("to_run") or []

    if not to_run:
        await step("Nothing to scrape — invoices still fresh")
        return {
            "status": "ok",
            "message": "Nothing to scrape; all selected invoices are still fresh.",
            "providers": statuses,
            "downloaded": [],
            "import": None,
        }

    settings.energy_invoice_dir.mkdir(parents=True, exist_ok=True)
    downloaded: list[dict[str, Any]] = []
    errors: list[dict[str, str]] = []

    for key in to_run:
        raise_if_cancelled()
        label = PROVIDERS[key]
        await step(f"Starting {label}")
        try:
            path: Path = await DOWNLOADERS[key](
                settings.energy_invoice_dir,
                on_step=step,
                raise_if_cancelled=raise_if_cancelled,
            )
            downloaded.append({"provider": key, "path": str(path), "status": "ok"})
            await step(f"Finished {label}")
            logger.info("Downloaded %s -> %s", key, path)
        except CancelledFetch:
            raise
        except Exception as exc:
            logger.exception("Failed downloading %s", key)
            errors.append(_error_entry(key, exc))
            await step(f"{label} failed: {exc}")

    raise_if_cancelled()
    import_result = None
    if downloaded:
        await step("Importing PDFs into energy-service")
        import_result = await trigger_import()
        await step("Import complete")

    overall = "ok" if downloaded and not errors else ("partial" if downloaded else "error")
    await step(f"Done ({overall})")
    return {
        "status": overall,
        "providers": statuses,
        "downloaded": downloaded,
        "errors": errors,
        "import": import_result,
    }
