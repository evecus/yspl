FROM docker.m.daocloud.io/library/rust:1.93-alpine AS builder
RUN apk add --no-cache musl-dev pkgconf ca-certificates
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM docker.m.daocloud.io/library/alpine:3.20
ENV TZ=Asia/Shanghai \
    YSP_DATA_DIR=/app/data \
    NODE_OPTIONS="--max-old-space-size=256 --optimize-for-size"
RUN apk add --no-cache nodejs tzdata ca-certificates && \
    cp /usr/share/zoneinfo/Asia/Shanghai /etc/localtime && \
    echo "Asia/Shanghai" > /etc/timezone && \
    mkdir -p /app /app/data
WORKDIR /app
COPY --from=builder /build/target/release/yspl /app/yspl
COPY ysp-engine.js /app/ysp-engine.js
EXPOSE 8767
VOLUME ["/app/data"]
CMD ["sh", "-c", "mkdir -p /app/data && chmod -R 777 /app/data 2>/dev/null || true; exec /app/yspl 8767"]
