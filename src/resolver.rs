//! 对应 Python `class Resolver` 的状态机部分：会话池、换台防抖、配额、冷却、
//! 400 换设备、后台刷新队列、SWR、playlist 缓存、TS 回放头匹配。
//! 设备协议的具体 HTTP 流程见 `protocol.rs`，HLS 文本处理见 `hls.rs`。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::error::{Result, YsptpError};
use crate::http_client::HttpClient;
use crate::identity::{cctv_playback_headers_for_entry, default_cctv_playback_headers};
use crate::state::{
    AppState, ChannelEntry, EngineArgs, PlaylistCacheEntry, PlaylistCacheProbe,
    ResolverState, SharedSession,
};
use crate::util::{log, now_f64, rust_round};

pub struct Resolver {
    pub args: EngineArgs,
    pub generic_client: HttpClient,
    pub state: Arc<Mutex<ResolverState>>,
    pub control: Arc<AsyncMutex<()>>,
    pub app: Arc<AppState>,
    refresh_tx: mpsc::UnboundedSender<String>,
    refresh_rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

/// `control_request_slot` 的 RAII 守卫：离开作用域时写入 last_control_request_at。
pub struct ControlGuard {
    state: Arc<Mutex<ResolverState>>,
}

impl Drop for ControlGuard {
    fn drop(&mut self) {
        self.state.lock().last_control_request_at = now_f64();
    }
}

impl Resolver {
    pub fn new(app: Arc<AppState>) -> Result<Arc<Self>> {
        let args = app.args.clone();
        let generic_client = HttpClient::new(args.timeout, args.insecure_tls)?;
        let (tx, rx) = mpsc::unbounded_channel();
        Ok(Arc::new(Self {
            args,
            generic_client,
            state: Arc::new(Mutex::new(ResolverState::default())),
            control: Arc::new(AsyncMutex::new(())),
            app,
            refresh_tx: tx,
            refresh_rx: Mutex::new(Some(rx)),
        }))
    }

    // ------------------------------------------------------------ 启动后台任务

    pub fn start_background(self: &Arc<Self>) {
        let r = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs_f64(r.args.heartbeat_interval.max(1.0))).await;
                let _ = crate::protocol::send_heartbeat(r.as_ref()).await;
            }
        });
        let r = self.clone();
        let rx = self.refresh_rx.lock().take();
        if let Some(mut rx) = rx {
            tokio::spawn(async move {
                while let Some(channel) = rx.recv().await {
                    let _ = r.refresh_channel_background(&channel).await;
                }
            });
        }
        let r = self.clone();
        tokio::spawn(async move { r.init_resolver().await });
    }

    /// 对应 `init_resolver()`：后台初始化设备协议并发池，失败按指数退避重试。
    async fn init_resolver(self: Arc<Self>) {
        log("[系统] 正在初始化设备协议并发池…", 0.0);
        let mut retry = 0i64;
        loop {
            match crate::protocol::ensure_slot_session(self.as_ref(), 0).await {
                Ok(_) => {
                    self.app.set_ready(true);
                    log(
                        format!(
                            "[系统] 设备身份就绪，高码率并发池已激活 (全部 {} 路按需解析)",
                            self.app.active_backend.len()
                        ),
                        0.0,
                    );
                    break;
                }
                Err(e) => {
                    retry += 1;
                    log(
                        format!("[系统] 设备协议初始化第 {retry} 次重试中: {e}，4K/高码率频道暂走 1080p 回退"),
                        10.0,
                    );
                    self.app.set_ready(false);
                    if e.message().contains("HTTP 400") {
                        self.rotate_device_on_400(&format!("init:{}", &e.message()[..e.message().len().min(60)]));
                    }
                    let backoff = (15.0 * 1.5f64.powi(retry.saturating_sub(1).min(4) as i32)).min(60.0);
                    tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
                }
            }
        }
    }

    // ------------------------------------------------------------ 控制面节流

    /// 对应 `control_request_slot(label)`：最小间隔 + 可选抖动。
    pub async fn control_slot(&self, _label: &str) -> ControlGuard {
        let min_interval = self.args.refresh_interval.max(0.0);
        loop {
            let wait = {
                let st = self.state.lock();
                let last = st.last_control_request_at;
                if last <= 0.0 { 0.0 } else { min_interval - (now_f64() - last) }
            };
            if wait <= 0.0 {
                break;
            }
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        }
        if let Some(d) = self.control_step_jitter_duration() {
            tokio::time::sleep(Duration::from_secs_f64(d)).await;
        }
        ControlGuard { state: self.state.clone() }
    }

    fn control_step_jitter_duration(&self) -> Option<f64> {
        let min_ms = self.args.control_step_jitter_min_ms;
        let max_ms = self.args.control_step_jitter_max_ms;
        if max_ms == 0 {
            return None;
        }
        let upper = max_ms.max(min_ms);
        let span = upper - min_ms;
        let extra = if span > 0 { rand::random::<u64>() % (span as u64 + 1) } else { 0 };
        Some((min_ms + extra as i64) as f64 / 1000.0)
    }

    // ------------------------------------------------------------ 会话辅助

    pub fn session_fresh(&self, created_at: f64) -> bool {
        self.args.session_ttl <= 0.0 || now_f64() - created_at < self.args.session_ttl
    }

    pub fn session_ttl_remaining(&self, created_at: f64) -> Option<f64> {
        if self.args.session_ttl <= 0.0 {
            None
        } else {
            Some(self.args.session_ttl - (now_f64() - created_at))
        }
    }

    pub fn slot_device_json_path(&self, slot: i32) -> std::path::PathBuf {
        if slot == 0 {
            return self.args.device_json.clone();
        }
        let p = &self.args.device_json;
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("device");
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("json");
        p.with_file_name(format!("{stem}-slot{slot}.{ext}"))
    }

    /// 对应 `evict_idle_links`：释放空闲超过 idle_sec 的高码率配额。
    pub fn evict_idle_links(&self, sess: &SharedSession, idle_sec: f64) {
        let last = { self.state.lock().channel_last_access.clone() };
        let now = now_f64();
        let mut s = sess.lock();
        s.linked_channels
            .retain(|ch| now - last.get(ch).copied().unwrap_or(0.0) <= idle_sec);
    }

    // ------------------------------------------------------------ 换台防抖

    /// 对应 `zap_debounced(channel)`：非 4K 频道切台后先出 1080p，停留超过防抖秒数再升码率。
    pub fn zap_debounced(self: &Arc<Self>, channel: &str) -> bool {
        if crate::channels::is_true_4k(channel) {
            return false;
        }
        let debounce = self.args.zap_debounce;
        if debounce <= 0.0 {
            return false;
        }
        let now = now_f64();
        let mut spawn: Option<(i64, f64)> = None;
        let result = {
            let mut st = self.state.lock();
            if let Some(entry) = st.cache.get(channel) {
                if entry.fresh(now) && !crate::identity::entry_missing_signed_playback_headers(entry) {
                    return false;
                }
            }
            if let Some(sess) = st.sessions.get(&0) {
                let has_link = sess.lock().linked_channels.contains(channel);
                if has_link {
                    if let Some(entry) = st.cache.get(channel) {
                        if now - entry.expires_at < 30.0 {
                            return false;
                        }
                    }
                }
            }
            st.zap_channel = channel.to_string();
            if st.zap_pending_channels.contains(channel) {
                return true;
            }
            let ch_zap_time = *st.zap_times.get(channel).unwrap_or(&0.0);
            let ch_zap_time = if ch_zap_time <= 0.0 || now - ch_zap_time > (debounce * 3.0).max(30.0) {
                st.zap_times.insert(channel.to_string(), now);
                st.zap_time = now;
                now
            } else {
                ch_zap_time
            };
            if now - ch_zap_time < debounce {
                st.zap_pending = channel.to_string();
                st.zap_pending_channels.insert(channel.to_string());
                st.zap_generation += 1;
                let gen = st.zap_generations.get(channel).copied().unwrap_or(0) + 1;
                st.zap_generations.insert(channel.to_string(), gen);
                let delay = (debounce - (now - ch_zap_time)).max(0.1);
                spawn = Some((gen, delay));
                true
            } else {
                false
            }
        };
        if let Some((gen, delay)) = spawn {
            let r = self.clone();
            let ch = channel.to_string();
            tokio::spawn(async move { r.zap_resolve_worker(&ch, gen, delay).await });
        }
        result
    }

    /// 对应 `acquire_prefetch_preemption`：换台时中断其他频道的后台预取。
    fn acquire_prefetch_preemption(&self, channel: &str) -> Arc<AtomicBool> {
        let mut map = self.app.prefetch_events.lock();
        for (ch, flag) in map.iter() {
            if ch != channel {
                flag.store(true, Ordering::SeqCst);
                log(format!("[{ch}] 收到新换台插队请求，已抢占并中断其后台预取"), 2.0);
            }
        }
        let flag = Arc::new(AtomicBool::new(false));
        map.insert(channel.to_string(), flag.clone());
        flag
    }

    fn release_prefetch_preemption(&self, channel: &str) {
        self.app.prefetch_events.lock().remove(channel);
    }

    async fn zap_resolve_worker(self: Arc<Self>, channel: &str, gen: i64, delay: f64) {
        let cancel = self.acquire_prefetch_preemption(channel);
        let step = 0.2;
        let mut waited = 0.0;
        while waited < delay {
            if cancel.load(Ordering::SeqCst) {
                self.release_prefetch_preemption(channel);
                return;
            }
            tokio::time::sleep(Duration::from_secs_f64(step)).await;
            waited += step;
        }
        let now = now_f64();
        {
            let mut st = self.state.lock();
            if gen != st.zap_generations.get(channel).copied().unwrap_or(0) {
                st.zap_pending_channels.remove(channel);
                self.release_prefetch_preemption(channel);
                return;
            }
            let last_acc = st.channel_last_access.get(channel).copied().unwrap_or(0.0);
            if now - last_acc > (delay + 2.0).max(12.0)
                || (st.zap_channel != channel && now - last_acc > 5.0)
            {
                st.zap_pending_channels.remove(channel);
                st.zap_times.remove(channel);
                if st.zap_pending == channel {
                    st.zap_pending.clear();
                }
                self.release_prefetch_preemption(channel);
                return;
            }
        }
        if !cancel.load(Ordering::SeqCst) {
            match self.ensure_channel(channel, true).await {
                Ok(_) => log(format!("[{channel}] 高码率后台预取完成, 继续停留将自动升级"), 10.0),
                Err(e) => log(format!("[{channel}] 高码率后台预取失败: {e}, 继续 1080p"), 10.0),
            }
        }
        self.release_prefetch_preemption(channel);
        let mut st = self.state.lock();
        st.zap_pending_channels.remove(channel);
        if st.zap_pending == channel {
            st.zap_pending.clear();
        }
    }

    // ------------------------------------------------------------ 400 换设备

    /// 对应 `rotate_device_on_400`：5 分钟内一次、每天最多 5 次。
    pub fn rotate_device_on_400(&self, reason: &str) -> bool {
        let now = now_f64();
        let (wipe_no, target) = {
            let mut st = self.state.lock();
            if now - st.last_device_wipe_at < 300.0 {
                return false;
            }
            let day = chrono::Local::now().format("%Y-%m-%d").to_string();
            if st.device_wipe_day != day {
                st.device_wipe_day = day.clone();
                st.device_wipe_count = 0;
            }
            if st.device_wipe_count >= 5 {
                log("[设备] 24小时内已自动换新身份 5 次，停止自动更换，请手动检查网络或设备", 60.0);
                return false;
            }
            st.last_device_wipe_at = now;
            st.device_wipe_day = day;
            st.device_wipe_count += 1;
            let n = st.device_wipe_count;
            st.sessions.remove(&0);
            st.cache.clear();
            st.playlist_cache.clear();
            st.channel_failures.clear();
            st.channel_last_access.clear();
            (n, self.slot_device_json_path(0))
        };
        crate::device::create_and_save_new_device(&target);
        log(format!(
            "[设备] 检测到 400 ({reason})，旧身份已废弃，换新设备身份 (5分钟内仅一次，今日第 {wipe_no} 次)"
        ), 0.0);
        true
    }

    // ------------------------------------------------------------ 错误 / 冷却

    pub fn record_recoverable_error(&self, reason: &str) -> bool {
        if !crate::util::is_identity_recoverable_error(reason) {
            return false;
        }
        let threshold = self.args.identity_reset_error_threshold;
        if threshold == 0 {
            return false;
        }
        {
            let mut st = self.state.lock();
            if st.last_identity_reset_at > 0.0
                && now_f64() - st.last_identity_reset_at < self.args.identity_reset_cooldown
            {
                return false;
            }
            st.identity_reset_error_count = (st.identity_reset_error_count + 1).min(i32::MAX as i64);
        }
        let _ = crate::meta::write_meta(self);
        false
    }

    pub fn clear_recoverable_error_count(&self) {
        self.state.lock().identity_reset_error_count = 0;
    }

    pub fn cooldown_seconds(fail_count: i32) -> f64 {
        match fail_count {
            0 | 1 => 15.0,
            2 => 60.0,
            _ => 120.0,
        }
    }

    pub fn is_channel_in_cooldown(&self, channel: &str) -> bool {
        let st = self.state.lock();
        match st.channel_failures.get(channel) {
            None => false,
            Some((failed_at, _, cnt)) => {
                now_f64() - failed_at < Self::cooldown_seconds(*cnt)
            }
        }
    }

    pub fn get_channel_failure_info(&self, channel: &str) -> Option<(f64, String, i32)> {
        self.state.lock().channel_failures.get(channel).cloned()
    }

    pub fn clear_channel_cooldown(&self, channel: &str) {
        self.state.lock().channel_failures.remove(channel);
    }

    // ------------------------------------------------------------ 后台刷新队列

    /// 对应 `enqueue_background_refresh`，返回 (status, reason)。
    pub fn enqueue_background_refresh(&self, channel: &str) -> (String, String) {
        let now = now_f64();
        let mut st = self.state.lock();
        let q = &mut st.refresh_queue;
        if q.running_channel == channel || q.queued_channels.contains_key(channel) {
            return ("already_active".to_string(), String::new());
        }
        let limit = self.args.background_refresh_queue_limit;
        if limit == 0 || q.queued.len() >= limit {
            q.dropped_total += 1;
            q.last_dropped_at = now;
            q.last_drop_reason = if limit == 0 {
                format!("background refresh queue disabled; channel={channel}")
            } else {
                format!("background refresh queue full ({}/{limit}); channel={channel}", q.queued.len())
            };
            return ("rejected".to_string(), q.last_drop_reason.clone());
        }
        q.queued.push_back(channel.to_string());
        q.queued_channels.insert(channel.to_string(), now);
        q.enqueued_total += 1;
        q.last_enqueued_at = now;
        drop(st);
        let _ = self.refresh_tx.send(channel.to_string());
        ("enqueued".to_string(), String::new())
    }

    /// 对应 `refresh_channel_background`。
    pub async fn refresh_channel_background(&self, channel: &str) -> bool {
        let ok = {
            let _guard = self.control.lock().await;
            let now = now_f64();
            let skip = {
                let st = self.state.lock();
                st.cache.get(channel).map(|e| {
                    e.fresh(now)
                        && e.expires_at - now > 180.0
                        && !crate::identity::entry_missing_signed_playback_headers(e)
                }).unwrap_or(false)
            };
            if skip {
                true
            } else {
                match crate::protocol::refresh_channel_controlled(self, channel).await {
                    Ok(_) => {
                        let _ = crate::meta::write_meta(self);
                        true
                    }
                    Err(e) => {
                        let text = e.message().to_string();
                        {
                            let mut st = self.state.lock();
                            st.last_error = text.clone();
                            st.last_error_at = now_f64();
                            let cnt = st.channel_failures.get(channel).map(|x| x.2 + 1).unwrap_or(1);
                            let at = st.last_error_at;
                            st.channel_failures.insert(channel.to_string(), (at, text.clone(), cnt));
                            if let Some(entry) = st.cache.get_mut(channel) {
                                entry.last_refresh_error = text.clone();
                                entry.last_refresh_failed_at = at;
                            }
                            if crate::util::is_session_invalidating_error(&text) {
                                st.sessions.remove(&0);
                            }
                        }
                        self.record_recoverable_error(&text);
                        false
                    }
                }
            }
        };
        let mut st = self.state.lock();
        st.background_refreshing.remove(channel);
        st.refresh_queue.running_channel.clear();
        st.refresh_queue.running_started_at = 0.0;
        st.refresh_queue.last_finished_at = now_f64();
        if ok { st.refresh_queue.completed_total += 1 } else { st.refresh_queue.failed_total += 1 }
        ok
    }

    // ------------------------------------------------------------ 主入口 ensure_channel

    /// 对应 `Resolver.ensure_channel(channel, force=False)`（SWR + 冷却 + 400 自愈）。
    pub async fn ensure_channel(&self, channel: &str, force: bool) -> Result<ChannelEntry> {
        let now = now_f64();
        if !force {
            let mut st = self.state.lock();
            if let Some(entry) = st.cache.get(channel).cloned() {
                if entry.fresh(now) && !crate::identity::entry_missing_signed_playback_headers(&entry) {
                    return Ok(entry);
                }
                if entry.stale_usable(now, self.args.stale_while_refresh_ttl)
                    && !crate::identity::entry_missing_signed_playback_headers(&entry)
                {
                    let retry_at = entry.last_refresh_failed_at + self.args.refresh_error_cooldown;
                    let started = st.background_refreshing.get(channel).copied();
                    let should = now >= retry_at && started.map(|s| now - s > 300.0).unwrap_or(true);
                    if should {
                        st.background_refreshing.insert(channel.to_string(), now);
                        drop(st);
                        let (status, reason) = self.enqueue_background_refresh(channel);
                        if status == "rejected" {
                            let mut st2 = self.state.lock();
                            st2.background_refreshing.remove(channel);
                            if let Some(e2) = st2.cache.get_mut(channel) {
                                e2.last_refresh_error = reason;
                                e2.last_refresh_failed_at = now;
                            }
                        }
                        return Ok(entry);
                    }
                    return Ok(entry);
                }
            }
            if let Some((failed_at, err_msg, cnt)) = st.channel_failures.get(channel).cloned() {
                let cd = Self::cooldown_seconds(cnt);
                let elapsed = now - failed_at;
                if elapsed < cd {
                    return Err(YsptpError::new(format!(
                        "channel {channel} in cooldown ({}s/{}s): {err_msg}",
                        elapsed as i64, cd as i64
                    )));
                }
            }
        }

        let _guard = self.control.lock().await;
        if !force {
            let st = self.state.lock();
            if let Some(entry) = st.cache.get(channel).cloned() {
                if entry.fresh(now_f64()) && !crate::identity::entry_missing_signed_playback_headers(&entry) {
                    return Ok(entry);
                }
            }
        }

        match crate::protocol::refresh_channel_controlled(self, channel).await {
            Ok(entry) => {
                self.clear_recoverable_error_count();
                let _ = crate::meta::write_meta(self);
                Ok(entry)
            }
            Err(err) => {
                let err_text = err.message().to_string();
                if err_text.contains("HTTP 400")
                    && self.rotate_device_on_400(&format!("{}:{}", channel, &err_text[..err_text.len().min(60)]))
                {
                    if let Ok(entry) = crate::protocol::refresh_channel_controlled(self, channel).await {
                        self.clear_recoverable_error_count();
                        let _ = crate::meta::write_meta(self);
                        return Ok(entry);
                    }
                }
                let mut clone: Option<ChannelEntry> = None;
                {
                    let mut st = self.state.lock();
                    st.last_error = err_text.clone();
                    st.last_error_at = now_f64();
                    let cnt = st.channel_failures.get(channel).map(|x| x.2 + 1).unwrap_or(1);
                    let at = st.last_error_at;
                    let swr = self.args.stale_while_refresh_ttl;
                    st.channel_failures.insert(
                        channel.to_string(),
                        (at, err_text.clone(), cnt),
                    );
                    if let Some(entry) = st.cache.get_mut(channel) {
                        if !entry.final_url.is_empty()
                            && entry.stale_usable(at, swr)
                        {
                            entry.last_refresh_error = err_text.clone();
                            entry.last_refresh_failed_at = at;
                            clone = Some(entry.clone());
                        }
                    }
                }
                let did_reset = self.record_recoverable_error(&err_text);
                if !did_reset {
                    let _ = crate::meta::write_meta(self);
                }
                if did_reset || clone.is_none() {
                    return Err(err);
                }
                Ok(clone.unwrap())
            }
        }
    }

    // ------------------------------------------------------------ playlist 缓存

    /// 对应 `inspect_playlist_cache`。
    pub fn inspect_playlist_cache(
        cached: Option<&PlaylistCacheEntry>,
        entry: &ChannelEntry,
        proxy_origin: &str,
        proxy_prefix: Option<&str>,
        now: f64,
    ) -> PlaylistCacheProbe {
        let Some(c) = cached else {
            return PlaylistCacheProbe { state: "miss:not_found".to_string(), age_ms: None };
        };
        let age_ms = Some(rust_round((now - c.cached_at).max(0.0) * 1000.0) as i64);
        let fresh = now < c.expires_at;
        let same_final_url = c.final_url == entry.final_url;
        let same_android_id = c.android_id == entry.android_id;
        let same_origin = c.proxy_origin == proxy_origin;
        let same_prefix = c.proxy_prefix == proxy_prefix.unwrap_or("");
        if fresh && same_final_url && same_android_id && same_origin && same_prefix {
            return PlaylistCacheProbe { state: "hit".to_string(), age_ms };
        }
        let mut reasons = Vec::new();
        if !fresh { reasons.push("expired"); }
        if !same_final_url { reasons.push("final_url_changed"); }
        if !same_android_id { reasons.push("android_id_changed"); }
        if !same_origin { reasons.push("origin_changed"); }
        if !same_prefix { reasons.push("prefix_changed"); }
        if reasons.is_empty() { reasons.push("unknown"); }
        PlaylistCacheProbe { state: format!("miss:{}", reasons.join("+")), age_ms }
    }

    /// 对应 `playback_headers_for_ts_url`：按 host / 路径前缀找已缓存的签名回放头。
    pub fn playback_headers_for_ts_url(&self, real_url: &str) -> Result<HashMap<String, String>> {
        let parsed = url::Url::parse(real_url)
            .map_err(|e| YsptpError::new(format!("invalid proxy ts URL: {e}")))?;
        let host = parsed.host_str().unwrap_or("").to_string();
        if host.is_empty() {
            return Err(YsptpError::new("proxy ts URL missing host"));
        }
        let ts_path = parsed.path().to_string();
        let mut same_host: Option<HashMap<String, String>> = None;
        {
            let st = self.state.lock();
            for entry in st.cache.values() {
                if crate::identity::entry_missing_signed_playback_headers(entry) {
                    continue;
                }
                let entry_host = if entry.final_host.is_empty() {
                    crate::util::url_host(&entry.final_url)
                } else {
                    entry.final_host.clone()
                };
                if !entry_host.eq_ignore_ascii_case(&host) {
                    continue;
                }
                let headers = cctv_playback_headers_for_entry(entry);
                if crate::identity::ts_path_matches_final_url(&ts_path, &entry.final_url) {
                    return Ok(headers);
                }
                if same_host.is_none() {
                    same_host = Some(headers);
                }
            }
        }
        if let Some(h) = same_host {
            return Ok(h);
        }
        if crate::jce::host_needs_signed_headers(&host) {
            return Err(YsptpError::new(format!("missing cached signed playback headers for {host}")));
        }
        Ok(default_cctv_playback_headers(&self.current_cctv_android_id()))
    }

    /// 对应 `current_cctv_android_id(resolver)`。
    pub fn current_cctv_android_id(&self) -> String {
        let (loaded, _) = crate::device::load_device_state(&self.args.device_json);
        if !loaded.profile.android_id.is_empty() {
            return loaded.profile.android_id;
        }
        let st = self.state.lock();
        if let Some(sess) = st.sessions.get(&0) {
            let s = sess.lock();
            if !s.profile.android_id.is_empty() {
                return s.profile.android_id.clone();
            }
        }
        for e in st.cache.values() {
            if !e.android_id.is_empty() {
                return e.android_id.clone();
            }
        }
        String::new()
    }

    /// 对应 `proxy_playlist_ts_urls`：把 playlist 里的 .ts 行改成 /proxy.ts?base64 链。
    pub fn proxy_playlist_ts_urls(
        playlist: &str,
        proxy_origin: &str,
        proxy_prefix: Option<&str>,
    ) -> String {
        crate::hls::proxy_playlist_ts_urls(playlist, proxy_origin, proxy_prefix)
    }

    /// 对应 `fetch_playlist`：playlist 缓存命中/失效探测 + 单飞锁 + SWR 回退。
    pub async fn fetch_playlist(
        &self,
        entry: &ChannelEntry,
        proxy_origin: &str,
        proxy_prefix: Option<&str>,
    ) -> Result<(String, String, PlaylistCacheProbe)> {
        let now = now_f64();
        {
            let st = self.state.lock();
            let probe = Self::inspect_playlist_cache(
                st.playlist_cache.get(&entry.channel), entry, proxy_origin, proxy_prefix, now,
            );
            if probe.state == "hit" {
                let c = &st.playlist_cache[&entry.channel];
                return Ok((c.body.clone(), c.content_type.clone(), probe));
            }
        }
        let lock = {
            let mut st = self.state.lock();
            st.playlist_locks
                .entry(entry.channel.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _g = lock.lock().await;
        let miss_probe = {
            let st = self.state.lock();
            let probe = Self::inspect_playlist_cache(
                st.playlist_cache.get(&entry.channel), entry, proxy_origin, proxy_prefix, now_f64(),
            );
            if probe.state == "hit" {
                let c = &st.playlist_cache[&entry.channel];
                return Ok((c.body.clone(), c.content_type.clone(), probe));
            }
            probe
        };
        let playback_headers = cctv_playback_headers_for_entry(entry);
        let (status, content_type, text) =
            match crate::hls::fetch_playlist_http1(&self.generic_client, &entry.final_url, &playback_headers).await {
                Ok(v) => v,
                Err(err) => {
                    self.record_recoverable_error(err.message());
                    let st = self.state.lock();
                    if let Some(c) = st.playlist_cache.get(&entry.channel) {
                        if now_f64() - c.cached_at < 20.0 {
                            return Ok((c.body.clone(), c.content_type.clone(), miss_probe));
                        }
                    }
                    return Err(err);
                }
            };
        if !(200..300).contains(&status) {
            let err = YsptpError::new(format!("upstream m3u8 HTTP {status}: {}", &text[..text.len().min(300)]));
            self.record_recoverable_error(err.message());
            let st = self.state.lock();
            if let Some(c) = st.playlist_cache.get(&entry.channel) {
                if now_f64() - c.cached_at < 20.0 {
                    return Ok((c.body.clone(), c.content_type.clone(), miss_probe));
                }
            }
            return Err(err);
        }
        self.clear_recoverable_error_count();
        let mut body = crate::hls::rewrite_playlist_urls(&text, &entry.final_url);
        body = Self::proxy_playlist_ts_urls(&body, proxy_origin, proxy_prefix);
        if self.args.playlist_cache_ttl > 0.0 {
            let now2 = now_f64();
            let c = PlaylistCacheEntry {
                body: body.clone(),
                content_type: content_type.clone(),
                cached_at: now2,
                expires_at: now2 + self.args.playlist_cache_ttl,
                final_url: entry.final_url.clone(),
                android_id: entry.android_id.clone(),
                proxy_origin: proxy_origin.to_string(),
                proxy_prefix: proxy_prefix.unwrap_or("").to_string(),
            };
            self.state.lock().playlist_cache.insert(entry.channel.clone(), c);
        }
        Ok((body, content_type, miss_probe))
    }

    /// 对应 `fetch_channel_playlist`：换台防抖 + 地址过期极速重取。
    pub async fn fetch_channel_playlist(&self, channel: &str, host_header: &str, proto: &str) -> Result<Option<String>> {
        {
            let mut st = self.state.lock();
            st.channel_last_access.insert(channel.to_string(), now_f64());
        }
        // zap_debounced 需要 Arc<Self>，这里用已注册的全局 resolver
        let r = self.app.resolver.read().clone();
        if let Some(r) = r {
            if r.zap_debounced(channel) {
                log(format!("[{channel}] 换台防抖: 先出 1080p, 停留后自动升码率"), 10.0);
                return Ok(None);
            }
        }
        let host = if host_header.trim().is_empty() {
            format!("localhost:{}", self.args.port)
        } else {
            host_header.trim().to_string()
        };
        let scheme = if proto == "https" { "https" } else { "http" };
        let proxy_origin = format!("{scheme}://{host}");
        let mut entry = self.ensure_channel(channel, false).await?;
        for attempt in 0..2 {
            match self.fetch_playlist(&entry, &proxy_origin, None).await {
                Ok((body, _, _)) => {
                    self.clear_channel_cooldown(channel);
                    return Ok(Some(body));
                }
                Err(e) => {
                    let s = e.message().to_string();
                    let is_expiry = s.contains("HTTP 403") || s.contains("HTTP 404")
                        || s.contains("HTTP 401") || s.contains("upstream m3u8");
                    if is_expiry && attempt == 0 {
                        log(format!("[{channel}] 高码率播放地址已过期，正在极速重取新地址…"), 4.0);
                        {
                            let mut st = self.state.lock();
                            st.cache.remove(channel);
                            st.playlist_cache.remove(channel);
                        }
                        entry = self.ensure_channel(channel, true).await?;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        unreachable!()
    }

    /// 对应 `status_json` 用到的 resolver 快照（详见 meta.rs）。
    pub fn snapshot_state(&self) -> HashMap<String, ChannelEntry> {
        self.state.lock().cache.clone()
    }

    /// 对应 `write_meta` 中读取 cache_detail 的方法。
    pub fn cache_detail(&self) -> HashMap<String, ChannelEntry> {
        self.snapshot_state()
    }
}

/// 供 meta.rs 组装诊断信息：设备协议是否已注册。
pub fn session_summary(r: &Resolver) -> Option<(i64, String, i64, String)> {
    let st = r.state.lock();
    let s = st.sessions.get(&0)?.clone();
    drop(st);
    let s = s.lock();
    Some((s.generation, s.cloud_guid.clone(), s.heartbeat_count, s.last_heartbeat_result.clone()))
}
