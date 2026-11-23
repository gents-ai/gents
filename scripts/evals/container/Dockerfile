# syntax=docker/dockerfile:1.7
#
# The image a containerized native eval runs in: the Linux gents CLI built
# from the build context's commit with its core packs embedded, the core pack
# archives, and what scripts/evals/run-ladder.sh reads from that commit. No
# suite's fixtures: the launcher mounts only the running suite's definition
# and subjects. The context is `git archive <commit>`, so nothing uncommitted
# enters it; scripts/evals/run-container.sh image builds it.

FROM ubuntu:24.04 AS toolchain

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        binutils build-essential ca-certificates clang cmake curl git libclang-dev \
        libssl-dev pkg-config protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none

WORKDIR /src
COPY rust-toolchain.toml /src/rust-toolchain.toml
COPY core-packs/rust-toolchain.toml /src/core-packs/rust-toolchain.toml
# Both toolchain files name one channel; the core packs add the WASI target.
RUN channel="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)" \
    && test -n "$channel" \
    && rustup toolchain install "$channel" --profile minimal \
        --component rustfmt,clippy,rust-src,llvm-tools --target wasm32-wasip1

FROM toolchain AS build
COPY . /src
ARG GENTS_COMMIT
RUN test -n "$GENTS_COMMIT" || { echo "build with --build-arg GENTS_COMMIT=<sha>" >&2; exit 2; }
ENV GENTS_BUILD_GIT_SHA=$GENTS_COMMIT GENTS_BUILD_GIT_REF=$GENTS_COMMIT GENTS_BUILD_GIT_DIRTY=false \
    CARGO_PROFILE_RELEASE_LTO=off CARGO_INCREMENTAL=0
# The first build is only the tool `make core-packs` builds the packs with; the
# second embeds them and must find all of them.
RUN --mount=type=cache,id=gents-eval-cargo-registry,sharing=locked,target=/opt/cargo/registry \
    --mount=type=cache,id=gents-eval-cargo-git,sharing=locked,target=/opt/cargo/git \
    --mount=type=cache,id=gents-eval-target,sharing=locked,target=/src/target \
    --mount=type=cache,id=gents-eval-pack-pack_builder,sharing=locked,target=/src/core-packs/pack_builder/target \
    --mount=type=cache,id=gents-eval-pack-web,sharing=locked,target=/src/core-packs/web/target \
    --mount=type=cache,id=gents-eval-pack-dynamic_workflows,sharing=locked,target=/src/core-packs/dynamic_workflows/target \
    --mount=type=cache,id=gents-eval-pack-charts,sharing=locked,target=/src/core-packs/charts/target \
    --mount=type=cache,id=gents-eval-pack-data_tables,sharing=locked,target=/src/core-packs/data_tables/target \
    --mount=type=cache,id=gents-eval-pack-script,sharing=locked,target=/src/core-packs/script/target \
    set -eux; \
    rm -f target/release/gents; \
    cargo build -p gents-cli --release --locked; \
    rm -rf target/core-packs; \
    make core-packs CORE_PACK_TOOL=/src/target/release/gents; \
    GENTS_REQUIRE_CORE_PACKS=1 cargo build -p gents-cli --release --locked; \
    mkdir -p /out/core-packs; \
    cp target/release/gents /out/gents; \
    cp target/core-packs/gents.*.pack target/core-packs/SHA256SUMS-core-packs.txt /out/core-packs/; \
    /out/gents runtime version | grep -F "$GENTS_COMMIT"

FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash ca-certificates coreutils curl findutils git grep iproute2 jq less \
        libgcc-s1 liblzma5 libssl3t64 libstdc++6 procps python3 sed \
    && rm -rf /var/lib/apt/lists/*

COPY scripts/evals /opt/gents/src/scripts/evals
COPY crates/gents-protocol/prompts /opt/gents/src/crates/gents-protocol/prompts
COPY crates/gents-protocol/presets /opt/gents/src/crates/gents-protocol/presets
COPY LICENSE /opt/gents/src/LICENSE
COPY --from=build /out/core-packs /opt/gents/src/target/core-packs
COPY --from=build --chmod=0755 /out/gents /opt/gents/bin/gents

ARG GENTS_COMMIT
LABEL org.opencontainers.image.revision=$GENTS_COMMIT \
      org.opencontainers.image.title="gents native eval"
ENV GENTS_IMAGE_COMMIT=$GENTS_COMMIT \
    PATH=/opt/gents/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
# `--allow-host-bash` is refused anywhere this marker is missing or writable;
# the launcher runs the image read-only.
RUN cd /opt/gents/src/target/core-packs && sha256sum -c SHA256SUMS-core-packs.txt \
    && gents runtime version \
    && mkdir -p /opt/gents/src/crates/gents/tests/fixtures/configurator_evals /etc/gents \
    && printf '%s\n' "$GENTS_COMMIT" > /etc/gents/eval-container
WORKDIR /eval
