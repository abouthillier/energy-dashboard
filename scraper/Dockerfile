FROM mcr.microsoft.com/playwright/python:v1.51.0-jammy

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY *.py ./
COPY tests ./tests

ENV HOST=0.0.0.0
ENV PORT=3011
ENV ENERGY_API_URL=http://energy-service:3010
ENV ENERGY_INVOICE_DIR=/invoices
ENV HEADLESS=true
ENV TZ=America/New_York

EXPOSE 3011

CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "3011"]
