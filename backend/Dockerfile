# Multi-stage build for the Sauron Rust services.
# Build one binary per image, selected with `--build-arg BIN=sauron-<name>`.
# (cargo-chef could share the dependency layer across services; kept simple here.)

FROM rust:1-bookworm AS builder
ARG BIN
WORKDIR /app
COPY . .
RUN cargo build --release -p ${BIN}
# Copy the selected binary to a stable path for the runtime stage.
RUN cp target/release/${BIN} /app/service

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/service /usr/local/bin/service
ENTRYPOINT ["/usr/local/bin/service"]
