# syntax=docker/dockerfile:1
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# drusite-server on a distroless base: no shell, no package manager, a non-root user. Build:
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
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package drusite-server \
    && install -D target/release/drusite-server /out/drusite-server

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:29ee70762214fe8190cea04ef1c0a84a8365879e047ce8f911ae5a5ab2355fa2
LABEL org.opencontainers.image.title="drusite-server" \
      org.opencontainers.image.description="The Drusite sync server" \
      org.opencontainers.image.source="https://github.com/pranavajk1/drusite" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/drusite-server /usr/local/bin/drusite-server
ENV DRUSITE__SERVER__LISTEN=0.0.0.0:8080
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/drusite-server"]
CMD ["serve"]
