use std::sync::Arc;

use clap::Parser as _;
use yspl::engine::{start_companion_engine, stop_companion_engine};
use yspl::resolver::Resolver;
use yspl::state::AppState;
use yspl::util::log;
use yspl::{server, Cli};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let app = Arc::new(AppState::new(cli.clone()));

    let data_dir = app.data_dir.clone();
    if !data_dir.exists() {
        let _ = std::fs::create_dir_all(&data_dir);
    }
    let test_file = data_dir.join(format!(".perm_test_{}", std::process::id()));
    match std::fs::write(&test_file, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&test_file);
            log(format!("[系统] 持久化存储目录: {} (读写权限正常)", data_dir.display()), 0.0);
        }
        Err(e) => {
            log(format!("[系统] 警告: 持久化目录 {} 写入受限 ({e})，状态将保持内存模式", data_dir.display()), 0.0);
        }
    }

    log(format!(
        "[系统] 高码率模式: {}",
        if cli.no_4k {
            "已禁用 (--no-4k)，全频道走 1080p JCE/bkliveinfo".to_string()
        } else if cli.only_4k || yspl::constants::env_only_4k() {
            "仅 4K/8K 频道走设备协议 (其余走 1080p JCE/bkliveinfo)".to_string()
        } else {
            format!(
                "全部 {} 个高码率频道走单设备并发池 (1 台设备, 高码率并发上限: {} 路, 空闲超 {}s 释放配额, 换台防抖: {}s)",
                app.active_backend.len(), app.args.links_per_device,
                yspl::constants::LINK_IDLE_TIMEOUT, cli.zap_debounce
            )
        }
    ), 0.0);

    let mut engine_child = if cli.no_engine { None } else { start_companion_engine(&app) };

    let resolver = Resolver::new(app.clone())?;
    *app.resolver.write() = Some(resolver.clone());
    if !cli.no_4k {
        resolver.start_background();
    }

    let count = app.directory.count();
    let c_yangshi = app.directory.count_cctv();
    let c_weishi = app.directory.count_weishi();
    log("====================================================================", 0.0);
    log(format!("yspl v0.1.0 启动成功: {count} 个独立频道 (央视 {c_yangshi} 路, 卫视 {c_weishi} 路)"), 0.0);
    log(format!("全频道订阅:   http://localhost:{}/all.m3u", cli.port), 0.0);
    log(format!("系统健康度:   http://localhost:{}/health", cli.port), 0.0);
    log(format!("实时诊断日志: http://localhost:{}/diag", cli.port), 0.0);
    log("====================================================================", 0.0);

    tokio::select! {
        res = server::serve(app.clone()) => {
            if let Err(e) = res {
                log(format!("[系统] HTTP 服务异常退出: {e}"), 0.0);
            }
        }
        _ = shutdown_signal() => {
            log("[系统] 服务已停止", 0.0);
        }
    }
    stop_companion_engine(&mut engine_child);
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
