# Build from the workspace root: docker build -f crates/peerly-rendezvous/Dockerfile -t peerly-rendezvous .
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p peerly-rendezvous

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/peerly-rendezvous /usr/local/bin/peerly-rendezvous
EXPOSE 9009/tcp 9009/udp
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/peerly-rendezvous", "--listen", "0.0.0.0:9009"]
