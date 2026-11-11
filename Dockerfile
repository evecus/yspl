# ysp-live v9.5.1 —— Rust 全量 1:1 重写版
# 多阶段构建：静态 musl 二进制 + Alpine 运行时（保留 Node.js 伴生引擎，按 1:1 要求）
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
COPY --from=builder /build/target/release/ysp-live /app/ysp-live
COPY ysp-engine.js /app/ysp-engine.js
EXPOSE 8767
VOLUME ["/app/data"]
CMD ["sh", "-c", "mkdir -p /app/data && chmod -R 777 /app/data 2>/dev/null || true; exec /app/ysp-live 8767"]
