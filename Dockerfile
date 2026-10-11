# Build stage compiles kelvin; the runtime image carries only the stripped binary.
# TLS is rustls with bundled roots, so the runtime needs no OpenSSL or CA package.
FROM rust:1.98.1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations migrations
COPY src src
RUN cargo build --release --locked && strip target/release/kelvin

FROM debian:trixie-20260918-slim
COPY --from=build /src/target/release/kelvin /usr/local/bin/kelvin
USER nobody
ENTRYPOINT ["kelvin"]
CMD ["run", "--network", "testnet", "--api", "0.0.0.0:8080"]
