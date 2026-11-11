# ysp-live-rs

央视频（YSP）直播网关的 **Rust 全量 1:1 重写**，基线为原版 `ysp-live-v9.5.py`（v9.5.1）。
所有协议、状态机、路由、持久化格式（`device-state-rs.json` / `proxy-cache-state-rs.json`）、
CLI 参数与 HTTP 行为均与 Python 版逐字节对齐；Node 伴生引擎 `ysp-engine.js` 与
`/engine_proxy` 回退按原版保留。

## 与 Python 版对比

| 维度 | Python + Node | Rust |
|---|---|---|
| 进程 | Python + Node 双进程 | 1（伴生引擎仍按原版自动拉起） |
| 并发 | ThreadingHTTPServer 每请求一线程 | tokio 异步 |
| 手写密码学 | 纯 Python AES-GCM/RSA-OAEP/TEA | `aes-gcm` / `rsa` / 自写 TEA（对拍对齐） |
| 镜像 | python:3.12-alpine ~100MB | distroless 静态 ~10MB |
| 产物 | — | 单二进制，amd64/arm64 musl |

## 构建与运行

```bash
cargo build --release
./target/release/ysp-live 8767 --data-dir ./data
# 参数与 Python 版一致：PORT（位置参数）、--bind、--data-dir、--cache-mb、--no-4k、--engine-port 等
```

Docker：

```bash
docker compose up -d        # 端口 8767，数据卷 /app/data
```

数据目录可直接复用 Python 版的 `/app/data` 卷（JSON 格式与键序完全兼容）。

## 模块对应

| Rust 模块 | Python 对应 |
|---|---|
| `src/main.rs` | `main()` + argparse |
| `src/server.rs` | `Handler(BaseHTTPRequestHandler)` 全部路由 |
| `src/resolver.rs` | `Resolver`（会话轮换/防抖/配额/SWR/400 自愈） |
| `src/protocol.rs` | app/start、cloud、live、VDN、心跳、JCE/bk 取流 |
| `src/jce.rs` | JCE(Tars) TLV + QMF 封包 + cKey(TEA) |
| `src/crypto/` | 手写 AES-GCM / RSA-OAEP / TEA（crate 实现，语义对拍） |
| `src/device.rs` | 设备指纹模板池 + `device-state-rs.json` |
| `src/hls.rs` | playlist 重写 / TS 代理 / LRU / 时移 / 备用流 |
| `src/state.rs` | `ResolverState` + 全局锁 + TS 缓存 + 高码率锁 |
| `src/channels*.rs` | 64 频道表 + ALIAS_MAP + TVG_IDS（脚本从源码机械抽取） |
| `src/engine.rs` | 伴生引擎拉起 + `/engine_proxy` 回退（原版保留） |

## CI

`.github/workflows/ci.yml`：`cargo clippy --locked -D warnings` + `cargo test`，
矩阵 `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`，产物附到 Release。

## 测试

```bash
cargo test    # 36 项：b64/JCE/cKey/指纹/别名/TS 链路/持久化键序等对拍
```

## 声明

本项目仅为原版 ysp-live 的语言迁移，用途与授权范围同原版；取流稳定性取决于上游协议与风控，与语言无关。
