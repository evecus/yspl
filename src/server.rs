use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::{Request, State};
use axum::http::{header::HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;

use crate::constants::*;
use crate::error::YsptpError;
use crate::hls;
use crate::identity::playback_request_headers;
use crate::state::AppState;
use crate::util::{header_value_clean, log, now_f64};

fn hv(v: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&header_value_clean(v)).ok()
}

fn send(
    code: u16,
    body: Vec<u8>,
    ctype: &str,
    head_only: bool,
    extra: &[(String, String)],
) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(code).unwrap_or(StatusCode::OK))
        .header("Server", format!("yspl/{VERSION}"))
        .header("Content-Type", ctype)
        .header("Content-Length", body.len().to_string())
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-cache, no-store, max-age=0");
    for (k, v) in extra {
        if let Some(val) = hv(v) {
            builder = builder.header(k.as_str(), val);
        }
    }
    let out = if head_only { Body::empty() } else { Body::from(body) };
    builder.body(out).unwrap_or_else(|_| Response::new(Body::empty()))
}

fn text_response(code: u16, s: &str, head_only: bool) -> Response {
    send(code, s.as_bytes().to_vec(), "text/plain; charset=utf-8", head_only, &[])
}

fn header(req: &Request, name: &str) -> Option<String> {
    req.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
}

fn forwarded_proto(req: &Request) -> String {
    let raw = header(req, "X-Forwarded-Proto").unwrap_or_default();
    let s = raw.split(',').next().unwrap_or("").trim().to_lowercase();
    if s.is_empty() { "http".to_string() } else { s }
}

fn host_header(req: &Request, app: &AppState) -> String {
    header(req, "Host").unwrap_or_else(|| format!("localhost:{}", app.args.port))
}

fn match_channel_path(path: &str) -> Option<String> {
    let slug = path.strip_prefix('/')?.strip_suffix(".m3u8")?;
    if slug.is_empty() || !slug.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    Some(slug.to_string())
}

pub async fn serve(app: Arc<AppState>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = Router::new().fallback(handle).with_state(app.clone());
    let addr: SocketAddr = format!("{}:{}", app.cli.bind, app.cli.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log(format!("[系统] HTTP 服务已监听 {addr}"), 0.0);
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

#[axum::debug_handler]
async fn handle(
    State(app): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    let head_only = req.method() == axum::http::Method::HEAD;
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let full_path = if query.is_empty() { path.clone() } else { format!("{path}?{query}") };

    if path == "/" || path == "/index.html" {
        let host = host_header(&req, &app);
        let text = format!(
            "====================================================\n\
yspl IPTV 直播网关运行中 (端口: {})\n\
====================================================\n\n\
全频道订阅: http://{host}/all.m3u\n\
系统健康度: http://{host}/health\n\
实时诊断日志: http://{host}/diag\n",
            app.args.port
        );
        return text_response(200, &text, head_only);
    }
    if path == "/favicon.ico" {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\"><text y=\".9em\" font-size=\"90\">📺</text></svg>";
        return send(200, svg.as_bytes().to_vec(), "image/svg+xml", head_only, &[]);
    }
    if path == "/health" {
        return health_response(&app, head_only);
    }
    if path == "/all.m3u" {
        let host = host_header(&req, &app);
        let proto = forwarded_proto(&req);
        let qs: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let only_4k = matches!(qs.get("k4").map(|s| s.to_lowercase()).as_deref(), Some("1" | "true" | "yes"));
        let body = hls::build_all_m3u(&app, &host, &proto, qs.get("group").map(|s| s.as_str()), only_4k);
        return send(200, body.into_bytes(), "application/vnd.apple.mpegurl", head_only, &[]);
    }
    if path == "/diag" {
        return diag_response(&app, head_only);
    }
    if path == "/proxy.ts" || path == "/proxy" {
        let range = header(&req, "Range");
        return proxy_ts(&app, range, &full_path, head_only).await;
    }
    if path == "/engine_proxy" {
        return engine_proxy(&app, &query, head_only).await;
    }
    if let Some(raw_slug) = match_channel_path(&path) {
        let proto = forwarded_proto(&req);
        let host = host_header(&req, &app);
        return channel_playlist(&app, &proto, &host, addr, &raw_slug, &query, head_only).await;
    }
    text_response(404, "not found\n", head_only)
}

fn health_response(app: &AppState, head_only: bool) -> Response {
    let now = now_f64();
    let (sess_valid, heartbeat_count, guid, active_slots) = {
        let r = app.resolver.read();
        if let Some(r) = r.as_ref() {
            let st = r.state.lock();
            let slots = st.sessions.values().filter(|s| r.session_fresh(s.lock().created_at)).count();
            let sess = st.sessions.get(&0).cloned();
            drop(st);
            if let Some(s) = sess {
                let s = s.lock();
                (r.session_fresh(s.created_at), s.heartbeat_count, s.cloud_guid.clone(), slots)
            } else {
                (false, 0, String::new(), slots)
            }
        } else {
            (false, 0, String::new(), 0)
        }
    };
    let health = serde_json::json!({
        "ok": true,
        "version": VERSION,
        "uptime_sec": (now - app.server_start_time) as i64,
        "engine": {
            "ready": app.ready(),
            "active_device_slots": active_slots,
            "session_valid": sess_valid,
            "heartbeat_count": heartbeat_count,
            "guid": guid,
        },
        "channels": app.directory.count(),
        "ts_cache": app.ts_cache.stats(),
        "routes": ["/", "/all.m3u", "/diag", "/health", "/<channel>.m3u8", "/proxy.ts"],
    });
    let body = serde_json::to_string_pretty(&health).unwrap_or_default();
    send(200, format!("{body}\n").into_bytes(), "application/json; charset=utf-8", head_only, &[])
}

fn diag_response(app: &AppState, head_only: bool) -> Response {
    let now = now_f64();
    let mut info = vec![
        "====================================================".to_string(),
        format!("yspl v0.1.0 诊断报告 (运行时间: {}s)", (now - app.server_start_time) as i64),
        "====================================================".to_string(),
    ];
    let engine_status = if app.ready() { "就绪" } else { "未就绪(1080p自动回退)" };
    let (valid, occupied, hb, guid, hb_err) = {
        let r = app.resolver.read();
        if let Some(r) = r.as_ref() {
            if let Some(sess) = r.state.lock().sessions.get(&0).cloned() {
                r.evict_idle_links(&sess, LINK_IDLE_TIMEOUT);
                let s = sess.lock();
                (r.session_fresh(s.created_at), s.linked_channels.len(), s.heartbeat_count,
                 s.cloud_guid.clone(), s.last_heartbeat_error.clone())
            } else {
                (false, 0, 0, String::new(), String::new())
            }
        } else {
            (false, 0, 0, String::new(), String::new())
        }
    };
    let cfg_links = app.args.links_per_device;
    info.push(format!("单设备并发池状态: {engine_status}"));
    info.push(format!(
        "高码率设备状态 ({} 路共用): {}, 占用={}/{}, 心跳={}, GUID={}",
        app.active_backend.len(), if valid { "有效" } else { "未就绪" }, occupied, cfg_links, hb,
        if guid.is_empty() { "无" } else { &guid }
    ));
    if !hb_err.is_empty() {
        info.push(format!("Slot 0 心跳日志: {hb_err}"));
    }
    let c = app.ts_cache.stats();
    info.push(format!(
        "分片内存缓存: 条目={}, 占用={}MB, 命中率={} (命中:{}/未中:{})",
        c["count"], c["size_mb"], c["hit_rate"], c["hits"], c["misses"]
    ));
    info.push("--- 频道状态列表 ---".to_string());
    for meta in app.directory.unique_channels() {
        let (age, mode, err) = {
            let ch = app.get_or_create_channel(&meta.slug);
            if let Some(ch) = ch {
                let c = ch.lock();
                let age = if c.last_ok > 0.0 {
                    format!("{}s前", (now - c.last_ok) as i64)
                } else {
                    "从未拉取".to_string()
                };
                let mode = if app.active_backend.contains(&meta.slug) && app.ready() {
                    "4K/高码率".to_string()
                } else {
                    c.mode.clone()
                };
                (age, mode, c.last_error.clone())
            } else {
                ("从未拉取".to_string(), "idle".to_string(), String::new())
            }
        };
        let mut cooldown = String::new();
        let r = app.resolver.read();
        if let Some(r) = r.as_ref() {
            if r.is_channel_in_cooldown(&meta.slug) {
                if let Some((failed_at, _, cnt)) = r.get_channel_failure_info(&meta.slug) {
                    let cd = crate::resolver::Resolver::cooldown_seconds(cnt);
                    let rem = (cd - (now - failed_at)).max(0.0) as i64;
                    cooldown = format!(" [高码冷却剩{rem}s]");
                }
            }
        }
        info.push(format!(
            "{} ({}) 模式={}{} 上次刷新={} 错误={}",
            meta.slug, meta.name, mode, cooldown, age, if err.is_empty() { "无" } else { &err }
        ));
    }
    let logs = crate::util::recent_logs(30);
    if !logs.is_empty() {
        info.push("--- 最近运行事件 (最新 30 条) ---".to_string());
        info.extend(logs);
    }
    text_response(200, &format!("{}\n", info.join("\n")), head_only)
}

async fn proxy_ts(app: &Arc<AppState>, range_header: Option<String>, full_path: &str, head_only: bool) -> Response {
    let Some(token) = hls::proxy_ts_token(full_path) else {
        return text_response(400, "missing proxy ts token\n", head_only);
    };
    let real_url = match hls::decode_proxy_ts_url(&token) {
        Ok(u) => u,
        Err(e) => return text_response(400, &format!("{e}\n"), head_only),
    };
    if range_header.is_none() {
        if let Some((data, headers)) = app.ts_cache.get(&real_url) {
            let mut extra: Vec<(String, String)> = headers.clone();
            extra.push(("X-Cache".into(), "HIT-RAM".into()));
            extra.push(("Access-Control-Allow-Origin".into(), "*".into()));
            extra.push(("Cache-Control".into(), "public, max-age=300".into()));
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header("Server", format!("yspl/{VERSION}"))
                .header("Content-Length", data.len().to_string());
            for (k, v) in &extra {
                if let Some(val) = hv(v) {
                    builder = builder.header(k.as_str(), val);
                }
            }
            return builder.body(if head_only { Body::empty() } else { Body::from(data) })
                .unwrap_or_else(|_| Response::new(Body::empty()));
        }
    }
    let playback_headers = {
        let r = app.resolver.read();
        match r.as_ref() {
            Some(r) => match r.playback_headers_for_ts_url(&real_url) {
                Ok(h) => h,
                Err(e) => return text_response(500, &format!("{e}\n"), head_only),
            },
            None => HashMap::new(),
        }
    };
    let parsed = match url::Url::parse(&real_url) {
        Ok(u) => u,
        Err(_) => return text_response(400, "invalid ts host\n", head_only),
    };
    let host = parsed.host_str().unwrap_or("").to_string();
    if host.is_empty() {
        return text_response(400, "invalid ts host\n", head_only);
    }
    let host_header_val = match parsed.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    let mut extra_pairs: Vec<(String, String)> = Vec::new();
    if let Some(r) = &range_header {
        extra_pairs.push(("Range".into(), header_value_clean(r)));
    }
    let mut headers = playback_request_headers(&playback_headers, Some(&extra_pairs));
    headers.push(("Host".into(), host_header_val));
    let raw = hls::client().raw_client();
    let response = raw
        .get(&real_url)
        .headers(to_header_map(&headers))
        .timeout(Duration::from_secs(30))
        .send()
        .await;
    let resp = match response {
        Ok(r) => r,
        Err(e) => return text_response(502, &format!("Upstream CDN error: {e}\n"), head_only),
    };
    let status = resp.status().as_u16();
    let pass: Vec<(String, String)> = resp
        .headers()
        .iter()
        .filter(|(k, _)| {
            matches!(k.as_str(), "content-type" | "content-range" | "accept-ranges" | "content-length" | "location")
        })
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();
    let expected_len = pass.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok());
    let is_oversized = expected_len.map(|n| n > TS_MAX_BYTES).unwrap_or(false);
    let body_bytes = if head_only { Vec::new() } else { resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default() };
    if status == 200 && range_header.is_none() && !is_oversized && !body_bytes.is_empty()
        && expected_len.map(|n| n == body_bytes.len()).unwrap_or(true) && body_bytes.len() <= TS_MAX_BYTES {
            app.ts_cache.set(&real_url, body_bytes.clone(), pass.clone());
        }
    let mut extra = pass;
    extra.push(("X-Cache".into(), "MISS-UPSTREAM".into()));
    extra.push(("Access-Control-Allow-Origin".into(), "*".into()));
    extra.push(("Cache-Control".into(), "public, max-age=300".into()));
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
        .header("Server", format!("yspl/{VERSION}"))
        .header("Content-Length", body_bytes.len().to_string());
    for (k, v) in &extra {
        if let Some(val) = hv(v) {
            builder = builder.header(k.as_str(), val);
        }
    }
    builder.body(if head_only { Body::empty() } else { Body::from(body_bytes) })
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn to_header_map(headers: &[(String, String)]) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(n), Ok(val)) = (
            k.parse::<reqwest::header::HeaderName>(),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            map.insert(n, val);
        }
    }
    map
}

async fn engine_proxy(app: &Arc<AppState>, query: &str, head_only: bool) -> Response {
    let qs: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let target = qs.get("url").cloned().unwrap_or_default();
    if target.is_empty() {
        return text_response(400, "missing url\n", head_only);
    }
    if !hls::is_safe_engine_proxy_url(app, &target, 0) {
        return text_response(403, "forbidden url\n", head_only);
    }
    let headers = [("User-Agent", UA), ("Accept", "*/*")];
    match hls::client()
        .send_timeout(reqwest::Method::GET, &target, &headers, None, Duration::from_secs(10))
        .await
    {
        Ok(resp) => {
            if resp.body.len() > TS_MAX_BYTES {
                return text_response(502, "upstream segment too large\n", head_only);
            }
            let ctype = resp.header("Content-Type").unwrap_or("video/mp2t").to_string();
            send(resp.status, resp.body, &ctype, head_only, &[])
        }
        Err(e) => text_response(502, &format!("engine proxy error: {e}\n"), head_only),
    }
}

async fn channel_playlist(
    app: &Arc<AppState>,
    proto: &str,
    host: &str,
    addr: SocketAddr,
    raw_slug: &str,
    query: &str,
    head_only: bool,
) -> Response {
    let canonical = crate::channels::canonical(raw_slug);
    let Some(ch) = app.get_or_create_channel(&canonical) else {
        return text_response(404, "未知频道\n", head_only);
    };

    if let Some((start, end)) = hls::parse_catchup_query(query) {
        if crate::channels::supports_timeshift(&canonical) {
            let (pid, sid, defn) = {
                let c = ch.lock();
                (c.pid.clone(), c.sid.clone(), c.defn.clone())
            };
            match crate::jce::jce_timeshift_url(hls::client(), &pid, &sid, start, end, &defn).await {
                Ok(m3u8_url) => {
                    if let Ok(pl) = crate::jce::fetch_abs_playlist(hls::client(), &m3u8_url, 0).await {
                        return send(200, pl.into_bytes(), "application/vnd.apple.mpegurl", head_only, &[]);
                    }
                }
                Err(e) => log(format!("[{canonical}] 时移获取失败: {e}，回退直播"), 10.0),
            }
        }
    }

    let client_ip = addr.ip().to_string();
    let resolver = app.resolver.read().clone();
    let mut high_lock_allowed = false;
    if app.active_backend.contains(&canonical) && app.ready() {
        if let Some(r) = &resolver {
            if !r.is_channel_in_cooldown(&canonical) {
                high_lock_allowed = app.high_locks.check_and_update(&canonical, &client_ip);
            }
        }
    }
    if app.active_backend.contains(&canonical) && app.ready() && high_lock_allowed {
        if let Some(r) = &resolver {
            if !r.is_channel_in_cooldown(&canonical) {
                match r.fetch_channel_playlist(&canonical, host, proto).await {
                    Ok(Some(pl)) if pl.contains("#EXTM3U") => {
                        let prev_src = {
                            let mut c = ch.lock();
                            let prev = c.last_m3u8_source.clone();
                            c.last_m3u8_source = "device".to_string();
                            c.last_built_playlist.clear();
                            c.last_built_time = 0.0;
                            prev
                        };
                        let body = if prev_src == "bk" { hls::inject_hls_discontinuity(&pl) } else { pl };
                        return send(200, body.into_bytes(), "application/vnd.apple.mpegurl", head_only, &[]);
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {

                    }
                    Err(e) => {
                        record_high_failure(r, &canonical, &e);
                    }
                }
            }
        }
    }

    let now_ts = now_f64();
    ch.lock().last_access = now_ts;
    let (cached_pl, cached_age) = {
        let c = ch.lock();
        (c.last_built_playlist.clone(), now_ts - c.last_built_time)
    };
    if !cached_pl.is_empty() && cached_age < 15.0 && cached_pl.contains("#EXTM3U") {
        let prev_src = {
            let mut c = ch.lock();
            let prev = c.last_m3u8_source.clone();
            c.last_m3u8_source = "bk".to_string();
            prev
        };
        let body = if prev_src == "device" { hls::inject_hls_discontinuity(&cached_pl) } else { cached_pl };
        let resp = send(200, body.into_bytes(), "application/vnd.apple.mpegurl", head_only,
            &[("X-SWR".into(), "HIT".into())]);
        if cached_age > 6.0 {
            hls::trigger_background_refresh(app, &ch);
        }
        return resp;
    }

    hls::ensure_channel(app, &ch).await;
    if let Some(pl) = hls::build_playlist(&ch, true) {
        if pl.contains("#EXTM3U") {
            let prev_src = {
                let mut c = ch.lock();
                let prev = c.last_m3u8_source.clone();
                c.last_m3u8_source = "bk".to_string();
                prev
            };
            let body = if prev_src == "device" { hls::inject_hls_discontinuity(&pl) } else { pl };
            return send(200, body.into_bytes(), "application/vnd.apple.mpegurl", head_only, &[]);
        }
    }
    if let Some(pl) = hls::fetch_upstream_engine_playlist(app, &canonical, host, proto).await {
        if pl.contains("#EXTM3U") {
            return send(200, pl.into_bytes(), "application/vnd.apple.mpegurl", head_only, &[]);
        }
    }
    let (name, err) = {
        let c = ch.lock();
        (c.name.clone(), c.last_error.clone())
    };
    text_response(503, &format!("频道 {name} 暂无数据 ({}), 请稍后重试\n", if err.is_empty() { "拉取中" } else { &err }), head_only)
}

fn record_high_failure(r: &Arc<crate::resolver::Resolver>, canonical: &str, e: &YsptpError) {
    {
        let mut st = r.state.lock();
        st.cache.remove(canonical);
        st.playlist_cache.remove(canonical);
        let cnt = st.channel_failures.get(canonical).map(|x| x.2 + 1).unwrap_or(1);
        st.channel_failures.insert(canonical.to_string(), (now_f64(), e.message().to_string(), cnt));
    }
    let fail_cnt = r.get_channel_failure_info(canonical).map(|x| x.2).unwrap_or(1);
    let cd_sec = crate::resolver::Resolver::cooldown_seconds(fail_cnt) as i64;
    log(format!(
        "[{canonical}] 高码率播放暂时受限 ({})，平滑回退 1080p (冷却 {cd_sec}s)",
        e.message().trim()
    ), 5.0);
}
