FROM rust:1-bookworm AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/litellm-relay /usr/local/bin/litellm-relay

EXPOSE 4142
ENTRYPOINT ["litellm-relay"]
CMD ["serve"]
