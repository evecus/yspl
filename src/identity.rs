//! 对应 Python `Identity` / `build_identity` / `fresh_headers` / `root_headers` /
//! `build_report_common_value` / `build_app_start_body` / `build_collect_headers` /
//! `build_vdn_appcommon` / `default_cctv_playback_headers` /
//! `cctv_playback_headers_for_entry` / `_playback_request_headers` /
//! `entry_missing_signed_playback_headers` / `ts_path_matches_final_url`。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::constants::{
    AK, DEFAULT_ACCEPT_LANGUAGE, REPORT_APP_KEY, VDN_APP_NAME,
};
use crate::device::{infer_os_version, report_model_from_build, DeviceProfile};
use crate::state::ChannelEntry;
use crate::util::{
    compute_fingerprint, current_time_ms, header_value_clean, url_host,
};

pub type Headers = Vec<(String, String)>;

/// 把持久化/内部 headers 转成 HttpClient 需要的借用切片。
pub fn as_pairs(h: &Headers) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn set_header(h: &mut Headers, key: &str, value: &str) {
    if let Some(item) = h.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        item.1 = value.to_string();
    } else {
        h.push((key.to_string(), value.to_string()));
    }
}

fn get_header(h: &Headers, key: &str) -> Option<String> {
    h.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub x_uid: String,
    pub x_fingerprint: String,
    pub fingerprint_timestamp_ms: i64,
    pub headers: Headers,
}

pub fn build_identity(profile: &DeviceProfile, app_channel: &str, version: &str) -> Identity {
    let x_uid = crate::device::compute_x_uid(profile);
    let (xfp, ts, _) = compute_fingerprint(&x_uid, current_time_ms());
    let headers: Headers = vec![
        ("Accept", "application/json"),
        ("Accept-Language", DEFAULT_ACCEPT_LANGUAGE),
        ("Referer", "api.cctv.cn"),
        ("User-Agent", "cctv_app_tv"),
        ("UID", profile.android_id.as_str()),
        ("appChannel", app_channel),
        ("X-Uid", x_uid.as_str()),
        ("X-Fingerprint", xfp.as_str()),
        ("X-Version", version),
        ("Content-Type", "application/json; charset=utf-8"),
        ("Connection", "Keep-Alive"),
        ("Accept-Encoding", "gzip"),
        ("Cache-Control", "no-cache"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    Identity { x_uid, x_fingerprint: xfp, fingerprint_timestamp_ms: ts, headers }
}

/// 对应 `fresh_headers`：按顺序重组请求头，注入 X-Timestamp / X-Nonce。
pub fn fresh_headers(
    template: &Headers,
    content_type: &str,
    accept: Option<&str>,
    force_ts: Option<i64>,
) -> Headers {
    let mut h: Headers = Vec::new();
    if let Some(a) = accept {
        h.push(("Accept".to_string(), a.to_string()));
    }
    let req_ts = force_ts.unwrap_or_else(current_time_ms);
    h.push(("X-Timestamp".to_string(), req_ts.to_string()));
    h.push(("X-Nonce".to_string(), uuid::Uuid::new_v4().to_string()));
    for key in [
        "Accept-Language", "Referer", "User-Agent", "UID", "appChannel",
        "X-Uid", "X-Fingerprint", "X-Version",
    ] {
        if let Some(v) = get_header(template, key) {
            h.push((key.to_string(), v));
        }
    }
    if !content_type.is_empty() {
        h.push(("Content-Type".to_string(), content_type.to_string()));
    }
    for key in ["Connection", "Accept-Encoding", "Cache-Control"] {
        if let Some(v) = get_header(template, key) {
            h.push((key.to_string(), v));
        }
    }
    h
}

pub fn root_headers(version: &str) -> Headers {
    let nonce = uuid::Uuid::new_v4().to_string();
    let ts = current_time_ms().to_string();
    vec![
        ("X-Uid", "ROOT"),
        ("X-Fingerprint", "ROOT"),
        ("X-Nonce", nonce.as_str()),
        ("X-Timestamp", ts.as_str()),
        ("X-Version", version),
        ("UID", "ROOT"),
        ("Referer", "api.cctv.cn"),
        ("User-Agent", "cctv_app_tv"),
        ("appChannel", "ROOT"),
        ("Connection", "Keep-Alive"),
        ("Accept-Encoding", "gzip"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

pub fn build_vdn_appcommon(version: &str) -> String {
    serde_json::to_string(&json!({
        "adid": "", "av": version, "an": VDN_APP_NAME, "ap": "cctv_app_tv"
    }))
    .unwrap_or_default()
}

/// Python `app_start_field(value, limit)`：str(value)[:limit]，按字符截断。
pub fn app_start_field(value: &str, limit: usize) -> String {
    if limit == usize::MAX {
        return value.to_string();
    }
    value.chars().take(limit).collect()
}

/// 对应 `build_report_common_value`，返回有序 JSON（serde 默认 BTreeMap == sort_keys）。
pub fn build_report_common_value(
    profile: &DeviceProfile,
    x_uid: &str,
    app_channel: &str,
    version: &str,
    sdk_version: &str,
    data_time_ms: i64,
) -> Value {
    let model = if profile.report_model.is_empty() {
        report_model_from_build(profile)
    } else {
        profile.report_model.clone()
    };
    let f = |v: &str, limit: usize| -> Value { Value::String(app_start_field(v, limit)) };
    let i = |v: i64, limit: usize| -> Value { Value::String(app_start_field(&v.to_string(), limit)) };
    json!({
        "cctv_id": f(x_uid, 64),
        "device_id": f(&profile.android_id, 64),
        "idfa": "", "idfv": "", "user_id": "",
        "app_key": f(REPORT_APP_KEY, 64),
        "imei": "",
        "android_id": f(&profile.android_id, 64),
        "mac": f(&profile.mac, 64),
        "device_builder_type": f(&profile.build_type, 64),
        "device_hardware": f(&profile.hardware, 64),
        "device_board": f(&profile.board, 64),
        "device_brand": f(&profile.brand, 64),
        "device_params": f(&profile.device, 64),
        "device_display": f(&profile.display, 64),
        "device_version_id": f(&profile.version_id, 64),
        "device_host": f(&profile.host, 128),
        "device_product": f(&profile.product, 64),
        "device_tags": f(&profile.tags, 64),
        "device_user": f(&profile.user, 30),
        "device_fingerprint": f(&profile.fingerprint, 128),
        "device_manufacturer": f(&profile.manufacturer, 64),
        "device_model": f(&model, 50),
        "device_resolution": f(&profile.resolution, 20),
        "system_type": "Android",
        "device_type": "TV",
        "app_language": "CHINESE",
        "app_version": f(version, 30),
        "sdk_version": f(sdk_version, 30),
        "os_version": f(&infer_os_version(profile), 20),
        "app_channel": f(app_channel, 50),
        "data_time": i(data_time_ms, 13),
    })
}

pub fn build_app_start_body(
    profile: &DeviceProfile,
    x_uid: &str,
    app_channel: &str,
    version: &str,
    data_time_ms: i64,
) -> Value {
    json!({
        "key": "app_start_d1",
        "value": build_report_common_value(profile, x_uid, app_channel, version, "", data_time_ms),
    })
}

pub fn build_collect_headers(profile: &DeviceProfile) -> Headers {
    let release = infer_os_version(profile);
    let os = if release.is_empty() { "Android" } else { &release };
    let ua = format!(
        "Dalvik/2.1.0 (Linux; U; Android {}; {} Build/{})",
        os, profile.model, profile.version_id
    );
    vec![
        ("Content-type".to_string(), "application/x-www-form-urlencoded".to_string()),
        ("Charset".to_string(), "UTF-8".to_string()),
        ("User-Agent".to_string(), ua),
        ("Connection".to_string(), "Keep-Alive".to_string()),
        ("Accept-Encoding".to_string(), "gzip".to_string()),
    ]
}

pub fn default_cctv_playback_headers(uid: &str) -> HashMap<String, String> {
    HashMap::from([
        ("UID".to_string(), uid.to_string()),
        ("APPID".to_string(), AK.to_string()),
        ("Referer".to_string(), "api.cctv.cn".to_string()),
        ("User-Agent".to_string(), "cctv_app_tv".to_string()),
    ])
}

pub fn cctv_playback_headers_for_entry(entry: &ChannelEntry) -> HashMap<String, String> {
    let mut h = default_cctv_playback_headers(&entry.android_id);
    for key in ["UID", "APPID", "APPRANDOMSTR", "Referer", "User-Agent", "APPSIGN"] {
        if let Some(v) = entry.playback_headers.get(key) {
            if !v.is_empty() {
                h.insert(key.to_string(), v.clone());
            }
        }
    }
    h
}

pub fn entry_missing_signed_playback_headers(entry: &ChannelEntry) -> bool {
    let host = if entry.final_host.is_empty() {
        url_host(&entry.final_url)
    } else {
        entry.final_host.clone()
    };
    if !crate::jce::host_needs_signed_headers(&host) {
        return false;
    }
    ["APPID", "APPSIGN", "APPRANDOMSTR"]
        .iter()
        .any(|k| entry.playback_headers.get(*k).map(|v| v.trim().is_empty()).unwrap_or(true))
}

/// 对应 `_playback_request_headers`（白名单 + 固定 Accept/Connection）。
pub fn playback_request_headers(
    playback_headers: &HashMap<String, String>,
    extra: Option<&[(String, String)]>,
) -> Headers {
    let mut h: Headers = Vec::new();
    for key in ["UID", "APPID", "APPRANDOMSTR", "Referer", "User-Agent", "APPSIGN"] {
        if let Some(v) = playback_headers.get(key) {
            if !v.trim().is_empty() {
                h.push((key.to_string(), header_value_clean(v)));
            }
        }
    }
    h.push(("Accept".to_string(), "*/*".to_string()));
    h.push(("Accept-Encoding".to_string(), "identity".to_string()));
    h.push(("Connection".to_string(), "close".to_string()));
    if let Some(items) = extra {
        for (k, v) in items {
            set_header(&mut h, k, v);
        }
    }
    h
}

pub fn ts_path_matches_final_url(ts_path: &str, final_url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(final_url) else { return false };
    let mut prefix = parsed.path().to_string();
    if prefix.ends_with(".m3u8") {
        prefix.truncate(prefix.len() - ".m3u8".len());
    }
    !prefix.is_empty() && ts_path.starts_with(&prefix)
}

pub fn is_ts_url(value: &str) -> bool {
    url::Url::parse(value)
        .map(|u| u.path().to_lowercase().ends_with(".ts"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::default_device_state;

    #[test]
    fn fresh_headers_order_and_nonce() {
        let s = default_device_state();
        let id = build_identity(&s.profile, "dangbei", "1.4.1");
        let h = fresh_headers(&id.headers, "application/json", Some("application/json"), None);
        assert_eq!(h[0].0, "Accept");
        assert_eq!(h[1].0, "X-Timestamp");
        assert_eq!(h[2].0, "X-Nonce");
        assert_eq!(h[3].0, "Accept-Language");
        assert!(h.iter().any(|(k, _)| k == "X-Uid"));
    }

    #[test]
    fn report_common_value_field_limits() {
        let s = default_device_state();
        let v = build_report_common_value(&s.profile, &s.x_uid, "dangbei", "1.4.1", "", 1_700_000_000_000);
        assert_eq!(v["cctv_id"].as_str().unwrap().len(), 40);
        assert_eq!(v["data_time"].as_str().unwrap().len(), 13);
        assert_eq!(v["system_type"].as_str().unwrap(), "Android");
    }

    #[test]
    fn playback_headers_whitelist_only() {
        let mut m = default_cctv_playback_headers("abc");
        m.insert("X-Evil".into(), "header injection".into());
        m.insert("APPID".into(), "OV".into());
        let h = playback_request_headers(&m, None);
        assert!(!h.iter().any(|(k, _)| k == "X-Evil"));
        assert!(h.iter().any(|(k, v)| k == "APPID" && v == "OV"));
        assert!(h.iter().any(|(k, v)| k == "Connection" && v == "close"));
    }

    #[test]
    fn ts_path_prefix_match() {
        // Python 语义：ts_path.startswith(final_url 路径去掉 .m3u8)
        assert!(ts_path_matches_final_url("/live/a/playlist1.ts", "https://x.cn/live/a/playlist.m3u8"));
        assert!(!ts_path_matches_final_url("/live/a/b.ts", "https://x.cn/live/a/playlist.m3u8"));
        assert!(!ts_path_matches_final_url("/other.ts", "https://x.cn/live/a/playlist.m3u8"));
    }
}
