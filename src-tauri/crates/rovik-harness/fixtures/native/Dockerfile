FROM rust@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4
RUN apt-get update && apt-get install -y --no-install-recommends cmake ninja-build clang && rm -rf /var/lib/apt/lists/*
COPY --chmod=755 rovik-harness-tool /usr/local/bin/rovik-harness-tool
