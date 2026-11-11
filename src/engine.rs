//! 对应 Python `start_companion_engine` / `stop_companion_engine` /
//! `is_port_listening` / `atexit` / SIGTERM 处理。
//! 按「全量 1:1」要求保留 Node.js 伴生引擎（ysp-engine.js 原样随镜像分发）。

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::state::AppState;
use crate::util::log;

pub fn is_port_listening(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

fn which(prog: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(prog);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_engine_file() -> Option<PathBuf> {
    let script_dir = crate::device::binary_dir();
    let candidates = [
        script_dir.join("ysp-engine.js"),
        std::env::current_dir().unwrap_or_default().join("ysp-engine.js"),
        PathBuf::from("/app/ysp-engine.js"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// 对应 `start_companion_engine(script_dir, port=8787)`，返回子进程句柄。
pub fn start_companion_engine(app: &AppState) -> Option<Child> {
    let port = app.engine_port;
    if is_port_listening(port) {
        log(format!("[引擎] Web WASM 引擎已就绪: 端口 {port} 已处于运行状态"), 0.0);
        return None;
    }
    let Some(engine_file) = find_engine_file() else {
        log("[引擎] 未找到 ysp-engine.js，Web WASM 伴生引擎未启用", 0.0);
        return None;
    };
    let Some(node) = which("node") else {
        log("[引擎] 未检测到 node 可执行程序，Web WASM 伴生引擎未启用 (走设备协议/1080p 回退)", 0.0);
        return None;
    };
    match Command::new(node)
        .arg(&engine_file)
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(proc) => {
            for _ in 0..30 {
                if is_port_listening(port) {
                    log(format!("[引擎] Web WASM 伴生引擎 (Node.js) 自动启动成功: 端口 {port}"), 0.0);
                    return Some(proc);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            log(format!("[引擎] Web WASM 伴生引擎已在后台启动 (端口 {port})"), 0.0);
            Some(proc)
        }
        Err(e) => {
            log(format!("[引擎] Web WASM 伴生引擎启动失败: {e}"), 0.0);
            None
        }
    }
}

/// 对应 `stop_companion_engine`。
pub fn stop_companion_engine(child: &mut Option<Child>) {
    let Some(mut p) = child.take() else { return };
    if let Ok(None) = p.try_wait() {
        let _ = p.kill();
        let _ = p.wait();
        log("[引擎] Web WASM 伴生引擎 (Node.js) 已停止", 0.0);
    }
}

/// 检查 engine_proxy 的目标是否是本地伴生引擎的合法 URL（见 hls.rs）。
pub fn engine_file_exists() -> bool {
    find_engine_file().is_some()
}
