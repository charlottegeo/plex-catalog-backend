FROM rust:1.90.0-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl-dev \
    pkg-config \
    perl \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .
ENV SQLX_OFFLINE=true
RUN cargo build --release
EXPOSE 3001
CMD ["./target/release/plex-catalog-backend"]
