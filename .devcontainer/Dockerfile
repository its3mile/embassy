FROM mcr.microsoft.com/devcontainers/rust:1-1-bullseye

RUN DEBIAN_FRONTEND=noninteractive \
    && apt-get update \
    && apt-get install -y \
    build-essential \
    libudev-dev \
    cmake \
    --no-install-recommends

RUN rustup target add thumbv6m-none-eabi \
    && rustup component add llvm-tools-preview \
    && cargo install --locked cargo-binutils \
    elf2uf2-rs \
    flip-link \
    probe-rs-tools

RUN curl --proto '=https' --tlsv1.2 -LsSf https://github.com/probe-rs/probe-rs/releases/latest/download/probe-rs-tools-installer.sh | sh

