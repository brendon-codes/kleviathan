FROM rust:1.92.0-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    autoconf \
    automake \
    bash \
    build-essential \
    clang \
    cmake \
    git \
    libclang-dev \
    libsqlite3-dev \
    libtool \
    perl \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY Cargo.toml Cargo.lock ./

RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release && \
    rm -rf src

COPY src/ src/
COPY tpl/ tpl/

RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash \
    ca-certificates \
    libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/kleviathan /usr/local/bin/kleviathan

RUN touch /.dockerenv

RUN useradd --create-home --home-dir /home/kleviathan --shell /bin/bash kleviathan
USER kleviathan
WORKDIR /home/kleviathan

RUN mkdir -p /home/kleviathan/.kleviathan/matrix_store

ENTRYPOINT ["kleviathan"]
CMD ["run-inner"]
