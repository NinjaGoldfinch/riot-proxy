# syntax=docker/dockerfile:1
# docs/design/07 §Option A. Static musl binary in a scratch image.
# The builder tag pins the toolchain (rust-toolchain.toml is dockerignored); bump both together.
FROM rust:1.98.1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src

# Build dependencies alone first so source edits reuse the cached layer.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked && rm -rf src

COPY . .
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM scratch
COPY --from=build /src/target/release/riot-proxy /riot-proxy
VOLUME /data
EXPOSE 8080
ENV DATA_DIR=/data
HEALTHCHECK --interval=30s --timeout=3s CMD ["/riot-proxy", "healthcheck"]
ENTRYPOINT ["/riot-proxy"]
CMD ["serve"]
