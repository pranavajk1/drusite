# syntax=docker/dockerfile:1
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# drusite-server on a distroless base: no shell, no package manager, a non-root user, with the
# web app (the browser version) it serves at /. Build:
#
#   docker build -t drusite-server .
#
# Configure it with DRUSITE__* environment variables, or a config file in DRUSITE_CONFIG
# (crates/drusite-server/drusite.example.toml shows every setting).

FROM rust:1.100.0-slim-trixie@sha256:3b199fd6db7d709274df56565b8f4003f9e30d7bfcff9d53148c86053b7855e2 AS build
# The toolchain the image has, rather than rust-toolchain.toml's (which adds targets the server
# doesn't need). Both are 1.100.0.
# No debug info in the image: it would be most of the binary.
ENV RUSTUP_TOOLCHAIN=1.100.0 CARGO_TERM_COLOR=always \
    CARGO_PROFILE_RELEASE_DEBUG=false CARGO_PROFILE_RELEASE_STRIP=symbols
WORKDIR /src
COPY . .
# The commit the image is built from, which the server logs next to its version.
ARG DRUSITE_COMMIT
ENV DRUSITE_COMMIT=${DRUSITE_COMMIT}
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package drusite-server \
    && install -D target/release/drusite-server /out/drusite-server

# The core as WebAssembly for the web app. SQLCipher is C: clang compiles it for wasm32 and
# llvm-ar (from the toolchain) archives it.
FROM rust:1.100.0-slim-trixie@sha256:3b199fd6db7d709274df56565b8f4003f9e30d7bfcff9d53148c86053b7855e2 AS wasm
ENV RUSTUP_TOOLCHAIN=1.100.0 CARGO_TERM_COLOR=always CARGO_HOME=/usr/local/cargo
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add wasm32-unknown-unknown \
    && rustup component add llvm-tools
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    version=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock) \
    && cargo install wasm-bindgen-cli --version "$version" --locked \
    && ui/scripts/wasm.sh

# The web app, built around it.
FROM node:26-slim@sha256:abe7e5a82b61a8ef67b315b85497c9e65173387ccf2f4a029248184b8e020be5 AS web
WORKDIR /src/ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-fund --ignore-scripts
WORKDIR /src
COPY . .
COPY --from=wasm /src/ui/src/core/web/wasm/ ui/src/core/web/wasm/
RUN cd ui && npm run build:web

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:29ee70762214fe8190cea04ef1c0a84a8365879e047ce8f911ae5a5ab2355fa2
LABEL org.opencontainers.image.title="drusite-server" \
      org.opencontainers.image.description="The Drusite sync server" \
      org.opencontainers.image.source="https://github.com/pranavajk1/drusite" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/drusite-server /usr/local/bin/drusite-server
COPY --from=web /src/ui/dist-web/ /usr/share/drusite/web/
ENV DRUSITE__SERVER__LISTEN=0.0.0.0:8080 DRUSITE__WEB__DIR=/usr/share/drusite/web
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/drusite-server"]
CMD ["serve"]
