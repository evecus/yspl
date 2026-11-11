//! 工具函数：对应 Python `ysp-live-v9.5.py` 中的 now_f64 / log / sha1_upper /
//! java_string_hashcode / compute_fingerprint / form_encode / 错误分类等。

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use md5::Md5;
use parking_lot::Mutex;
use rand::Rng as _;
use serde_json::Value;
use sha1::Sha1;
use sha2::{Digest as _, Sha256};

use crate::constants::AK;

pub fn now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn current_time_ms() -> i64 {
    (now_f64() * 1000.0) as i64
}

/// Python `rust_round`: math.floor(x + 0.5)，与 Rust 的 round (half away from zero) 不同。
pub fn rust_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

pub fn sha1_upper(value: &str) -> String {
    let digest = Sha1::digest(value.as_bytes());
    hex_upper(&digest)
}

pub fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex_lower(&digest)
}

pub fn md5_hex(value: &str) -> String {
    let digest = Md5::digest(value.as_bytes());
    hex_lower(&digest)
}

pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn hex_upper(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

// ---------------------------------------------------------------- 日志

struct LogState {
    ring: VecDeque<String>,
    last_times: HashMap<String, f64>,
}

static LOG_STATE: OnceLock<Mutex<LogState>> = OnceLock::new();

fn log_state() -> &'static Mutex<LogState> {
    LOG_STATE.get_or_init(|| {
        Mutex::new(LogState {
            ring: VecDeque::with_capacity(120),
            last_times: HashMap::new(),
        })
    })
}

/// 对应 Python `log(msg, min_interval=0.0)`：同消息按间隔去重 + 120 行环形缓冲。
pub fn log(msg: impl AsRef<str>, min_interval: f64) {
    let clean = msg.as_ref().trim();
    if clean.is_empty() {
        return;
    }
    let now = now_f64();
    let interval = if min_interval > 0.0 { min_interval } else { 1.5 };
    let line = {
        let mut st = log_state().lock();
        let last = st.last_times.get(clean).copied().unwrap_or(0.0);
        if now - last < interval {
            return;
        }
        st.last_times.insert(clean.to_string(), now);
        if st.last_times.len() > 600 {
            st.last_times.clear();
        }
        let ts = chrono::Local::now().format("%H:%M:%S");
        let line = format!("[{ts}] {clean}");
        if st.ring.len() == 120 {
            st.ring.pop_front();
        }
        st.ring.push_back(line.clone());
        line
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// `/diag` 用：返回最近 30 条日志（去掉 ANSI 颜色码）。
pub fn recent_logs(n: usize) -> Vec<String> {
    let st = log_state().lock();
    let clean = |s: &str| -> String {
        // 去掉 \033[...m
        let mut out = String::with_capacity(s.len());
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\u{1b}' && it.peek() == Some(&'[') {
                it.next();
                while let Some(&d) = it.peek() {
                    it.next();
                    if d == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    };
    st.ring.iter().rev().take(n).rev().map(|s| clean(s)).collect()
}

// ---------------------------------------------------------------- 设备指纹

/// Java `String.hashCode()`，32 位有符号溢出。
pub fn java_string_hashcode(value: &str) -> i32 {
    let mut h: u32 = 0;
    for ch in value.chars() {
        h = h.wrapping_mul(31).wrapping_add(ch as u32);
    }
    h as i32
}

/// Python `java_uuid_from_hashes`: 两个 32 位 hash 拼成 128 位十六进制（无分隔符）。
pub fn java_uuid_from_hashes(msb_hash: i32, lsb_hash: i32) -> String {
    let msb = msb_hash as u64 as u128; // 负数按补码扩展，等价 Python 的 & 0xFFFFFFFFFFFFFFFF
    let lsb = lsb_hash as u64 as u128;
    format!("{:032x}", (msb << 64) | lsb)
}

/// 东八区「当天 0 点」的 Unix 毫秒（Python native_day0_ms，含 UTC+8 偏移）。
pub fn native_day0_ms(now_s: i64) -> i64 {
    86_400_000 * (now_s + 28_800).div_euclid(86_400) - 28_800_000
}

/// 返回 (X-Fingerprint, timestamp_ms, day0_ms)。
pub fn compute_fingerprint(x_uid: &str, now_ms: i64) -> (String, i64, i64) {
    let day0_ms = native_day0_ms(now_ms / 1000);
    let first = sha256_hex(&format!("{AK}{x_uid}{now_ms}{day0_ms}"));
    (sha256_hex(&first), now_ms, day0_ms)
}

pub fn random_hex_string(length: usize) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| HEX[rng.gen_range(0..16)] as char)
        .collect()
}

pub fn random_mac_address() -> String {
    let mut raw = [0u8; 6];
    rand::thread_rng().fill(&mut raw[..]);
    raw[0] = (raw[0] | 2) & 254;
    raw.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

pub fn sanitize_profile_id(value: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

pub fn resolution_from_screen_param(screen_param: &str) -> String {
    let parts: Vec<&str> = screen_param.split('-').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        format!("{}*{}", parts[0], parts[1])
    } else {
        "3840*2160".to_string()
    }
}

// ---------------------------------------------------------------- JSON / 表单

/// 对应 `compact_json_bytes`：sort_keys=True + ensure_ascii=False + 紧凑分隔 + 可选转义 `/`。
/// serde_json 默认 Map 为 BTreeMap（键有序），且不转义非 ASCII，故只需处理斜杠。
pub fn compact_json_bytes(value: &Value, escape_forward_slashes: bool) -> Vec<u8> {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    if escape_forward_slashes {
        text.replace('/', "\\/").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// 对应 `form_urlencode_value`：逐 UTF-8 字节，大写 %XX，空格为 `+`。
pub fn form_urlencode_value(s: &str) -> String {
    let mut out = String::new();
    for &byte in s.as_bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '*' | '-' | '.' | '_') {
            out.push(ch);
        } else if byte == 32 {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn form_encode(pairs: &[(&str, &str)]) -> Vec<u8> {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_urlencode_value(k), form_urlencode_value(v)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

pub fn header_value_clean(value: &str) -> String {
    value.replace(['\r', '\n'], "")
}

pub fn url_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

pub fn b64_standard_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn b64_standard_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

pub fn b64_urlsafe_nopad_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

pub fn b64_urlsafe_nopad_decode(s: &str) -> Option<Vec<u8>> {
    // 对应 Python：先补齐 `=` 再 urlsafe_b64decode。
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.as_bytes())
        .ok()
        .or_else(|| {
            let padded = format!("{}{}", s, "=".repeat((4 - s.len() % 4) % 4));
            base64::engine::general_purpose::URL_SAFE.decode(padded.as_bytes()).ok()
        })
}

// ---------------------------------------------------------------- 错误分类

fn has_http_400(lower: &str) -> bool {
    lower.contains("http 400") || lower.contains("status=400")
}

pub fn is_session_core_http_400(lower: &str) -> bool {
    has_http_400(lower)
        && ["app/start", "live/v1/01", "live/v1/02", "vdn", "heartbeat"]
            .iter()
            .any(|n| lower.contains(n))
}

pub fn is_control_plane_http_400(lower: &str) -> bool {
    has_http_400(lower)
        && [
            "app/start", "dictionary", "app event", "page event", "cloud_get",
            "cloud get", "cloud_register", "cloud register", "device info",
            "heartbeat", "index", "drm config", "version config", "live/v1/01",
            "live/v1/02", "vdn", "collect report",
        ]
        .iter()
        .any(|n| lower.contains(n))
}

pub fn is_session_invalidating_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("app/start")
        || lower.contains("decrypt")
        || lower.contains("session_key")
        || lower.contains("missing encrypted session key")
        || is_session_core_http_400(&lower)
        || ["live/v1/01 http 400", "live/v1/01 http 401", "live/v1/01 http 403",
            "live/v1/02 http 400", "live/v1/02 http 401", "live/v1/02 http 403",
            "vdn http 400", "vdn http 401", "vdn http 403", "内部错误",
            "no usable url", "missing videos"]
            .iter()
            .any(|n| lower.contains(n))
}

pub fn is_identity_recoverable_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    is_session_invalidating_error(message)
        || is_control_plane_http_400(&lower)
        || ["upstream m3u8 http 400", "upstream m3u8 http 401", "upstream m3u8 http 403",
            "upstream m3u8 http 404", "playlist response missing", "invalid playlist status",
            "connection reset", "connection refused", "operation timed out", "timed out",
            "network is unreachable", "nodename nor servname", "failed to lookup address"]
            .iter()
            .any(|n| lower.contains(n))
}

// ---------------------------------------------------------------- 其他

/// 对应 `parse_result_code`：按固定键序从 JSON 里递归找整型返回码。
pub fn parse_result_code(value: &Value) -> Option<i64> {
    let obj = value.as_object()?;
    for key in ["result", "code", "errCode", "errcode", "ret"] {
        if let Some(raw) = obj.get(key) {
            if raw.is_boolean() {
                continue;
            }
            if let Some(i) = raw.as_i64() {
                return Some(i);
            }
            if let Some(s) = raw.as_str() {
                if let Ok(i) = s.parse::<i64>() {
                    return Some(i);
                }
            }
        }
    }
    for key in ["data", "error", "response"] {
        if let Some(inner) = obj.get(key) {
            if let Some(found) = parse_result_code(inner) {
                return Some(found);
            }
        }
    }
    None
}

pub fn extract_guid(value: &Value) -> String {
    value
        .get("data")
        .and_then(|d| d.get("guid"))
        .and_then(|g| g.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn normalize_channel(raw: &str) -> String {
    let mut key = raw.trim().trim_matches('/').to_lowercase();
    if key.ends_with(".m3u8") {
        key.truncate(key.len() - ".m3u8".len());
    }
    if matches!(key.as_str(), "cctv16-4k" | "cctv16_4k" | "cctv16/4k" | "cctv16(4k)") {
        return "cctv164k".to_string();
    }
    key
}

pub fn http_status_text(status: u16, reason: &str) -> String {
    if reason.is_empty() {
        status.to_string()
    } else {
        format!("{status} {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_hashcode_matches_python() {
        // Python: java_string_hashcode("abc") == 96354
        assert_eq!(java_string_hashcode("abc"), 96354);
        assert_eq!(java_string_hashcode(""), 0);
    }

    #[test]
    fn fingerprint_shape() {
        let (fp, ts, day0) = compute_fingerprint("ABC", 1_700_000_000_000);
        assert_eq!(fp.len(), 64);
        assert_eq!(ts, 1_700_000_000_000);
        // Python: day0 = 86400000*((now_s+28800)//86400) - 28800000（UTC+8 对齐日界）
        assert_eq!(day0 % 86_400_000, 57_600_000);
    }

    #[test]
    fn form_encode_matches_python() {
        assert_eq!(form_urlencode_value("a b/c中"), "a+b%2Fc%E4%B8%AD");
        assert_eq!(form_urlencode_value("*-._~"), "*-._%7E");
    }

    #[test]
    fn compact_json_sorted_and_slash_escaped() {
        let v = serde_json::json!({"b": 1, "a": "x/y"});
        assert_eq!(
            String::from_utf8(compact_json_bytes(&v, true)).unwrap(),
            "{\"a\":\"x\\/y\",\"b\":1}"
        );
        assert_eq!(
            String::from_utf8(compact_json_bytes(&v, false)).unwrap(),
            "{\"a\":\"x/y\",\"b\":1}"
        );
    }

    #[test]
    fn b64_urlsafe_roundtrip() {
        let enc = b64_urlsafe_nopad_encode(b"hello\x00world");
        assert!(!enc.contains('='));
        assert_eq!(b64_urlsafe_nopad_decode(&enc).unwrap(), b"hello\x00world");
    }
}
