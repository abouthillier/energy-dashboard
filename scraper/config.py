from pathlib import Path

from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=(".env", "../.env"),
        extra="ignore",
    )

    host: str = "0.0.0.0"
    port: int = 3011
    energy_api_url: str = "http://energy-service:3010"
    energy_invoice_dir: Path = Path("/invoices")
    min_bill_age_days: int = 28
    timezone: str = "America/New_York"
    cron_day: int = 10
    cron_hour: int = 12
    cron_minute: int = 0

    mansfield_username: str = ""
    mansfield_password: str = ""
    mansfield_portal_url: str = (
        "https://www.invoicecloud.com/portal/(S(gaymqpeyyn1imkpmosi2mbif))/2/CustomerLogin.aspx?BillerGUID=83b8b666-18cd-4742-98b4-d51401fe3940"
    )
    # One-time manual login writes this; monthly scrape reuses it (no reCAPTCHA).
    mansfield_storage_state: Path = Path("/invoices/mansfield-storage-state.json")

    eversource_username: str = ""
    eversource_password: str = ""
    eversource_login_url: str = "https://www.eversource.com/security/account/login"
    eversource_bills_url: str = (
        "https://www.eversource.com/residential/account-billing/past-bills-payments"
    )

    headless: bool = True
    flaresolverr_url: str = "http://127.0.0.1:8191"
    flaresolverr_timeout_ms: int = 60000


settings = Settings()
