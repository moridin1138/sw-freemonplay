# Build environment for the x86_64 Linux client.
#
# Ubuntu 22.04 (glibc 2.35, GStreamer 1.20) is deliberately the oldest still-supported
# base: binaries built here run on Ubuntu 22.04+, Debian 12+ and other 2022-or-later
# distros. The client needs GStreamer >= 1.14. (build.sh selects linux/amd64.)
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential pkg-config curl ca-certificates \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    && rm -rf /var/lib/apt/lists/*

# Same toolchain as the rest of the project (edition 2024 needs 1.85+).
ARG RUST_VERSION=1.99.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}"
ENV PATH=/root/.cargo/bin:$PATH

WORKDIR /src
