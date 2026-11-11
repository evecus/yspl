//! ysp-live v9.5.1 —— 央视频全频道直播代理，Python 版全量 1:1 Rust 重写。
//!
//! 模块划分与 Python 主文件 `ysp-live-v9.5.py` 的段落一一对应：
//! - `constants` / `channels` / `device_pool`：静态常量与数据表
//! - `crypto`：手写 AES-256-GCM / RSA-OAEP-SHA256 / TEA / digest（改用成熟 crate）
//! - `jce`：JCE(Tars) W/R 编解码、QMF 封包、cKey、时移与 bkliveinfo
//! - `device` / `identity` / `protocol`：设备身份、指纹、gsnw 设备协议全套流程
//! - `http_client`：urllib/http.client 的等价实现（禁代理、IPv4-only、gzip、重试）
//! - `resolver`：会话池、换台防抖、配额、冷却、SWR、后台刷新
//! - `hls`：playlist 重写、TS 分片代理与缓存、M3U 订阅、时移解析
//! - `server`：HTTP 路由（对应 Handler.do_GET / do_HEAD）
//! - `engine`：Node.js 伴生引擎（按 1:1 要求保留）
//! - `meta`：*-rs.json 持久化与诊断状态

pub mod channels;
pub mod channels_data;
pub mod constants;
pub mod crypto;
pub mod device;
pub mod device_pool;
pub mod engine;
pub mod error;
pub mod hls;
pub mod http_client;
pub mod identity;
pub mod jce;
pub mod meta;
pub mod protocol;
pub mod resolver;
pub mod server;
pub mod state;
pub mod util;

pub use state::Cli;
