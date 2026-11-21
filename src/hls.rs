use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone};
use parking_lot::Mutex;
use url::Url;

use crate::constants::*;
use crate::error::{Result, YsptpError};
use crate::http_client::HttpClient;
use crate::identity::{is_ts_url, playback_request_headers};
use crate::state::{AppState, SharedChannel};
use crate::util::{
    b64_urlsafe_nopad_decode, b64_urlsafe_nopad_encode, log, now_f64, rust_round,
};

static HTTP: OnceLock<HttpClient> = OnceLock::new();

pub fn client() -> &'static HttpClient {
    HTTP.get_or_init(|| HttpClient::new(10.0, false).expect("HTTP client init failed"))
}

pub struct ApiFlowLimiter {
    min_interval: f64,
    jitter: f64,
    last_call: Mutex<f64>,
}

impl ApiFlowLimiter {
    pub fn new(min_interval_ms: i64, jitter_ms: i64) -> Self {
        Self {
            min_interval: min_interval_ms as f64 / 1000.0,
            jitter: jitter_ms as f64 / 1000.0,
            last_call: Mutex::new(0.0),
        }
    }

    pub async fn wait_slot(&self) {
        let now = now_f64();
        let target_delay = self.min_interval + rand::random::<f64>() * self.jitter;
        let diff = now - *self.last_call.lock();
        if diff < target_delay {
            tokio::time::sleep(Duration::from_secs_f64(target_delay - diff)).await;
        }
        *self.last_call.lock() = now_f64();
    }
}

pub fn rs_lines(text: &str) -> Vec<String> {
    let norm = text.replace("\r\n", "\n");
    let mut lines: Vec<String> = norm.split('\n').map(|s| s.to_string()).collect();
    if norm.ends_with('\n') {
        lines.pop();
    }
    for ln in &mut lines {
        if ln.ends_with('\r') {
            ln.pop();
        }
    }
    lines
}

pub fn rewrite_playlist_urls(text: &str, base_url: &str) -> String {
    let has_base = Url::parse(base_url)
        .map(|u| !u.scheme().is_empty() && u.host_str().is_some())
        .unwrap_or(false);
    let base = Url::parse(base_url).ok();
    let mut out = String::new();
    for line in rs_lines(text) {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            out.push_str(&line);
        } else if has_base {
            let joined = base
                .as_ref()
                .and_then(|b| b.join(stripped).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| stripped.to_string());
            out.push_str(&joined);
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }
    out
}

pub fn proxy_playlist_ts_urls(playlist: &str, proxy_origin: &str, proxy_prefix: Option<&str>) -> String {
    let origin = proxy_prefix.unwrap_or(proxy_origin).trim_end_matches('/');
    let proxy_query = if proxy_prefix.is_some() { "/proxy?ts=" } else { "/proxy.ts?=" };
    let mut out = String::new();
    for line in rs_lines(playlist) {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            out.push_str(&line);
        } else if is_ts_url(stripped) {
            let encoded = b64_urlsafe_nopad_encode(stripped.as_bytes());
            out.push_str(&format!("{origin}{proxy_query}{encoded}"));
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }
    out
}

pub fn inject_hls_discontinuity(pl: &str) -> String {
    if pl.contains("#EXT-X-DISCONTINUITY") {
        return pl.to_string();
    }
    let mut out = Vec::new();
    let mut inserted = false;
    for line in pl.lines() {
        let s = line.trim();
        if !inserted
            && (s.starts_with("#EXT-X-PROGRAM-DATE-TIME:")
                || s.starts_with("#EXTINF:")
                || (!s.is_empty() && !s.starts_with('#')))
        {
            out.push("#EXT-X-DISCONTINUITY".to_string());
            inserted = true;
        }
        out.push(line.to_string());
    }
    if !inserted {
        return pl.to_string();
    }
    let mut s = out.join("\n");
    if pl.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn set_header(h: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(item) = h.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        item.1 = value.to_string();
    } else {
        h.push((key.to_string(), value.to_string()));
    }
}

fn pairs(h: &[(String, String)]) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

pub async fn fetch_playlist_http1(
    client: &HttpClient,
    final_url: &str,
    playback_headers: &HashMap<String, String>,
) -> Result<(u16, String, String)> {
    let mut current_url = final_url.to_string();
    for _ in 0..4 {
        let parsed = Url::parse(&current_url)
            .map_err(|e| YsptpError::new(format!("invalid final URL: {e}")))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(YsptpError::new(format!(
                "manual playlist fetch currently expects http or https URL, got {}", parsed.scheme()
            )));
        }
        let host = parsed.host_str().unwrap_or("").to_string();
        if host.is_empty() {
            return Err(YsptpError::new("final URL missing host"));
        }
        let host_header = match parsed.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.clone(),
        };
        let mut headers = playback_request_headers(playback_headers, None);
        set_header(&mut headers, "Host", &host_header);
        let raw = client.raw_client();
        let resp = raw
            .get(&current_url)
            .headers(to_header_map(&pairs(&headers)))
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| YsptpError::new(crate::error::normalize_network_error(&e)))?;
        let status = resp.status().as_u16();
        let loc = resp.headers().get("location").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/vnd.apple.mpegurl")
            .to_string();
        let body = resp.bytes().await.map_err(|e| YsptpError::new(crate::error::normalize_network_error(&e)))?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = loc {
                current_url = Url::parse(&current_url)
                    .ok()
                    .and_then(|b| b.join(&loc).ok())
                    .map(|u| u.to_string())
                    .unwrap_or(loc);
                continue;
            }
        }
        return Ok((status, content_type, String::from_utf8_lossy(&body).into_owned()));
    }
    Err(YsptpError::new("too many redirects fetching playlist"))
}

fn to_header_map(headers: &[(&str, &str)]) -> reqwest::header::HeaderMap {
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

pub fn query_param(url: &str, name: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    for pair in query.split('&') {
        let (key, _, value) = pair.partition_once('=');
        if key == name {
            return Some(value.to_string());
        }
    }
    None
}

trait PartitionOnce {
    fn partition_once(&self, sep: char) -> (&str, &str, &str);
}
impl PartitionOnce for str {
    fn partition_once(&self, sep: char) -> (&str, &str, &str) {
        match self.find(sep) {
            Some(i) => (&self[..i], &self[i..i + 1], &self[i + 1..]),
            None => (self, "", ""),
        }
    }
}

pub fn proxy_ts_token(url: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    if let Some(t) = query.strip_prefix('=') {
        return if t.is_empty() { None } else { Some(t.to_string()) };
    }
    if let Some(t) = query_param(url, "ts") {
        return Some(t);
    }
    if let Some(t) = query_param(url, "url") {
        return Some(t);
    }
    if !query.is_empty() && !query.contains('=') {
        return Some(query.to_string());
    }
    None
}

pub fn decode_proxy_ts_url(token: &str) -> Result<String> {
    let raw = b64_urlsafe_nopad_decode(token)
        .ok_or_else(|| YsptpError::new("invalid proxy ts token"))?;
    let value = String::from_utf8(raw)
        .map_err(|e| YsptpError::new(format!("proxy ts token is not UTF-8: {e}")))?;
    let parsed = Url::parse(&value)
        .map_err(|e| YsptpError::new(format!("proxy ts token is not a URL: {e}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(YsptpError::new(format!(
            "proxy ts currently requires http or https URL, got {}", parsed.scheme()
        )));
    }
    if !parsed.path().to_lowercase().ends_with(".ts") {
        return Err(YsptpError::new("proxy ts URL is not a .ts segment"));
    }
    Ok(value)
}

fn cst() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).unwrap()
}

pub fn parse_catchup_time_val(val_str: Option<&str>) -> Option<i64> {
    let val = val_str?.trim();
    if val.is_empty() {
        return None;
    }
    let is_utc = val.to_uppercase().ends_with('Z');
    if val.contains('T') || (val.contains('-') && val.contains(':')) {
        let iso = if is_utc {
            format!("{}+00:00", &val[..val.len() - 1])
        } else {
            val.to_string()
        };
        if let Ok(dt) = DateTime::parse_from_rfc3339(&iso) {
            return Some(dt.timestamp());
        }
        for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f"] {
            if let Ok(naive) = NaiveDateTime::parse_from_str(&iso, fmt) {
                return Some(cst().from_local_datetime(&naive).single()?.timestamp());
            }
        }
    }
    let clean = {

        let mut s = String::new();
        let mut chars = val.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '.' {
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() { chars.next(); } else { break; }
                }
            } else {
                s.push(c);
            }
        }
        s
    };
    let digits: String = clean.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    if digits.len() == 10 {
        return digits.parse::<i64>().ok();
    }
    if digits.len() == 13 {
        return digits.parse::<i64>().ok().map(|v| v / 1000);
    }
    let tz = if is_utc { FixedOffset::east_opt(0).unwrap() } else { cst() };
    if digits.len() >= 14 {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&digits[..14], "%Y%m%d%H%M%S") {
            return Some(tz.from_local_datetime(&naive).single()?.timestamp());
        }
    }
    if digits.len() == 12 {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&digits, "%Y%m%d%H%M") {
            return Some(tz.from_local_datetime(&naive).single()?.timestamp());
        }
    }
    None
}

pub fn parse_catchup_query(query_str: &str) -> Option<(i64, i64)> {
    if query_str.is_empty() {
        return None;
    }
    let qs: HashMap<String, String> = url::form_urlencoded::parse(query_str.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let now = now_f64() as i64;
    let mut start_ts: Option<i64> = None;
    let mut end_ts: Option<i64> = None;
    if let Some(ps) = qs.get("playseek") {
        if let Some((a, b)) = ps.split_once('-') {
            start_ts = parse_catchup_time_val(Some(a));
            end_ts = parse_catchup_time_val(Some(b));
        } else {
            start_ts = parse_catchup_time_val(Some(ps));
        }
    }
    for k in ["start", "utc", "starttime", "end", "lutc", "endtime"] {
        let target = if matches!(k, "end" | "lutc" | "endtime") { &mut end_ts } else { &mut start_ts };
        if target.is_none() {
            *target = parse_catchup_time_val(qs.get(k).map(|s| s.as_str()));
        }
    }
    let start = start_ts?;
    let end = match end_ts {
        Some(e) if e > start => e,
        _ => (start + 60).max((start + 7200).min(now)),
    };
    Some((start, end))
}

type Segment = (f64, String, String);

pub fn seg_key(url: &str, pdt: &str) -> String {
    if !pdt.is_empty() {
        return format!("pdt:{pdt}");
    }
    Url::parse(url)
        .map(|p| format!("{}://{}{}", p.scheme(), p.host_str().unwrap_or(""), p.path()))
        .unwrap_or_else(|_| url.to_string())
}

pub fn defn_label(defn: &str) -> String {
    match defn.trim().to_lowercase().as_str() {
        "fhd" => "1080p".to_string(),
        "shd" => "720p".to_string(),
        _ => if defn.is_empty() { "1080p".to_string() } else { defn.to_string() },
    }
}

async fn jce_fetch(ch: &SharedChannel) -> Result<Vec<Segment>> {
    ch_app_flow_limiter().await;
    let (pid, sid, defn) = {
        let c = ch.lock();
        (c.pid.clone(), c.sid.clone(), c.defn.clone())
    };
    let now = now_f64() as i64;
    let m3u8_url = crate::jce::jce_timeshift_url(client(), &pid, &sid, now - WINDOW, now, &defn).await?;
    let headers = [("User-Agent", UA)];
    let resp = client()
        .send_timeout(reqwest::Method::GET, &m3u8_url, &headers, None, Duration::from_secs(20))
        .await?;
    let text = resp.text();
    let base = Url::parse(&m3u8_url).ok();
    let mut segs = Vec::new();
    let mut dur = 6.0f64;
    let mut pdt = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            dur = rest.split(',').next().unwrap_or("").parse::<f64>().unwrap_or(6.0);
        } else if let Some(v) = line.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            pdt = v.to_string();
        } else if !line.is_empty() && !line.starts_with('#') {
            let url = base
                .as_ref()
                .and_then(|b| b.join(line).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| line.to_string());
            segs.push((dur, pdt.clone(), url));
            pdt.clear();
        }
    }
    if segs.is_empty() {
        return Err(YsptpError::new("empty playlist"));
    }
    Ok(segs)
}

async fn ch_app_flow_limiter() {

    static L: OnceLock<ApiFlowLimiter> = OnceLock::new();
    L.get_or_init(|| ApiFlowLimiter::new(700, 250)).wait_slot().await;
}

async fn jce_refresh(ch: &SharedChannel) -> Result<bool> {
    let segs = match jce_fetch(ch).await {
        Ok(v) => v,
        Err(e) if e.is_dead_host() => return Err(e),
        Err(e) => return Err(e),
    };
    let is_init;
    let mut added = 0i64;
    let info;
    {
        let mut c = ch.lock();
        is_init = c.order.is_empty();
        if is_init {
            for (_, pdt, url) in &segs {
                let k = seg_key(url, pdt);
                if !c.seen_keys.contains(&k) {
                    if c.seen_keys.len() >= MAX_SEGS * 2 { c.seen_keys.pop_front(); }
                    c.seen_keys.push_back(k);
                }
            }
            let start = segs.len().saturating_sub(25);
            for (dur, pdt, url) in &segs[start..] {
                let key = seg_key(url, pdt);
                c.seq += 1;
                let seq = c.seq;
                c.segments.insert(key.clone(), (seq, *dur, pdt.clone(), url.clone()));
                c.order.push_back(key);
                added += 1;
                if !pdt.is_empty() { c.last_pdt = pdt.clone(); }
            }
        } else {
            for (dur, pdt, url) in &segs {
                if !pdt.is_empty() && !c.last_pdt.is_empty() && pdt.as_str() <= c.last_pdt.as_str() {
                    continue;
                }
                let key = seg_key(url, pdt);
                if let Some(seg) = c.segments.get_mut(&key) {
                    seg.3 = url.clone();
                    continue;
                }
                if c.seen_keys.contains(&key) {
                    continue;
                }
                if c.seen_keys.len() >= MAX_SEGS * 2 { c.seen_keys.pop_front(); }
                c.seen_keys.push_back(key.clone());
                c.seq += 1;
                let seq = c.seq;
                c.segments.insert(key.clone(), (seq, *dur, pdt.clone(), url.clone()));
                c.order.push_back(key);
                if !pdt.is_empty() { c.last_pdt = pdt.clone(); }
                added += 1;
            }
        }
        while c.order.len() > MAX_SEGS {
            if let Some(old) = c.order.pop_front() {
                c.segments.remove(&old);
            }
        }
        c.last_error.clear();
        info = (c.slug.clone(), defn_label(&c.defn), c.segments.len());
    }
    if is_init && added > 0 {
        log(format!("[{}] {} 备用流缓冲就绪 ({} 片)", info.0, info.1, info.2), 0.0);
    }
    Ok(true)
}

async fn bk_refresh(ch: &SharedChannel) -> Result<bool> {
    ch_app_flow_limiter().await;
    let now = now_f64();
    let (sid, pid, defn, urls, urls_time) = {
        let c = ch.lock();
        (c.sid.clone(), c.pid.clone(), c.defn.clone(), c.bk_urls.clone(), c.bk_urls_time)
    };
    let mut urls = urls;
    if now - urls_time > BK_URL_TTL || urls.is_empty() {
        urls = crate::jce::bk_playurls(client(), &sid, &pid, &defn).await?;
        {
            let mut c = ch.lock();
            c.bk_urls = urls.clone();
            c.bk_urls_time = now_f64();
        }
        log(format!("[{}] bkliveinfo 备用通道就绪 ({} 地址)", ch.lock().slug, urls.len()), 60.0);
    }
    let mut last_err = String::new();
    for attempt in 0..2 {
        for u in &urls {
            match crate::jce::fetch_abs_playlist(client(), u, 0).await {
                Ok(pl) if pl.contains("#EXTM3U") => {
                    let mut c = ch.lock();
                    c.bk_playlist = pl;
                    c.last_error.clear();
                    return Ok(true);
                }
                Ok(_) => continue,
                Err(e) => {
                    let msg = e.message();
                    last_err = if msg.contains("HTTP ") {
                        format!("HTTPError: {}", msg)
                    } else {
                        msg.to_string()
                    };
                }
            }
        }
        if attempt == 0 {
            if let Ok(new_urls) = crate::jce::bk_playurls(client(), &sid, &pid, &defn).await {
                urls = new_urls;
                ch.lock().bk_urls_time = now_f64();
                log(format!("[{}] {} 地址已过期，已重新拉取", ch.lock().slug, defn_label(&defn)), 10.0);
            }
        }
    }
    {
        let mut c = ch.lock();
        c.bk_urls.clear();
        c.bk_urls_time = 0.0;
    }
    Err(YsptpError::new(if last_err.is_empty() { "bk playlist failed".to_string() } else { last_err[..last_err.len().min(120)].to_string() }))
}

pub async fn refresh_once(ch: &SharedChannel) -> bool {
    let mode = { ch.lock().mode.clone() };
    let result = if mode == "bk" {
        bk_refresh(ch).await
    } else {
        match jce_refresh(ch).await {
            Err(e) if e.is_dead_host() => {
                {
                    let mut c = ch.lock();
                    c.mode = "bk".to_string();
                }
                log(format!("[{}] JCE 域名受限，自动切换 bkliveinfo 备用通道", ch.lock().slug), 10.0);
                bk_refresh(ch).await
            }
            other => other,
        }
    };
    match result {
        Ok(true) => {
            ch.lock().last_ok = now_f64();
            true
        }
        _ => {
            let e = result.err().map(|e| e.message().to_string()).unwrap_or_default();
            {
                let mut c = ch.lock();
                c.last_error = e.chars().take(120).collect();
            }
            log(format!("[{}] {} 刷新异常: {}", ch.lock().slug, defn_label(&ch.lock().defn), ch.lock().last_error), 15.0);
            false
        }
    }
}

pub fn build_playlist(ch: &SharedChannel, holdback: bool) -> Option<String> {
    let mut c = ch.lock();
    if c.mode == "bk" {
        if !c.bk_playlist.is_empty() {
            c.last_built_playlist = c.bk_playlist.clone();
            c.last_built_time = now_f64();
            return Some(c.bk_playlist.clone());
        }
        return None;
    }
    let keys: Vec<String> = c.order.iter().rev().take(15).rev().cloned().collect();
    let segs: Vec<(i64, f64, String, String)> =
        keys.iter().filter_map(|k| c.segments.get(k).cloned()).collect();
    if segs.is_empty() {
        return None;
    }
    let display: &[(i64, f64, String, String)] = if holdback && segs.len() >= 4 {
        &segs[..segs.len() - 1]
    } else {
        &segs
    };
    let target = display.iter().map(|s| rust_round(s.1) as i64).max().unwrap_or(6).max(6);
    let mut out = vec![
        "#EXTM3U".to_string(),
        "#EXT-X-VERSION:3".to_string(),
        format!("#EXT-X-TARGETDURATION:{target}"),
        format!("#EXT-X-MEDIA-SEQUENCE:{}", display[0].0),
    ];
    for (_, dur, pdt, url) in display {
        if !pdt.is_empty() {
            out.push(format!("#EXT-X-PROGRAM-DATE-TIME:{pdt}"));
        }
        out.push(format!("#EXTINF:{dur:.3},"));
        out.push(url.clone());
    }
    let res = format!("{}\n", out.join("\n"));
    c.last_built_playlist = res.clone();
    c.last_built_time = now_f64();
    Some(res)
}

pub async fn coalesce_refresh<Fut: Future<Output = ()>>(
    app: &Arc<AppState>,
    slug: &str,
    fut: Fut,
    timeout_secs: f64,
) {
    let (ev, wait) = {
        let mut m = app.inflight_events.lock();
        if let Some(e) = m.get(slug) {
            (e.clone(), true)
        } else {
            let e = Arc::new(tokio::sync::Notify::new());
            m.insert(slug.to_string(), e.clone());
            (e, false)
        }
    };
    if wait {
        let _ = tokio::time::timeout(Duration::from_secs_f64(timeout_secs), ev.notified()).await;
        return;
    }
    fut.await;
    app.inflight_events.lock().remove(slug);
    ev.notify_waiters();
}

pub async fn ensure_channel(app: &Arc<AppState>, ch: &SharedChannel) {
    let now = now_f64();
    let (need_fetch, thread_dead, wait_only, slug) = {
        let mut c = ch.lock();
        c.last_access = now;
        if c.starting {
            (false, false, true, c.slug.clone())
        } else {
            let thread_dead = !c.thread_alive;
            let cache_stale = now - c.last_ok > IDLE_TIMEOUT
                || (c.segments.is_empty() && c.bk_playlist.is_empty());
            if cache_stale {
                c.segments.clear();
                c.order.clear();
                c.seen_keys.clear();
                c.last_pdt.clear();
                c.bk_playlist.clear();
            }
            if cache_stale || thread_dead {
                c.starting = true;
            } else {
                return;
            }
            (cache_stale, thread_dead, false, c.slug.clone())
        }
    };
    if wait_only {
        let ch2 = ch.clone();
        coalesce_refresh(app, &slug, async move { let _ = refresh_once(&ch2).await; }, 5.0).await;
        ch.lock().starting = false;
        return;
    }
    if need_fetch {
        let ch2 = ch.clone();
        coalesce_refresh(app, &slug, async move { let _ = refresh_once(&ch2).await; }, 5.0).await;
    }
    if thread_dead {
        ch.lock().thread_alive = true;
        let app2 = app.clone();
        let ch3 = ch.clone();
        tokio::spawn(async move {
            refresh_loop(&app2, &ch3).await;
            ch3.lock().thread_alive = false;
        });
    }
    ch.lock().starting = false;
}

async fn refresh_loop(_app: &Arc<AppState>, ch: &SharedChannel) {
    let (slug, mode, defn) = {
        let c = ch.lock();
        (c.slug.clone(), c.mode.clone(), c.defn.clone())
    };
    log(format!("[{slug}] {} 后台拉取启动 [{mode}]", defn_label(&defn)), 10.0);
    let mut fails = 0i64;
    if now_f64() - ch.lock().last_ok < REFRESH_INTERVAL {
        build_playlist(ch, true);
        tokio::time::sleep(Duration::from_secs_f64(REFRESH_INTERVAL)).await;
    }
    while now_f64() - ch.lock().last_access < IDLE_TIMEOUT {
        let ok = refresh_once(ch).await;
        if ok {
            build_playlist(ch, true);
        }
        fails = if ok { 0 } else { fails + 1 };
        let interval = if fails < 3 { REFRESH_INTERVAL } else { 30.0 };
        tokio::time::sleep(Duration::from_secs_f64(interval)).await;
    }
    log(format!("[{slug}] 空闲超时，已停止后台拉取"), 10.0);
}

pub fn trigger_background_refresh(app: &Arc<AppState>, ch: &SharedChannel) {
    let slug = { ch.lock().slug.clone() };
    if app.inflight_events.lock().contains_key(&slug) {
        return;
    }
    let app2 = app.clone();
    let ch2 = ch.clone();
    let ch3 = ch.clone();
    tokio::spawn(async move {
        coalesce_refresh(&app2, &slug, async move {
            if refresh_once(&ch2).await {
                build_playlist(&ch3, true);
            }
        }, 6.0).await;
    });
}

pub fn build_all_m3u(app: &AppState, host: &str, proto: &str, group_filter: Option<&str>, only_4k: bool) -> String {
    let mut lines = vec![
        "#EXTM3U url-tvg=\"https://epg.zsdc.eu.org/t.xml.gz,https://epg.pw/xmltv/epg_CN.xml.gz\" x-tvg-url=\"https://epg.zsdc.eu.org/t.xml.gz\"".to_string(),
        "#EXT-X-APTV-PREVIEW: FALSE".to_string(),
        "#EXT-X-APTV-LATENCY: FALSE".to_string(),
        "#EXT-X-APTV-LOGO: FALSE".to_string(),
    ];
    for meta in app.directory.unique_channels() {
        if let Some(g) = group_filter {
            if !meta.group.contains(g) {
                continue;
            }
        }
        if only_4k && !crate::channels::is_true_4k(&meta.slug) {
            continue;
        }
        let mut attrs = format!(
            " tvg-id=\"{}\" tvg-name=\"{}\" tvg-logo=\"{}\" group-title=\"{}\"",
            meta.tvg_id, meta.name, meta.logo, meta.group
        );
        if crate::channels::supports_timeshift(&meta.slug) {
            attrs.push_str(" catchup=\"append\" catchup-days=\"7\" catchup-source=\"?playseek=${(b)yyyyMMddHHmmss}-${(e)yyyyMMddHHmmss}\"");
        }
        lines.push(format!("#EXTINF:-1{attrs},{}", meta.name));
        lines.push(format!("{proto}://{host}/{}.m3u8", meta.slug));
    }
    format!("{}\n", lines.join("\n"))
}

pub fn quote_all(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn is_safe_engine_proxy_url(app: &AppState, target: &str, depth: u8) -> bool {
    if depth > 2 {
        return false;
    }
    let Ok(parsed) = Url::parse(target) else { return false };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return false;
    }
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "127.0.0.1" || host == "localhost" {
        if parsed.port() != Some(app.engine_port) || !parsed.path().starts_with("/segment") {
            return false;
        }
        let qs: HashMap<String, String> = url::form_urlencoded::parse(parsed.query().unwrap_or("").as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        return qs
            .get("url")
            .map(|inner| is_safe_engine_proxy_url(app, inner, depth + 1))
            .unwrap_or(false);
    }
    app.engine_proxy_suffixes
        .iter()
        .any(|s| host == s[1..] || host.ends_with(s))
}

pub async fn fetch_upstream_engine_playlist(
    app: &AppState,
    canonical: &str,
    host: &str,
    proto: &str,
) -> Option<String> {
    let scheme = if proto == "https" { "https" } else { "http" };
    for u in [
        format!("http://127.0.0.1:{}/live/{canonical}.m3u8", app.engine_port),
        format!("http://127.0.0.1:{}/{canonical}.m3u8", app.engine_port),
    ] {
        let headers = [("User-Agent", UA)];
        let Ok(resp) = client()
            .send_timeout(reqwest::Method::GET, &u, &headers, None, Duration::from_secs(10))
            .await
        else { continue };
        if resp.status != 200 {
            continue;
        }
        let text = resp.text();
        if !text.contains("#EXTM3U") {
            continue;
        }
        let base_prefix = format!("{scheme}://{host}/engine_proxy?url=");
        let base_url = Url::parse(&u).ok();
        let mut out_lines = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('#') {
                out_lines.push(line.to_string());
            } else {
                let full = if line.starts_with("http://") || line.starts_with("https://") {
                    line.to_string()
                } else {
                    base_url
                        .as_ref()
                        .and_then(|b| b.join(line).ok())
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| line.to_string())
                };
                out_lines.push(format!("{base_prefix}{}", quote_all(&full)));
            }
        }
        return Some(format!("{}\n", out_lines.join("\n")));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_playlist_absolute_urls() {
        let pl = "#EXTM3U\n#EXTINF:6.0,\nseg1.ts\n";
        let out = rewrite_playlist_urls(pl, "https://cdn.cctv.cn/live/a/playlist.m3u8");
        assert!(out.contains("https://cdn.cctv.cn/live/a/seg1.ts"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn proxy_ts_chain_roundtrip() {
        let pl = "#EXTM3U\nhttps://cdn.cctv.cn/live/seg1.ts\n";
        let out = proxy_playlist_ts_urls(pl, "http://127.0.0.1:8767", None);
        let line = out.lines().nth(1).unwrap();
        let token = line.split("/proxy.ts?=").nth(1).unwrap();
        assert_eq!(decode_proxy_ts_url(token).unwrap(), "https://cdn.cctv.cn/live/seg1.ts");
    }

    #[test]
    fn discontinuity_injected_once() {
        let pl = "#EXTM3U\n#EXTINF:6.0,\na.ts\n";
        let out = inject_hls_discontinuity(pl);
        assert_eq!(out.matches("#EXT-X-DISCONTINUITY").count(), 1);
        assert_eq!(inject_hls_discontinuity(&out).matches("#EXT-X-DISCONTINUITY").count(), 1);
    }

    #[test]
    fn catchup_time_formats() {
        assert_eq!(parse_catchup_time_val(Some("1700000000")), Some(1700000000));
        assert_eq!(parse_catchup_time_val(Some("1700000000000")), Some(1700000000));
        assert!(parse_catchup_time_val(Some("20240101120000")).is_some());
        assert!(parse_catchup_time_val(Some("bogus")).is_none());
    }

    #[test]
    fn catchup_query_defaults() {
        let (s, e) = parse_catchup_query("playseek=20240101120000-20240101130000").unwrap();
        assert!(e > s);
    }

    #[test]
    fn quote_all_encodes_slash() {
        assert_eq!(quote_all("https://x.cn/a?b=1"), "https%3A%2F%2Fx.cn%2Fa%3Fb%3D1");
    }

    #[test]
    fn seg_key_prefers_pdt() {
        assert_eq!(seg_key("https://x.cn/a.ts?q=1", "2024-01-01T00:00:00Z"), "pdt:2024-01-01T00:00:00Z");
        assert_eq!(seg_key("https://x.cn/a.ts?q=1", ""), "https://x.cn/a.ts");
    }
}
