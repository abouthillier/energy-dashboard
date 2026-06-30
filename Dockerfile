FROM rust:1-alpine AS builder

WORKDIR /app

RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static sqlite-dev

COPY Cargo.toml Cargo.lock* ./
COPY migrations ./migrations
COPY src ./src

RUN cargo build --release

FROM alpine:3.20

RUN apk add --no-cache ca-certificates poppler-utils

WORKDIR /app
COPY --from=builder /app/target/release/energy-service /usr/local/bin/energy-service

ENV ENERGY_HOST=0.0.0.0
ENV ENERGY_PORT=3010
ENV ENERGY_DATABASE_URL=sqlite:/data/energy.sqlite
ENV ENERGY_INVOICE_DIR=/invoices

EXPOSE 3010

CMD ["energy-service"]
