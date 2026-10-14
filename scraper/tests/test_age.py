from datetime import date

from age import is_eligible, parse_bill_date, provider_status


def test_parse_bill_date_prefers_bill_date():
    bill = {
        "bill_date": "2026-06-15",
        "billing_period_end": "2026-06-01",
        "bill_month": "2026-05",
    }
    assert parse_bill_date(bill) == date(2026, 6, 15)


def test_parse_bill_date_falls_back_to_month():
    assert parse_bill_date({"bill_month": "2026-04"}) == date(2026, 4, 1)


def test_eligible_when_missing_or_old():
    assert is_eligible(None) is True
    assert is_eligible(27) is False
    assert is_eligible(28) is True
    assert is_eligible(40) is True


def test_provider_status_marks_fresh_ineligible():
    bills = [
        {
            "provider": "Mansfield Municipal Electric",
            "bill_date": "2026-07-01",
            "bill_month": "2026-07",
        },
        {
            "provider": "Eversource Gas",
            "billing_period_end": "2026-05-01",
            "bill_month": "2026-05",
        },
    ]
    statuses = provider_status(bills, as_of=date(2026, 7, 13), min_age_days=28)
    by_key = {row["key"]: row for row in statuses}
    assert by_key["mansfield"]["eligible"] is False
    assert by_key["mansfield"]["age_days"] == 12
    assert by_key["eversource"]["eligible"] is True
    assert by_key["eversource"]["age_days"] == 73
