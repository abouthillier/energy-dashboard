from __future__ import annotations

import asyncio
import logging
import uuid
from datetime import datetime, timezone
from typing import Any, Callable, Awaitable

logger = logging.getLogger(__name__)

ProgressCb = Callable[[str], Awaitable[None] | None]


class CancelledFetch(Exception):
    pass


class FetchJob:
    def __init__(self) -> None:
        self.id = str(uuid.uuid4())
        self.state = "queued"  # queued | running | ok | partial | error | cancelled
        self.steps: list[dict[str, str]] = []
        self.result: dict[str, Any] | None = None
        self.error: str | None = None
        self.started_at = datetime.now(timezone.utc).isoformat()
        self.finished_at: str | None = None
        self.cancel_event = asyncio.Event()
        self.task: asyncio.Task | None = None

    def snapshot(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "state": self.state,
            "steps": list(self.steps),
            "result": self.result,
            "error": self.error,
            "startedAt": self.started_at,
            "finishedAt": self.finished_at,
            "cancelRequested": self.cancel_event.is_set(),
        }

    async def step(self, message: str, *, check_cancel: bool = True) -> None:
        if check_cancel:
            self.raise_if_cancelled()
        entry = {
            "at": datetime.now(timezone.utc).isoformat(),
            "message": message,
        }
        self.steps.append(entry)
        logger.info("[job %s] %s", self.id[:8], message)

    def raise_if_cancelled(self) -> None:
        if self.cancel_event.is_set():
            raise CancelledFetch("Scrape cancelled")

    def request_cancel(self) -> bool:
        if self.state in ("ok", "partial", "error", "cancelled"):
            return False
        self.cancel_event.set()
        # Soft cancel first; Playwright loops check raise_if_cancelled between steps.
        # Hard-cancel the task so stuck downloads/navigation don't hang forever.
        if self.task and not self.task.done():
            self.task.cancel()
        return True


# Single in-flight job (home-lab; no queue).
_current: FetchJob | None = None
_lock = asyncio.Lock()


def current_job() -> FetchJob | None:
    return _current


async def start_job(runner: Callable[[FetchJob], Awaitable[dict[str, Any]]]) -> FetchJob:
    global _current
    async with _lock:
        if _current and _current.state in ("queued", "running"):
            raise RuntimeError("A scrape is already running")
        job = FetchJob()
        _current = job

        async def _run() -> None:
            job.state = "running"
            try:
                result = await runner(job)
                if job.cancel_event.is_set():
                    job.state = "cancelled"
                    job.error = "Scrape cancelled"
                else:
                    job.result = result
                    job.state = result.get("status") or "ok"
            except asyncio.CancelledError:
                job.state = "cancelled"
                job.error = "Scrape cancelled"
                await job.step("Cancelled", check_cancel=False)
            except CancelledFetch:
                job.state = "cancelled"
                job.error = "Scrape cancelled"
                await job.step("Cancelled", check_cancel=False)
            except Exception as exc:
                logger.exception("Fetch job failed")
                job.state = "error"
                job.error = str(exc)
                await job.step(f"Failed: {exc}")
            finally:
                job.finished_at = datetime.now(timezone.utc).isoformat()

        job.task = asyncio.create_task(_run())
        return job
