FROM rust:1.99-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock 223fz_27_agents_ru_unified.json ./
COPY src src
COPY ui ui
RUN cargo build --locked --release --bin review_ui
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates poppler-utils && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/review_ui /usr/local/bin/review_ui
USER 65534:65534
CMD ["review_ui"]
