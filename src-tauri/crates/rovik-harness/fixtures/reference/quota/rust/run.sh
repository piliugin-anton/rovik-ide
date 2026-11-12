#!/bin/sh
set -eux
test "$(/usr/local/cargo/bin/rustc --version | cut -d ' ' -f 2)" = "1.85.1"
/usr/local/cargo/bin/rustc /oracle/consumer.rs --edition=2021 --target=x86_64-unknown-linux-gnu -C panic=unwind -C overflow-checks=yes -D warnings -o /build/consumer
/build/consumer
