# riot-proxy v2 developer commands. `just --list` shows them all.

set dotenv-load := false  # the binary reads .env itself (config precedence)

image := "riot-proxy:dev"
musl_target := "x86_64-unknown-linux-musl"

default:
    @just --list

# Run the proxy locally with ./.env
dev *ARGS:
    cargo run -- serve {{ARGS}}

# Run every test
test *ARGS:
    cargo test {{ARGS}}

# rustfmt check + clippy with warnings as errors (what CI runs)
lint:
    cargo fmt --all --check
    cargo clippy --all-targets --all-features -- -D warnings

# Coverage summary (needs cargo-llvm-cov)
cov:
    cargo llvm-cov --all-features --workspace --summary-only

# Static release binary (needs musl-gcc, e.g. apt install musl-tools)
musl:
    cargo build --release --target {{musl_target}}
    ls -la target/{{musl_target}}/release/riot-proxy

# Build the FROM-scratch image
docker:
    docker build -t {{image}} .

# One raw Riot call with the key from ./.env, e.g. `just riot account/by-riot-id europe Faker KR1`
riot *ARGS:
    cargo run --features dev-cli -- riot get {{ARGS}}
