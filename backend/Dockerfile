# Multi-stage build for the Sauron Rust services.
# Build one binary per image, selected with `--build-arg BIN=sauron-<name>`.
# (cargo-chef could share the dependency layer across services; kept simple here.)

FROM rust:1-bookworm AS builder
ARG BIN
# Set automatically by BuildKit; defaulted so a plain `docker build` still works.
ARG TARGETARCH=amd64
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl unzip \
    && rm -rf /var/lib/apt/lists/*

# sauron-tier (and sauron-api, which depends on it) link DuckDB *dynamically* —
# the `bundled` cargo feature is off by default because compiling the DuckDB C++
# amalgamation is the single slowest item in the workspace build. So the build
# needs a libduckdb to link against, exactly as the RPM path does via
# packaging/rpm/fetch-libduckdb.sh (not reachable here: the build context is
# ./backend, and there is no git in this image).
#
# The DuckDB version is DERIVED from Cargo.lock rather than pinned, so a future
# libduckdb-sys bump fetches the matching library instead of silently linking a
# mismatched one. libduckdb-sys encodes it as 1.<MMmmpp>.x, e.g. 1.10504.0 => 1.5.4.
# Copied ahead of the sources so this layer is reused until Cargo.lock changes.
COPY Cargo.lock ./
RUN set -eux; \
    encoded="$(awk '\
        $1=="name" && $3=="\"libduckdb-sys\""{f=1; next} \
        f && $1=="version"{gsub(/"/,"",$3); split($3,a,"."); print a[2]; exit}' Cargo.lock)"; \
    [ -n "$encoded" ] || { echo "libduckdb-sys not found in Cargo.lock" >&2; exit 1; }; \
    ver="$((encoded / 10000)).$(((encoded / 100) % 100)).$((encoded % 100))"; \
    case "$TARGETARCH" in \
        amd64) asset=libduckdb-linux-amd64.zip ;; \
        arm64) asset=libduckdb-linux-arm64.zip ;; \
        *) echo "no prebuilt libduckdb for TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo ">> fetching libduckdb $ver ($asset)"; \
    mkdir -p /opt/duckdb; \
    curl --proto '=https' --tlsv1.2 -fSL \
        "https://github.com/duckdb/duckdb/releases/download/v${ver}/${asset}" \
        -o /tmp/libduckdb.zip; \
    unzip -o -q /tmp/libduckdb.zip -d /opt/duckdb; \
    rm /tmp/libduckdb.zip; \
    [ -f /opt/duckdb/libduckdb.so ] || { echo "archive had no libduckdb.so" >&2; ls -la /opt/duckdb >&2; exit 1; }; \
    strip --strip-unneeded /opt/duckdb/libduckdb.so || echo ">> (strip failed — shipping unstripped)"
ENV DUCKDB_LIB_DIR=/opt/duckdb \
    DUCKDB_INCLUDE_DIR=/opt/duckdb

COPY . .
RUN cargo build --release --bin ${BIN}
# Copy the selected binary to a stable path for the runtime stage.
RUN cp target/release/${BIN} /app/service

# Stage only the shared libraries this particular binary actually links. Four of
# the six services do not touch DuckDB, and libduckdb.so is ~57 MB stripped —
# copying it unconditionally would bloat every image for two that need it.
RUN mkdir -p /app/libs \
    && if ldd /app/service | grep -q libduckdb; then cp /opt/duckdb/libduckdb.so /app/libs/; fi

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libpq5 \
    && rm -rf /var/lib/apt/lists/*
# Empty for services that do not link DuckDB; ldconfig is a no-op in that case.
COPY --from=builder /app/libs/ /usr/local/lib/
RUN ldconfig
COPY --from=builder /app/service /usr/local/bin/service
ENTRYPOINT ["/usr/local/bin/service"]
