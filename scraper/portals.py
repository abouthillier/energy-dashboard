from __future__ import annotations

from collections.abc import Awaitable, Callable
from datetime import date
from pathlib import Path
import inspect
import re

from playwright.async_api import async_playwright

from config import settings
from flaresolverr_client import playwright_cookies, solve_cloudflare

ProgressCb = Callable[[str], Awaitable[None] | None]
CancelCb = Callable[[], None]


class PortalError(RuntimeError):
    pass


async def _progress(on_step: ProgressCb | None, message: str) -> None:
    if on_step:
        result = on_step(message)
        if inspect.isawaitable(result):
            await result


def _check_cancel(raise_if_cancelled: CancelCb | None) -> None:
    if raise_if_cancelled:
        raise_if_cancelled()


def _assert_no_blocker(html: str) -> None:
    # Only trip on an active challenge page, not script tags/marketing copy.
    lower = html.lower()
    challenge_phrases = (
        "enter the verification code",
        "enter your verification code",
        "two-factor authentication",
        "authenticator app",
        "verify your identity",
        "one-time passcode",
        "one time passcode",
        "security code has been sent",
        "we sent you a code",
        "enter the code we sent",
        "just a moment",
        "checking your browser",
        "attention required",
        "enable javascript and cookies",
    )
    for phrase in challenge_phrases:
        if phrase in lower:
            raise PortalError(
                f"Portal blocked scrape ({phrase}); complete manually or disable MFA for this account"
            )


async def _write_pdf(destination: Path, data: bytes) -> Path:
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(data)
    if destination.stat().st_size < 100 or not data.startswith(b"%PDF"):
        raise PortalError(f"Download is not a usable PDF: {destination}")
    return destination


async def _click_and_save(page, locator, destination: Path) -> Path:
    destination.parent.mkdir(parents=True, exist_ok=True)
    async with page.expect_download(timeout=120_000) as download_info:
        await locator.click()
    download = await download_info.value
    await download.save_as(str(destination))
    data = destination.read_bytes()
    if len(data) < 100 or not data.startswith(b"%PDF"):
        raise PortalError(f"Download is not a usable PDF: {destination}")
    return destination


async def _click_view_invoice(page, context, locator, destination: Path) -> Path:
    """InvoiceCloud 'View Invoice' may download, open a tab, or return application/pdf."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    pages_before = set(context.pages)
    pdf_responses: list = []

    def on_response(response) -> None:
        ctype = (response.headers.get("content-type") or "").lower()
        if "application/pdf" in ctype:
            pdf_responses.append(response)

    page.on("response", on_response)
    context.on("page", lambda p: p.on("response", on_response))
    try:
        try:
            return await _click_and_save(page, locator, destination)
        except Exception:
            pass  # click already happened; fall through to popup / response paths

        for _ in range(40):
            if pdf_responses:
                return await _write_pdf(destination, await pdf_responses[0].body())

            for candidate in context.pages:
                if candidate is page and candidate in pages_before:
                    continue
                try:
                    await candidate.wait_for_load_state("domcontentloaded", timeout=5_000)
                except Exception:
                    continue
                url = (candidate.url or "").lower()
                if ".pdf" in url or "getpdf" in url:
                    raw = await context.request.get(candidate.url)
                    data = await raw.body()
                    if data.startswith(b"%PDF"):
                        return await _write_pdf(destination, data)
                save_btn = candidate.get_by_role(
                    "link", name=re.compile(r"download|pdf|save|print", re.I)
                ).or_(
                    candidate.get_by_role(
                        "button", name=re.compile(r"download|pdf|save|print", re.I)
                    )
                )
                if await save_btn.count():
                    try:
                        return await _click_and_save(candidate, save_btn.first, destination)
                    except Exception:
                        pass
                for frame in candidate.frames:
                    frame_url = (frame.url or "").lower()
                    if ".pdf" in frame_url or "getpdf" in frame_url:
                        raw = await context.request.get(frame.url)
                        data = await raw.body()
                        if data.startswith(b"%PDF"):
                            return await _write_pdf(destination, data)

            url = page.url.lower()
            if ".pdf" in url or "getpdf" in url:
                raw = await context.request.get(page.url)
                data = await raw.body()
                if data.startswith(b"%PDF"):
                    return await _write_pdf(destination, data)

            await page.wait_for_timeout(500)

        raise PortalError(
            f"View Invoice click did not yield a PDF (url={page.url}, tabs={len(context.pages)})"
        )
    finally:
        page.remove_listener("response", on_response)

def _mansfield_state_path(invoice_dir: Path | None = None) -> Path:
    """Always land next to invoices so host login + container scrape share one file."""
    out = invoice_dir or settings.energy_invoice_dir
    return out / settings.mansfield_storage_state.name


def _is_mansfield_login_page(url: str) -> bool:
    return "customerlogin.aspx" in url.lower()


async def _mansfield_fill_credentials(page, *, submit: bool = True) -> None:
    email = page.locator(
        "#ctl00_ctl00_cphBody_cphBodyLeft_txtEmailAddress, "
        "input[type='email'], input[name*='Email' i], input[id*='Email' i], input[name*='User' i]"
    ).first
    password = page.locator(
        "#ctl00_ctl00_cphBody_cphBodyLeft_txtPassword, input[type='password']"
    ).first
    await email.wait_for(state="visible", timeout=60_000)
    await email.fill(settings.mansfield_username)
    await password.fill(settings.mansfield_password)
    if not submit:
        return

    login_btn = page.locator(
        "#ctl00_ctl00_cphBody_cphBodyLeft_btnLogin, "
        "#ctl00_ctl00_cphBody_cphBodyLeft_LoginControl1_LoginButton"
    ).or_(page.get_by_role("button", name=re.compile(r"sign\s*in", re.I))).or_(
        page.get_by_role("link", name=re.compile(r"sign\s*in", re.I))
    )
    if await login_btn.count():
        try:
            async with page.expect_navigation(timeout=90_000, wait_until="domcontentloaded"):
                await login_btn.first.click()
        except Exception:
            await login_btn.first.click()
            await page.wait_for_load_state("domcontentloaded", timeout=90_000)
    else:
        try:
            async with page.expect_navigation(timeout=90_000, wait_until="domcontentloaded"):
                await password.press("Enter")
        except Exception:
            await password.press("Enter")
            await page.wait_for_load_state("domcontentloaded", timeout=90_000)


async def _assert_mansfield_logged_in(page, out_dir: Path, *, using_saved_session: bool) -> None:
    await page.wait_for_timeout(2_000)
    _assert_no_blocker(await page.content())
    if not _is_mansfield_login_page(page.url):
        return
    debug = out_dir / "mansfield-login-stuck.png"
    await page.screenshot(path=str(debug), full_page=True)
    body = (await page.inner_text("body")).lower()
    if using_saved_session:
        raise PortalError(
            "Mansfield saved session expired or was rejected — run "
            "`python login_mansfield.py` on the host (headed browser, complete captcha once). "
            f"Screenshot: {debug.name}"
        )
    if (
        "checkbox challenge" in body
        or "complete the checkbox" in body
        or "please complete the captcha" in body
    ):
        raise PortalError(
            "Mansfield login blocked by Google reCAPTCHA. "
            "Run `python login_mansfield.py` once to save a session instead. "
            f"Screenshot: {debug.name}"
        )
    raise PortalError(
        f"Mansfield still on login page after Sign In ({page.url}). "
        f"Credentials may be wrong. Screenshot: {debug.name}"
    )


async def download_mansfield(
    invoice_dir: Path | None = None,
    *,
    on_step: ProgressCb | None = None,
    raise_if_cancelled: CancelCb | None = None,
) -> Path:
    out_dir = invoice_dir or settings.energy_invoice_dir
    state_path = _mansfield_state_path(out_dir)
    has_session = state_path.is_file()
    if not has_session and (not settings.mansfield_username or not settings.mansfield_password):
        raise PortalError(
            "Mansfield needs a saved session or credentials. "
            "Run `python login_mansfield.py` (headed) once, or set "
            "MANSFIELD_USERNAME / MANSFIELD_PASSWORD."
        )

    stamp = date.today().strftime("%Y-%m")
    destination = out_dir / f"mansfield-{stamp}.pdf"

    await _progress(on_step, "Mansfield: clearing Cloudflare via FlareSolverr")
    _check_cancel(raise_if_cancelled)
    solved = await solve_cloudflare(settings.mansfield_portal_url)

    await _progress(
        on_step,
        "Mansfield: launching browser with saved session"
        if has_session
        else "Mansfield: launching browser with cleared cookies",
    )
    playwright = await async_playwright().start()
    browser = await playwright.chromium.launch(
        headless=settings.headless,
        args=["--disable-blink-features=AutomationControlled"],
    )
    try:
        _check_cancel(raise_if_cancelled)
        context_kwargs: dict = {
            "accept_downloads": True,
            "user_agent": solved.get("user_agent"),
            "locale": "en-US",
        }
        if has_session:
            context_kwargs["storage_state"] = str(state_path)
        context = await browser.new_context(**context_kwargs)
        await context.add_cookies(playwright_cookies(solved["cookies"]))
        page = await context.new_page()

        await _progress(on_step, "Mansfield: opening InvoiceCloud portal")
        await page.goto(
            solved.get("url") or settings.mansfield_portal_url,
            wait_until="domcontentloaded",
            timeout=90_000,
        )
        _assert_no_blocker(await page.content())
        _check_cancel(raise_if_cancelled)

        if has_session and not _is_mansfield_login_page(page.url):
            await _progress(on_step, "Mansfield: reusing saved login session")
        else:
            if has_session and _is_mansfield_login_page(page.url):
                await _progress(on_step, "Mansfield: saved session hit login page; trying credentials")
            sign_in = page.get_by_role("link", name=re.compile(r"sign\s*in", re.I)).or_(
                page.get_by_role("button", name=re.compile(r"sign\s*in", re.I))
            )
            if await sign_in.count() and not await page.locator("input[type='password']").count():
                await sign_in.first.click()
                await page.wait_for_load_state("domcontentloaded")

            if not settings.mansfield_username or not settings.mansfield_password:
                await _assert_mansfield_logged_in(page, out_dir, using_saved_session=True)

            await _progress(on_step, "Mansfield: signing in")
            await _mansfield_fill_credentials(page)
            # InvoiceCloud keeps sockets open — never wait for networkidle.
            await _assert_mansfield_logged_in(page, out_dir, using_saved_session=False)

        await _progress(on_step, f"Mansfield: post-login at {page.url}")
        await _progress(on_step, "Mansfield: locating View Invoice")
        pdf_control = (
            page.get_by_role("link", name=re.compile(r"view\s*invoice", re.I))
            .or_(page.get_by_role("button", name=re.compile(r"view\s*invoice", re.I)))
            .or_(page.get_by_text(re.compile(r"^\s*view\s*invoice\s*$", re.I)))
            .or_(
                page.locator(
                    'a:has-text("View Invoice"), button:has-text("View Invoice"), '
                    'input[value*="View Invoice" i], a[href*="ViewInvoice" i]'
                )
            )
            .first
        )
        try:
            await pdf_control.wait_for(state="visible", timeout=60_000)
        except Exception:
            debug = out_dir / "mansfield-no-view-invoice.png"
            await page.screenshot(path=str(debug), full_page=True)
            raise PortalError(
                f"Mansfield: no View Invoice control on {page.url}. Screenshot: {debug.name}"
            )
        await _progress(on_step, "Mansfield: downloading PDF")
        await _click_view_invoice(page, context, pdf_control, destination)
        # Refresh cookies/localStorage so the next monthly run keeps the session warm.
        state_path.parent.mkdir(parents=True, exist_ok=True)
        await context.storage_state(path=str(state_path))
        await _progress(on_step, f"Mansfield: saved {destination.name}")
        return destination
    except Exception:
        _check_cancel(raise_if_cancelled)
        raise
    finally:
        await browser.close()
        await playwright.stop()


async def download_eversource(
    invoice_dir: Path | None = None,
    *,
    on_step: ProgressCb | None = None,
    raise_if_cancelled: CancelCb | None = None,
) -> Path:
    if not settings.eversource_username or not settings.eversource_password:
        raise PortalError("EVERSOURCE_USERNAME / EVERSOURCE_PASSWORD not configured")

    out_dir = invoice_dir or settings.energy_invoice_dir
    stamp = date.today().strftime("%Y-%m")
    destination = out_dir / f"eversource-{stamp}.pdf"

    await _progress(on_step, "Eversource: launching browser")
    playwright = await async_playwright().start()
    browser = await playwright.chromium.launch(
        headless=settings.headless,
        args=["--disable-blink-features=AutomationControlled"],
    )
    try:
        _check_cancel(raise_if_cancelled)
        context = await browser.new_context(accept_downloads=True, locale="en-US")
        page = await context.new_page()
        await _progress(on_step, "Eversource: opening login")
        await page.goto(settings.eversource_login_url, wait_until="domcontentloaded", timeout=90_000)
        _assert_no_blocker(await page.content())
        _check_cancel(raise_if_cancelled)

        await _progress(on_step, "Eversource: signing in")
        user = page.locator('input[name="email"]').first
        password = page.locator('input#password, input[name="password"]').first
        await user.wait_for(state="visible", timeout=60_000)
        await user.fill(settings.eversource_username)
        await password.fill(settings.eversource_password)

        sign_in = page.locator("#signIn").or_(
            page.get_by_role("button", name=re.compile(r"sign\s*in", re.I))
        )
        if await sign_in.count():
            await sign_in.first.click()
        else:
            await password.press("Enter")

        # Login stays on /login while prompting for optional 2SV enrollment.
        await _progress(on_step, "Eversource: waiting for post-login page")
        prompted = page.get_by_role("heading", name=re.compile(r"two-step verification", re.I)).or_(
            page.get_by_text(re.compile(r"would you like to turn on two-step verification", re.I))
        )
        later = page.get_by_role("button", name=re.compile(r"ask me again later", re.I)).or_(
            page.get_by_text(re.compile(r"ask me again later", re.I))
        )
        left_login = False
        for tick in range(45):
            _check_cancel(raise_if_cancelled)
            url = page.url.lower()
            if await later.count() and await later.first.is_visible():
                await _progress(on_step, "Eversource: dismissing 2SV enrollment prompt")
                await later.first.click()
                await page.wait_for_load_state("domcontentloaded", timeout=30_000)
                await page.wait_for_timeout(1_500)
                left_login = True
                break
            if await prompted.count() and await prompted.first.is_visible():
                # Heading visible but button not yet — wait a beat.
                await page.wait_for_timeout(1_000)
                continue
            if "login" not in url and "signin" not in url:
                left_login = True
                break
            if tick and tick % 5 == 0:
                await _progress(on_step, f"Eversource: still on {page.url}")
            await page.wait_for_timeout(2_000)

        if not left_login:
            debug = (invoice_dir or settings.energy_invoice_dir) / "eversource-login-stuck.png"
            await page.screenshot(path=str(debug), full_page=True)
            raise PortalError(
                "Eversource login did not leave the sign-in flow "
                f"(url={page.url}). Screenshot: {debug.name}"
            )

        _assert_no_blocker(await page.content())
        _check_cancel(raise_if_cancelled)

        await _progress(on_step, "Eversource: opening past bills")
        await page.goto(settings.eversource_bills_url, wait_until="domcontentloaded", timeout=90_000)
        await page.wait_for_timeout(2_000)
        later = page.get_by_role("button", name=re.compile(r"ask me again later", re.I))
        if await later.count() and await later.first.is_visible():
            await _progress(on_step, "Eversource: dismissing 2SV prompt again")
            await later.first.click()
            await page.wait_for_timeout(1_500)
            await page.goto(settings.eversource_bills_url, wait_until="domcontentloaded", timeout=90_000)
            await page.wait_for_timeout(2_000)
        if "login" in page.url.lower():
            raise PortalError("Eversource still on login after submit — session not established")
        _assert_no_blocker(await page.content())
        _check_cancel(raise_if_cancelled)

        await _progress(on_step, "Eversource: downloading latest bill PDF")
        # i-doxs AccountHistory: first blue VIEW opens the newest statement as a PDF download.
        pdf_control = page.locator("a.btn.btn-blue.btn-pay", has_text=re.compile(r"^VIEW$", re.I)).first
        await pdf_control.wait_for(state="visible", timeout=60_000)
        await _click_and_save(page, pdf_control, destination)
        await _progress(on_step, f"Eversource: saved {destination.name}")
        return destination
    except Exception:
        _check_cancel(raise_if_cancelled)
        raise
    finally:
        await browser.close()
        await playwright.stop()
