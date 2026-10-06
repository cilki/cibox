FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends git openssh-client ca-certificates \
        curl xz-utils build-essential cmake clang-format \
    && rm -rf /var/lib/apt/lists/*

COPY --from=zricethezav/gitleaks:v8.30.1 /usr/bin/gitleaks /usr/local/bin/gitleaks

ARG ZIG_VERSION=0.15.1
ARG ZIG_SHA256=c61c5da6edeea14ca51ecd5e4520c6f4189ef5250383db33d01848293bfafe05
RUN curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" -o /tmp/zig.tar.xz \
    && echo "${ZIG_SHA256}  /tmp/zig.tar.xz" | sha256sum -c - \
    && mkdir -p /usr/local/zig \
    && tar -xJf /tmp/zig.tar.xz -C /usr/local/zig --strip-components=1 \
    && ln -s /usr/local/zig/zig /usr/local/bin/zig \
    && rm /tmp/zig.tar.xz

# CI runners often clone the repo as a different user than the container runs as
RUN git config --global --add safe.directory '*'
