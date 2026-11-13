FROM node@sha256:a9f5f7c91a432850b2a8a7797adf5eadb6c733ceed61167806cee7ea7fbc29df AS node
FROM rust@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4
RUN --mount=from=node,source=/usr/local,target=/opt/node,ro --mount=type=secret,id=proxy_ca \
    cp /opt/node/bin/node /usr/local/bin/node && \
    cp -a /opt/node/lib/node_modules /usr/local/lib/ && \
    ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm && \
    apt-get update && apt-get install -y --no-install-recommends cmake ninja-build clang clang-tidy libclang-dev python3 python3-pip python3-pytest && \
    if [ -f /run/secrets/proxy_ca ]; then export NODE_EXTRA_CA_CERTS=/run/secrets/proxy_ca; fi && \
    npm install --global --ignore-scripts --no-audit --no-fund typescript@7.0.2 && \
    rm -rf /var/lib/apt/lists/* /root/.npm
COPY --chmod=755 rovik-harness-tool /usr/local/bin/rovik-harness-tool
COPY clang-graph.cpp /tmp/rovik-clang-graph.cpp
RUN c++ -std=c++17 -Wall -Wextra -Werror -Wno-misleading-indentation -I/usr/lib/llvm-14/include \
    /tmp/rovik-clang-graph.cpp -L/usr/lib/llvm-14/lib -lclang -o /usr/local/bin/rovik-clang-graph && \
    rm /tmp/rovik-clang-graph.cpp
