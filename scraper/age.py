from __future__ import annotations

from datetime import date
from typing import Any

PROVIDERS = {
    "mansfield": "Mansfield Municipal Electric",
    "eversource": "Eversource Gas",
}


def parse_bill_date(bill: dict[str, Any]) -> date | None:
    for key in ("bill_date", "billing_period_end"):
        value = bill.get(key)
        if value:
            return date.fromisoformat(str(value)[:10])

    bill_month = bill.get("bill_month")
    if bill_month:
        # bill_month is YYYY-MM
        return date.fromisoformat(f"{bill_month}-01")
    return None


def days_old(as_of: date, latest: date | None) -> int | None:
    if latest is None:
        return None
    return (as_of - latest).days


def is_eligible(age_days: int | None, min_age_days: int = 28) -> bool:
    # No bill yet → eligible to scrape.
    if age_days is None:
        return True
    return age_days >= min_age_days


def latest_bill_for_provider(bills: list[dict[str, Any]], provider_name: str) -> dict[str, Any] | None:
    matching = [bill for bill in bills if bill.get("provider") == provider_name]
    if not matching:
        return None

    def sort_key(bill: dict[str, Any]) -> tuple:
        parsed = parse_bill_date(bill)
        return (parsed or date.min, bill.get("bill_month") or "")

    return max(matching, key=sort_key)


def provider_status(
    bills: list[dict[str, Any]],
    *,
    as_of: date | None = None,
    min_age_days: int = 28,
    provider_keys: list[str] | None = None,
) -> list[dict[str, Any]]:
    today = as_of or date.today()
    keys = provider_keys or list(PROVIDERS.keys())
    statuses = []
    for key in keys:
        provider_name = PROVIDERS[key]
        latest = latest_bill_for_provider(bills, provider_name)
        latest_date = parse_bill_date(latest) if latest else None
        age = days_old(today, latest_date)
        statuses.append(
            {
                "key": key,
                "provider": provider_name,
                "latest_date": latest_date.isoformat() if latest_date else None,
                "latest_bill_month": latest.get("bill_month") if latest else None,
                "age_days": age,
                "eligible": is_eligible(age, min_age_days),
            }
        )
    return statuses
