#!/bin/sh
set -eux
test "$(/usr/local/cargo/bin/rustc --version | cut -d ' ' -f 2)" = "1.85.1"
if /usr/local/cargo/bin/rustc /oracle/consumer.rs --edition=2021 --target=x86_64-unknown-linux-gnu -C panic=unwind -C overflow-checks=yes -D warnings -F unsafe-code -o /build/consumer; then
  /build/consumer
else
  exit 42
fi
