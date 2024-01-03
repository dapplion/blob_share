FROM rust:1.73.0-bullseye AS builder
RUN apt-get update && apt-get -y upgrade && apt-get install -y cmake libclang-dev
WORKDIR /app
COPY . . 
RUN SQLX_OFFLINE=1 cargo build --release

# Final layer to minimize size
FROM gcr.io/distroless/cc-debian11
COPY --from=builder /app/bundler/static /static
COPY --from=builder /app/target/release/blobshare /blobshare
COPY --from=builder /app/target/release/bundler_client_cli /bundler_client_cli
ENTRYPOINT ["/blobshare"]
