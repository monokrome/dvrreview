FROM rust:alpine AS builder

WORKDIR /app

# Install build dependencies
RUN apk add --no-cache \
    musl-dev \
    postgresql-dev \
    pkgconfig

# Copy source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY migrations ./migrations

# Build release binary
RUN cargo build --release

FROM alpine:latest

# Install runtime dependencies
RUN apk add --no-cache \
    libpq \
    ffmpeg \
    ca-certificates

# Copy binary from builder
COPY --from=builder /app/target/release/dvrreview /usr/local/bin/dvrreview

# Copy migrations for diesel
COPY --from=builder /app/migrations /app/migrations

WORKDIR /data

ENTRYPOINT ["dvrreview"]
