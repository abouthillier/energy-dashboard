#!/usr/bin/env python3
"""Mansfield session helper.

InvoiceCloud uses reCAPTCHA Enterprise. Completing the checkbox inside Docker/Xvfb
almost always fails server-side (you see the checkbox again after Sign In).

Use cookie import from a real desktop browser instead:

  python import_mansfield_session.py /invoices/mansfield-cookies.json

Optional (usually futile): MANSFIELD_FORCE_NOVNC=1 python login_mansfield.py
"""

from __future__ import annotations

import os
import sys


def main() -> int:
    if os.environ.get("MANSFIELD_FORCE_NOVNC", "").lower() in ("1", "true", "yes"):
        # Late import keeps the common path fast/clear.
        from login_mansfield_novnc import main as novnc_main

        return novnc_main()

    print(
        """
Mansfield login cannot be done reliably in Docker/Xvfb — Google reCAPTCHA Enterprise
rejects the session even after the checkbox turns green.

Do this instead (once every few weeks when the session expires):

  1. On your normal desktop browser, open the Mansfield InvoiceCloud portal and Sign In.
  2. Cookie-Editor extension → Export for invoicecloud.com → JSON.
  3. Save as ~/energy-invoices/mansfield-cookies.json
  4. Import:

       docker compose -f ~/docker-compose.yml run --rm --entrypoint '' \\
         -e ENERGY_INVOICE_DIR=/invoices \\
         energy-scraper python import_mansfield_session.py /invoices/mansfield-cookies.json

  5. Fetch Bills again.
""".strip()
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
