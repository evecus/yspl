use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::device::DeviceProfile;
use crate::http_client::HttpClient;
use crate::identity::Identity;

#[derive(Clone, Debug, clap::Parser)]
#[command(name = "yspl", version, about = "央视频全频道直播代理（yspl v0.1.0）")]
pub struct Cli {

    #[arg(default_value_t = 8767)]
    pub port: u16,

    #[arg(long, default_value = "0.0.0.0")]
    pub bind: String,

    #[arg(long, env = "YSP_DATA_DIR", default_value = "")]
    pub data_dir: String,

    #[arg(long, default_value_t = 200)]
    pub cache_mb: i64,

    #[arg(long)]
    pub no_4k: bool,

    #[arg(long)]
    pub only_4k: bool,

    #[arg(long, env = "YSP_LINKS_PER_DEVICE", default_value_t = 6)]
    pub links_per_device: usize,

    #[arg(long, env = "YSP_ZAP_DEBOUNCE", default_value_t = 10.0)]
    pub zap_debounce: f64,

    #[arg(long, default_value_t = 8787)]
    pub engine_port: u16,

    #[arg(long, default_value_t = 10.0)]
    pub timeout: f64,

    #[arg(long)]
    pub no_engine: bool,
}

#[derive(Clone, Debug)]
pub struct EngineArgs {
    pub host: String,
    pub port: u16,
    pub timeout: f64,
    pub insecure_tls: bool,
    pub cache_ttl: f64,
    pub stale_while_refresh_ttl: f64,
    pub refresh_error_cooldown: f64,
    pub playlist_cache_ttl: f64,
    pub background_refresh_queue_limit: usize,
    pub http_workers: usize,
    pub http_queue_limit: usize,
    pub identity_reset_error_threshold: i64,
    pub identity_reset_cooldown: f64,
    pub refresh_interval: f64,
    pub control_step_jitter_min_ms: i64,
    pub control_step_jitter_max_ms: i64,
    pub heartbeat_interval: f64,
    pub heartbeat_ttl_guard: f64,
    pub session_ttl: f64,
    pub meta_json: std::path::PathBuf,
    pub device_json: std::path::PathBuf,
    pub links_per_device: usize,
    pub zap_debounce: f64,
}

impl EngineArgs {
    pub fn from_cli(cli: &crate::Cli, data_dir: &std::path::Path) -> Self {
        Self {
            host: cli.bind.clone(),
            port: cli.port,
            timeout: cli.timeout,
            insecure_tls: false,
            cache_ttl: 60.0,
            stale_while_refresh_ttl: 0.0,
            refresh_error_cooldown: 5.0,
            playlist_cache_ttl: 2.0,
            background_refresh_queue_limit: 4,
            http_workers: 16,
            http_queue_limit: 1000,
            identity_reset_error_threshold: 0,
            identity_reset_cooldown: 300.0,
            refresh_interval: 0.2,
            control_step_jitter_min_ms: 0,
            control_step_jitter_max_ms: 0,
            heartbeat_interval: 30.0,
            heartbeat_ttl_guard: 60.0,
            session_ttl: 7200.0,
            meta_json: data_dir.join("proxy-cache-state-rs.json"),
            device_json: data_dir.join("device-state-rs.json"),
            links_per_device: cli.links_per_device,
            zap_debounce: cli.zap_debounce,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelEntry {
    pub channel: String,
    pub live_id: String,
    pub final_url: String,
    pub playback_headers: HashMap<String, String>,
    pub android_id: String,
    pub x_uid: String,
    pub rate: String,
    pub rate_name: String,
    pub raw_live_host: String,
    pub final_host: String,
    pub refreshed_at: f64,
    pub expires_at: f64,
    pub generation: i64,
    pub last_refresh_error: String,
    pub last_refresh_failed_at: f64,
    pub session_generation: i64,
}

impl ChannelEntry {
    pub fn fresh(&self, now: f64) -> bool {
        !self.final_url.is_empty() && now < self.expires_at
    }

    pub fn stale_usable(&self, now: f64, ttl: f64) -> bool {
        !self.final_url.is_empty() && ttl > 0.0 && now < self.expires_at + ttl
    }

    pub fn from_value(v: &Value) -> Self {
        let obj = v.as_object();
        let s = |k: &str| -> String {
            obj.and_then(|o| o.get(k))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        let f = |k: &str| -> f64 { obj.and_then(|o| o.get(k)).and_then(|x| x.as_f64()).unwrap_or(0.0) };
        let i = |k: &str| -> i64 { obj.and_then(|o| o.get(k)).and_then(|x| x.as_i64()).unwrap_or(0) };
        let headers = obj
            .and_then(|o| o.get("playback_headers"))
            .and_then(|x| x.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            channel: s("channel"), live_id: s("live_id"), final_url: s("final_url"),
            playback_headers: headers, android_id: s("android_id"), x_uid: s("x_uid"),
            rate: s("rate"), rate_name: s("rate_name"), raw_live_host: s("raw_live_host"),
            final_host: s("final_host"), refreshed_at: f("refreshed_at"),
            expires_at: f("expires_at"), generation: i("generation"),
            last_refresh_error: s("last_refresh_error"),
            last_refresh_failed_at: f("last_refresh_failed_at"),
            session_generation: i("session_generation"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlaylistCacheEntry {
    pub body: String,
    pub content_type: String,
    pub cached_at: f64,
    pub expires_at: f64,
    pub final_url: String,
    pub android_id: String,
    pub proxy_origin: String,
    pub proxy_prefix: String,
}

#[derive(Clone, Debug)]
pub struct PlaylistCacheProbe {
    pub state: String,
    pub age_ms: Option<i64>,
}

pub struct AppStartResult {
    pub session_key: String,
}
pub struct Live01Result {
    pub live_url: String,
    pub rate: String,
    pub rate_name: String,
}
pub struct VdnGetStreamResult {
    pub final_url: String,
    pub app_sign: String,
    pub app_random_str: String,
}

pub struct AppSession {
    pub profile: DeviceProfile,
    pub identity: Identity,
    pub client: HttpClient,
    pub session_key: String,
    pub cloud_guid: String,
    pub version: String,
    pub screen_param: String,
    pub cast_model: String,
    pub created_at: f64,
    pub generation: i64,
    pub last_heartbeat_at: f64,
    pub heartbeat_count: i64,
    pub last_heartbeat_result: String,
    pub last_heartbeat_error: String,
    pub linked_channels: HashSet<String>,
}

pub type SharedSession = Arc<Mutex<AppSession>>;

#[derive(Default)]
pub struct RefreshQueueStats {
    pub queued: VecDeque<String>,
    pub queued_channels: HashMap<String, f64>,
    pub running_channel: String,
    pub running_started_at: f64,
    pub enqueued_total: i64,
    pub dropped_total: i64,
    pub completed_total: i64,
    pub failed_total: i64,
    pub last_enqueued_at: f64,
    pub last_finished_at: f64,
    pub last_dropped_at: f64,
    pub last_drop_reason: String,
}

#[derive(Default)]
pub struct HttpQueueStats {
    pub queued: VecDeque<String>,
    pub active_workers: i64,
    pub enqueued_total: i64,
    pub rejected_total: i64,
    pub completed_total: i64,
    pub panicked_total: i64,
    pub last_enqueued_at: f64,
    pub last_completed_at: f64,
    pub last_rejected_at: f64,
}

pub struct ResolverState {
    pub cache: HashMap<String, ChannelEntry>,
    pub playlist_cache: HashMap<String, PlaylistCacheEntry>,
    pub background_refreshing: HashMap<String, f64>,
    pub channel_failures: HashMap<String, (f64, String, i32)>,
    pub generation: i64,
    pub session_generation: i64,
    pub sessions: HashMap<i32, SharedSession>,
    pub channel_slot_map: HashMap<String, i32>,
    pub slot_last_reset: HashMap<i32, f64>,
    pub last_control_request_at: f64,
    pub last_business_end_at: f64,
    pub refreshing_channel: String,
    pub last_error: String,
    pub last_error_at: f64,
    pub identity_reset_error_count: i64,
    pub channel_last_access: HashMap<String, f64>,
    pub identity_reset_count: i64,
    pub last_identity_reset_at: f64,
    pub last_identity_reset_reason: String,
    pub ad_display: bool,
    pub zap_channel: String,
    pub zap_time: f64,
    pub zap_pending: String,
    pub zap_generation: i64,
    pub zap_times: HashMap<String, f64>,
    pub zap_pending_channels: HashSet<String>,
    pub zap_generations: HashMap<String, i64>,
    pub last_device_wipe_at: f64,
    pub device_wipe_day: String,
    pub device_wipe_count: i64,
    pub refresh_queue: RefreshQueueStats,
    pub http_queue: HttpQueueStats,
    pub playlist_locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

impl Default for ResolverState {
    fn default() -> Self {
        Self {
            cache: HashMap::new(), playlist_cache: HashMap::new(),
            background_refreshing: HashMap::new(), channel_failures: HashMap::new(),
            generation: 0, session_generation: 0, sessions: HashMap::new(),
            channel_slot_map: HashMap::new(), slot_last_reset: HashMap::new(),
            last_control_request_at: 0.0, last_business_end_at: 0.0,
            refreshing_channel: String::new(), last_error: String::new(), last_error_at: 0.0,
            identity_reset_error_count: 0, channel_last_access: HashMap::new(),
            identity_reset_count: 0, last_identity_reset_at: 0.0,
            last_identity_reset_reason: String::new(), ad_display: false,
            zap_channel: String::new(), zap_time: 0.0, zap_pending: String::new(),
            zap_generation: 0, zap_times: HashMap::new(), zap_pending_channels: HashSet::new(),
            zap_generations: HashMap::new(), last_device_wipe_at: 0.0,
            device_wipe_day: String::new(), device_wipe_count: 0,
            refresh_queue: RefreshQueueStats::default(), http_queue: HttpQueueStats::default(),
            playlist_locks: HashMap::new(),
        }
    }
}

struct TsItem {
    data: Vec<u8>,
    headers: Vec<(String, String)>,
    expire_at: f64,
}

pub struct TsSegmentCache {
    inner: Mutex<TsInner>,
}

pub type CachedResponse = (Vec<u8>, Vec<(String, String)>);

struct TsInner {
    max_bytes: usize,
    max_age: f64,
    cache: HashMap<String, TsItem>,
    order: VecDeque<String>,
    curr_bytes: usize,
    hits: i64,
    misses: i64,
}

impl TsSegmentCache {
    pub fn new(max_mb: usize, max_age_sec: f64) -> Self {
        Self {
            inner: Mutex::new(TsInner {
                max_bytes: max_mb * 1024 * 1024,
                max_age: max_age_sec,
                cache: HashMap::new(),
                order: VecDeque::new(),
                curr_bytes: 0,
                hits: 0,
                misses: 0,
            }),
        }
    }

    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let mut st = self.inner.lock();
        let now = crate::util::now_f64();
        match st.cache.get(key) {
            None => {
                st.misses += 1;
                None
            }
            Some(item) if now > item.expire_at => {
                let size = item.data.len();
                st.cache.remove(key);
                st.order.retain(|k| k != key);
                st.curr_bytes = st.curr_bytes.saturating_sub(size);
                st.misses += 1;
                None
            }
            Some(item) => {
                let data = item.data.clone();
                let headers = item.headers.clone();
                st.hits += 1;
                Some((data, headers))
            }
        }
    }

    pub fn set(&self, key: &str, data: Vec<u8>, headers: Vec<(String, String)>) {
        let mut st = self.inner.lock();
        if data.len() > st.max_bytes {
            return;
        }
        if let Some(old) = st.cache.remove(key) {
            st.curr_bytes = st.curr_bytes.saturating_sub(old.data.len());
            st.order.retain(|k| k != key);
        }
        while st.curr_bytes + data.len() > st.max_bytes && !st.order.is_empty() {
            if let Some(old_key) = st.order.pop_front() {
                if let Some(old) = st.cache.remove(&old_key) {
                    st.curr_bytes = st.curr_bytes.saturating_sub(old.data.len());
                }
            }
        }
        let now = crate::util::now_f64();
        let expire_at = now + st.max_age;
        st.cache.insert(
            key.to_string(),
            TsItem { data: data.clone(), headers, expire_at },
        );
        st.order.push_back(key.to_string());
        st.curr_bytes += data.len();
    }

    pub fn stats(&self) -> Value {
        let st = self.inner.lock();
        let total = (st.hits + st.misses).max(1);
        serde_json::json!({
            "count": st.cache.len(),
            "size_mb": (st.curr_bytes as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0,
            "hits": st.hits,
            "misses": st.misses,
            "hit_rate": format!("{:.1}%", st.hits as f64 / total as f64 * 100.0),
        })
    }
}

pub struct Channel {
    pub slug: String,
    pub name: String,
    pub sid: String,
    pub pid: String,
    pub defn: String,
    pub segments: HashMap<String, (i64, f64, String, String)>,
    pub order: VecDeque<String>,
    pub seen_keys: VecDeque<String>,
    pub seq: i64,
    pub last_access: f64,
    pub last_error: String,
    pub last_ok: f64,
    pub mode: String,
    pub last_pdt: String,
    pub bk_urls: Vec<String>,
    pub bk_urls_time: f64,
    pub bk_playlist: String,
    pub starting: bool,
    pub last_built_playlist: String,
    pub last_built_time: f64,
    pub last_m3u8_source: String,
    pub thread_alive: bool,
}

impl Channel {
    pub fn new(slug: &str, name: &str, sid: &str, pid: &str, defn: &str) -> Self {
        let mode = if crate::channels::is_force_bk(slug) { "bk" } else { "jce" }.to_string();
        Self {
            slug: slug.to_string(), name: name.to_string(), sid: sid.to_string(),
            pid: pid.to_string(), defn: defn.to_string(),
            segments: HashMap::new(), order: VecDeque::new(),
            seen_keys: VecDeque::with_capacity(crate::constants::MAX_SEGS * 2),
            seq: 0, last_access: 0.0, last_error: String::new(), last_ok: 0.0,
            mode, last_pdt: String::new(), bk_urls: Vec::new(), bk_urls_time: 0.0,
            bk_playlist: String::new(), starting: false, last_built_playlist: String::new(),
            last_built_time: 0.0, last_m3u8_source: String::new(), thread_alive: false,
        }
    }
}

pub type SharedChannel = Arc<Mutex<Channel>>;

pub struct HighLocks {
    inner: Mutex<HashMap<String, (String, f64)>>,
    start: Instant,
    timeout: f64,
}

impl Default for HighLocks {
    fn default() -> Self {
        Self::new()
    }
}

impl HighLocks {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            start: Instant::now(),
            timeout: crate::constants::high_lock_timeout(),
        }
    }

    pub fn check_and_update(&self, channel: &str, client_ip: &str) -> bool {
        let channel = crate::util::normalize_channel(channel);
        let client_ip = client_ip.trim();
        if channel.is_empty() || client_ip.is_empty() {
            return false;
        }
        let now = self.start.elapsed().as_secs_f64();
        let mut locks = self.inner.lock();
        match locks.get(&channel).cloned() {
            None => {
                crate::util::log(format!("[{channel}] 高码率通道已由 {client_ip} 独占"), 0.0);
                locks.insert(channel, (client_ip.to_string(), now));
                true
            }
            Some((owner, last_seen)) if now - last_seen >= self.timeout => {
                crate::util::log(format!("[{channel}] 原高码率锁超时释放，新设备 {client_ip} 接管"), 0.0);
                locks.insert(channel, (client_ip.to_string(), now));
                true
            }
            Some((owner, _)) if owner == client_ip => {
                locks.insert(channel, (owner, now));
                true
            }
            _ => false,
        }
    }
}

pub struct AppState {
    pub cli: crate::Cli,
    pub args: EngineArgs,
    pub data_dir: std::path::PathBuf,
    pub directory: crate::channels::ChannelDirectory,
    pub channel_map: RwLock<HashMap<String, SharedChannel>>,
    pub ts_cache: TsSegmentCache,
    pub high_locks: HighLocks,
    pub resolver_ready: AtomicBool,
    pub server_start_time: f64,
    pub engine_port: u16,
    pub active_backend: HashSet<String>,
    pub resolver: RwLock<Option<Arc<crate::resolver::Resolver>>>,

    pub inflight_events: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,

    pub prefetch_events: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub flow_limiter: crate::hls::ApiFlowLimiter,
    pub engine_proxy_suffixes: Vec<&'static str>,
}

impl AppState {
    pub fn new(cli: crate::Cli) -> Self {
        let data_dir = if cli.data_dir.is_empty() {
            std::env::var("YSP_DATA_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| crate::device::binary_dir())
        } else {
            std::path::PathBuf::from(&cli.data_dir)
        };
        let args = EngineArgs::from_cli(&cli, &data_dir);
        let directory = crate::channels::ChannelDirectory::new();
        let mut channel_map = HashMap::new();
        for m in directory.unique_channels() {
            channel_map.insert(
                m.slug.clone(),
                Arc::new(Mutex::new(Channel::new(&m.slug, &m.name, &m.sid, &m.pid, &m.defn))),
            );
        }
        let active_backend = if cli.no_4k {
            HashSet::new()
        } else if cli.only_4k || crate::constants::env_only_4k() {
            crate::channels_data::TRUE_4K_CHANNELS.iter().map(|s| (*s).to_string()).collect()
        } else {
            crate::channels_data::BACKEND_CHANNELS.iter().map(|s| (*s).to_string()).collect()
        };
        Self {
            cli: cli.clone(),
            args,
            data_dir,
            directory,
            channel_map: RwLock::new(channel_map),
            ts_cache: TsSegmentCache::new(cli.cache_mb.max(0) as usize, 180.0),
            high_locks: HighLocks::new(),
            resolver_ready: AtomicBool::new(false),
            server_start_time: crate::util::now_f64(),
            engine_port: cli.engine_port,
            active_backend,
            resolver: RwLock::new(None),
            inflight_events: Mutex::new(HashMap::new()),
            prefetch_events: Mutex::new(HashMap::new()),
            flow_limiter: crate::hls::ApiFlowLimiter::new(700, 250),
            engine_proxy_suffixes: crate::channels_data::ALLOWED_ENGINE_PROXY_SUFFIXES.to_vec(),
        }
    }

    pub fn ready(&self) -> bool {
        self.resolver_ready.load(Ordering::SeqCst)
    }

    pub fn set_ready(&self, v: bool) {
        self.resolver_ready.store(v, Ordering::SeqCst);
    }

    pub fn get_or_create_channel(&self, slug: &str) -> Option<SharedChannel> {
        let canonical = crate::channels::canonical(slug);
        if let Some(ch) = self.channel_map.read().get(&canonical) {
            return Some(ch.clone());
        }
        let meta = self.directory.get_meta(&canonical)?;
        let ch = Arc::new(Mutex::new(Channel::new(
            &meta.slug, &meta.name, &meta.sid, &meta.pid, &meta.defn,
        )));
        self.channel_map.write().insert(canonical, ch.clone());
        Some(ch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_cache_lru_by_bytes() {
        let c = TsSegmentCache::new(1, 60.0);
        c.set("a", vec![0u8; 600_000], vec![]);
        c.set("b", vec![0u8; 600_000], vec![]);
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
        let s = c.stats();
        assert_eq!(s["count"].as_i64().unwrap(), 1);
    }

    #[test]
    fn high_lock_same_ip_renews() {
        let l = HighLocks::new();
        assert!(l.check_and_update("cctv1", "1.2.3.4"));
        assert!(l.check_and_update("cctv1", "1.2.3.4"));
        assert!(!l.check_and_update("cctv1", "5.6.7.8"));
    }
}
