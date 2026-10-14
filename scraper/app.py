from __future__ import annotations

import logging
import re
from contextlib import asynccontextmanager
from typing import Any
from zoneinfo import ZoneInfo

from apscheduler.schedulers.asyncio import AsyncIOScheduler
from apscheduler.triggers.cron import CronTrigger
from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel, Field

from config import settings
from fetch import plan_fetch, run_fetch, status_payload
from job import current_job, start_job

_ARTIFACT_NAME = re.compile(r"^[a-z0-9][a-z0-9._-]*\.png$", re.I)

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s %(message)s")
logger = logging.getLogger("energy-scraper")

scheduler = AsyncIOScheduler(timezone=ZoneInfo(settings.timezone))


class FetchRequest(BaseModel):
    force: bool = False
    providers: list[str] | None = Field(default=None)


async def scheduled_fetch() -> None:
    logger.info("Running scheduled bill fetch")
    try:
        job = await start_job(lambda j: run_fetch(force=False, scheduled=True, job=j))
        if job.task:
            await job.task
        logger.info("Scheduled fetch finished: %s", job.state)
    except RuntimeError as exc:
        logger.warning("Skipping scheduled fetch: %s", exc)


@asynccontextmanager
async def lifespan(_app: FastAPI):
    trigger = CronTrigger(
        day=settings.cron_day,
        hour=settings.cron_hour,
        minute=settings.cron_minute,
        timezone=settings.timezone,
    )
    scheduler.add_job(scheduled_fetch, trigger, id="monthly-bill-fetch", replace_existing=True)
    scheduler.start()
    logger.info(
        "Scheduler started: day=%s hour=%s tz=%s",
        settings.cron_day,
        settings.cron_hour,
        settings.timezone,
    )
    yield
    scheduler.shutdown(wait=False)


app = FastAPI(title="energy-scraper", lifespan=lifespan)


@app.get("/health")
async def health() -> dict[str, str]:
    return {"status": "ok"}


@app.get("/artifacts/{name}")
async def artifact(name: str) -> FileResponse:
    """Serve debug screenshots written next to invoices (login stuck, missing control, …)."""
    if not _ARTIFACT_NAME.fullmatch(name):
        raise HTTPException(status_code=400, detail="Invalid artifact name")
    path = (settings.energy_invoice_dir / name).resolve()
    if path.parent != settings.energy_invoice_dir.resolve() or not path.is_file():
        raise HTTPException(status_code=404, detail="Artifact not found")
    return FileResponse(path, media_type="image/png")


@app.get("/status")
async def status() -> dict[str, Any]:
    try:
        return await status_payload()
    except Exception as exc:
        raise HTTPException(status_code=502, detail=f"Energy service unavailable: {exc}") from exc


@app.get("/fetch/job")
async def fetch_job() -> dict[str, Any]:
    job = current_job()
    if not job:
        return {"status": "idle", "job": None}
    return {"status": "ok", "job": job.snapshot()}


@app.post("/fetch/cancel")
async def fetch_cancel() -> dict[str, Any]:
    job = current_job()
    if not job:
        return {"status": "idle", "cancelled": False}
    cancelled = job.request_cancel()
    await job.step("Cancel requested", check_cancel=False)
    return {"status": "ok", "cancelled": cancelled, "job": job.snapshot()}


@app.post("/fetch")
async def fetch(body: FetchRequest | None = None) -> Any:
    request = body or FetchRequest()
    try:
        plan = await plan_fetch(force=request.force, providers=request.providers, scheduled=False)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except Exception as exc:
        # Avoid bare 500 (and Cloudflare HTML) when energy-service is unreachable.
        raise HTTPException(
            status_code=422,
            detail=f"Cannot reach energy-service at {settings.energy_api_url}: {exc}",
        ) from exc

    if plan.get("status") == "needs_confirmation":
        return JSONResponse(status_code=409, content=plan)

    to_run = plan.get("to_run") or []
    if not to_run:
        return {
            "status": "ok",
            "message": "Nothing to scrape; all selected invoices are still fresh.",
            "providers": plan.get("providers"),
            "downloaded": [],
            "import": None,
        }

    try:
        job = await start_job(
            lambda j: run_fetch(
                force=True,  # gate already cleared
                providers=request.providers or to_run,
                scheduled=False,
                job=j,
            )
        )
    except RuntimeError as exc:
        existing = current_job()
        return JSONResponse(
            status_code=409,
            content={
                "status": "busy",
                "message": str(exc),
                "job": existing.snapshot() if existing else None,
            },
        )

    return JSONResponse(status_code=202, content={"status": "started", "job": job.snapshot()})
