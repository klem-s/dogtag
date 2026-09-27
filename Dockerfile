FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY overlay overlay
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=build /src/target/release/dogtag /usr/local/bin/dogtag
ENTRYPOINT ["dogtag"]
