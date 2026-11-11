use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::constants::*;
use crate::crypto::{aes_gcm_decrypt_b64, aes_gcm_encrypt_b64, rsa_encrypt_device_id};
use crate::crypto::tea::compute_vdn_code;
use crate::device::{infer_os_version, infer_sdk_int, DeviceProfile, DeviceState};
use crate::error::{Result, YsptpError};
use crate::http_client::{response_json, HttpClient};
use crate::identity::{
    as_pairs, build_app_start_body, build_collect_headers, build_identity, build_report_common_value,
    build_vdn_appcommon, default_cctv_playback_headers, fresh_headers, root_headers, Headers, Identity,
};
use crate::resolver::Resolver;
use crate::state::{AppSession, AppStartResult, ChannelEntry, Live01Result, SharedSession, VdnGetStreamResult};
use crate::util::{
    compact_json_bytes, compute_fingerprint, current_time_ms, extract_guid, form_urlencode_value,
    http_status_text, log, now_f64, parse_result_code, url_host,
};

fn set_header(h: &mut Headers, key: &str, value: &str) {
    if let Some(item) = h.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        item.1 = value.to_string();
    } else {
        h.push((key.to_string(), value.to_string()));
    }
}

fn body_json(resp: &crate::http_client::HttpResponse) -> (u16, String, Option<Value>) {
    response_json(resp)
}

// ---------------------------------------------------------------- 遥测 / 辅助流程

pub async fn collect_report_async(r: &Resolver, profile: &DeviceProfile, body: &Value) -> Result<()> {
    let headers = build_collect_headers(profile);
    let info = String::from_utf8(compact_json_bytes(body, false)).unwrap_or_default();
    let resp = r
        .generic_client
        .post_form(COLLECT_REPORT_URL, &as_pairs(&headers), &[("info", info.as_str())])
        .await?;
    if !(200..300).contains(&resp.status) {
        return Err(YsptpError::new(format!("collect report HTTP {}", http_status_text(resp.status, &resp.reason))));
    }
    Ok(())
}

pub async fn dictionary_obtain(r: &Resolver, version: &str) -> Result<()> {
    let _guard = r.control_slot("dictionary_obtain").await;
    let headers = root_headers(version);
    let resp = r
        .generic_client
        .post_bytes(DICTIONARY_URL, &as_pairs(&headers), Vec::new())
        .await?;
    if !(200..300).contains(&resp.status) {
        return Err(YsptpError::new(format!("dictionary HTTP {}", http_status_text(resp.status, &resp.reason))));
    }
    Ok(())
}

pub async fn index_flow(r: &Resolver, client: &HttpClient, identity: &Identity, app_channel: &str) -> Result<()> {
    let body = json!({ "channel": app_channel, "source": "application" });
    let _guard = r.control_slot("index").await;
    let headers = fresh_headers(&identity.headers, "application/json", Some("application/json"), None);
    let resp = client.post_json(INDEX_URL, &as_pairs(&headers), &body).await?;
    if !(200..300).contains(&resp.status) {
        return Err(YsptpError::new(format!("index HTTP {}", http_status_text(resp.status, &resp.reason))));
    }
    Ok(())
}

pub async fn warmup_flow(r: &Resolver, client: &HttpClient, identity: &Identity, version: &str) -> Result<()> {
    let appcommon = build_vdn_appcommon(version);
    {
        let _guard = r.control_slot("drm_config").await;
        let headers = fresh_headers(&identity.headers, "application/x-www-form-urlencoded", None, None);
        let resp = client
            .post_form(DRM_CONFIG_URL, &as_pairs(&headers), &[("appcommon", appcommon.as_str())])
            .await?;
        if !(200..300).contains(&resp.status) {
            return Err(YsptpError::new(format!("drm config HTTP {}", http_status_text(resp.status, &resp.reason))));
        }
    }
    let url = format!("{VERSION_CONFIG_URL}?appcommon={}", form_urlencode_value(&appcommon));
    let _guard = r.control_slot("version_config").await;
    let mut headers = fresh_headers(&identity.headers, "", None, None);
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case("Content-Type"));
    let resp = client.get(&url, &as_pairs(&headers)).await?;
    if !(200..300).contains(&resp.status) {
        return Err(YsptpError::new(format!("version config HTTP {}", http_status_text(resp.status, &resp.reason))));
    }
    Ok(())
}

pub async fn device_info_report_flow(
    r: &Resolver,
    profile: &DeviceProfile,
    identity: &Identity,
    app_channel: &str,
    version: &str,
    cloud_guid: &str,
) -> Result<()> {
    let _guard = r.control_slot("device_info_report").await;
    let mut value = build_report_common_value(
        profile, &identity.x_uid, app_channel, version, "", current_time_ms(),
    );
    let sdk_int = infer_sdk_int(profile);
    let os = infer_os_version(profile);
    let system_info = if sdk_int.is_empty() {
        os.clone()
    } else if os.is_empty() {
        sdk_int.clone()
    } else {
        format!("{os}/{sdk_int}")
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert("version".into(), Value::String(version.to_string()));
        obj.insert("network_status".into(), Value::String("WiFi".into()));
        obj.insert("device_info".into(), Value::String(format!("{}-{}", profile.brand, profile.model)));
        obj.insert("manufacturer".into(), Value::String(profile.manufacturer.clone()));
        obj.insert("cpu_info".into(), Value::String(String::new()));
        obj.insert("chip_info".into(), Value::String(profile.hardware.clone()));
        obj.insert("ram_info".into(), Value::String(String::new()));
        obj.insert("memory_info".into(), Value::String(String::new()));
        obj.insert("system_info".into(), Value::String(system_info));
        obj.insert("guid".into(), Value::String(cloud_guid.to_string()));
    }
    let body = json!({ "key": "app_device_info", "value": value });
    let headers = fresh_headers(&identity.headers, "application/json", Some("application/json"), None);
    let resp = r.generic_client.post_json(REPORT_SINGLE_URL, &as_pairs(&headers), &body).await?;
    if !(200..300).contains(&resp.status) {
        return Err(YsptpError::new(format!("device info HTTP {}", http_status_text(resp.status, &resp.reason))));
    }
    Ok(())
}

/// 对应 `heartbeat_flow`，返回上报返回码字符串。
pub async fn heartbeat_flow(
    r: &Resolver,
    profile: &DeviceProfile,
    identity: &Identity,
    app_channel: &str,
    version: &str,
    cloud_guid: &str,
) -> Result<String> {
    let _guard = r.control_slot("heartbeat").await;
    let mut value = build_report_common_value(
        profile, &identity.x_uid, app_channel, version, "", current_time_ms(),
    );
    if let Some(obj) = value.as_object_mut() {
        obj.insert("network_type".into(), Value::String("WiFi".into()));
        obj.insert("guid".into(), Value::String(cloud_guid.to_string()));
        obj.insert("other".into(), Value::String(String::new()));
    }
    let body = json!({ "key": "app_heartbeat", "value": value });
    let headers = fresh_headers(&identity.headers, "application/json", Some("application/json"), None);
    let resp = r.generic_client.post_json(REPORT_SINGLE_URL, &as_pairs(&headers), &body).await?;
    let (status, _, value) = body_json(&resp);
    if !(200..300).contains(&status) {
        return Err(YsptpError::new(format!("heartbeat HTTP {status}")));
    }
    Ok(value
        .as_ref()
        .and_then(parse_result_code)
        .map(|c| c.to_string())
        .unwrap_or_default())
}

// ---------------------------------------------------------------- appStart / 云注册

/// 对应 `app_start_flow`：两次重试，用 X-Fingerprint 前 32 字符解出 session_key。
pub async fn app_start_flow(
    r: &Resolver,
    client: &HttpClient,
    profile: &DeviceProfile,
    identity: &mut Identity,
    app_channel: &str,
    version: &str,
) -> Result<AppStartResult> {
    let mut last = String::new();
    for attempt in 1..=2 {
        if attempt > 1 {
            tokio::time::sleep(Duration::from_secs((attempt - 1) as u64)).await;
        }
        let (xfp, ts) = {
            let _guard = r.control_slot("app_start").await;
            let token_time = current_time_ms();
            let (xfp, ts, _) = compute_fingerprint(&identity.x_uid, token_time);
            identity.x_fingerprint = xfp.clone();
            identity.fingerprint_timestamp_ms = ts;
            set_header(&mut identity.headers, "X-Fingerprint", &xfp);
            let body = build_app_start_body(profile, &identity.x_uid, app_channel, version, current_time_ms());
            let payload = compact_json_bytes(&body, true);
            let mut headers = fresh_headers(&identity.headers, "application/json", Some("application/json"), Some(ts));
            set_header(&mut headers, "UID", "");
            let resp = client.post_bytes(APP_START_URL, &as_pairs(&headers), payload).await?;
            let (status, text, value) = body_json(&resp);
            if (200..300).contains(&status) {
                let encrypted = value
                    .as_ref()
                    .and_then(|v| v.get("data"))
                    .map(|data| match data {
                        Value::Object(m) => m.get("key").unwrap_or(data).as_str().unwrap_or("").to_string(),
                        Value::String(s) => s.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                if !encrypted.is_empty() {
                    let key = &xfp[..xfp.len().min(32)];
                    let session_key = aes_gcm_decrypt_b64(&encrypted, key)?;
                    return Ok(AppStartResult { session_key });
                }
            }
            last = format!("app/start HTTP {status}: {}", &text[..text.len().min(300)]);
            (xfp, ts)
        };
        let _ = (xfp, ts);
    }
    Err(YsptpError::new(last))
}

/// 对应 `cloud_registration_flow`：get -> 按需 register -> 再 get。
pub async fn cloud_registration_flow(r: &Resolver, identity: &Identity, cloud_device_id: &str) -> Result<String> {
    let body = json!({
        "device_name": DEFAULT_DEVICE_NAME,
        "device_id": rsa_encrypt_device_id(cloud_device_id)?,
    });
    let get_post = |label: &'static str| {
        let body = body.clone();
        let headers = fresh_headers(&identity.headers, "application/json", Some("application/json"), None);
        async move {
            let _guard = r.control_slot(label).await;
            r.generic_client.post_json(CLOUD_GET_URL, &as_pairs(&headers), &body).await
        }
    };
    let resp = get_post("cloud_get").await?;
    let (_, _, value) = body_json(&resp);
    let result = value.as_ref().and_then(parse_result_code);
    if result == Some(RESULT_OK) {
        return Ok(value.as_ref().map(extract_guid).unwrap_or_default());
    }
    if result != Some(RESULT_NEEDS_REGISTER) && result != Some(RESULT_GET_MISSING_OR_INVALID) {
        return Ok(String::new());
    }
    let mut last_result: Option<i64> = None;
    let mut guid = String::new();
    for _ in 0..2 {
        let headers = fresh_headers(&identity.headers, "application/json", Some("application/json"), None);
        let resp = {
            let _guard = r.control_slot("cloud_register").await;
            r.generic_client.post_json(CLOUD_REGISTER_URL, &as_pairs(&headers), &body).await?
        };
        let (_, _, value) = body_json(&resp);
        last_result = value.as_ref().and_then(parse_result_code);
        guid = value.as_ref().map(extract_guid).unwrap_or_default();
        if !guid.is_empty() || last_result != Some(RESULT_REGISTER_RETRY_LATER) {
            break;
        }
    }
    if !guid.is_empty() {
        return Ok(guid);
    }
    if matches!(last_result, Some(x) if x == RESULT_OK || x == RESULT_REGISTERED_ELSEWHERE || x == RESULT_GET_MISSING_OR_INVALID)
    {
        let resp = get_post("cloud_get_after_register").await?;
        let (_, _, value) = body_json(&resp);
        return Ok(value.as_ref().map(extract_guid).unwrap_or_default());
    }
    Ok(String::new())
}

// ---------------------------------------------------------------- 会话引导

/// 对应 `bootstrap_session`：注册设备、建立会话、心跳、后台遥测。
pub async fn bootstrap_session(
    r: &Resolver,
    generation: i64,
    device_state: Option<DeviceState>,
    save_to_disk: bool,
    _fast: bool,
    device_json: &Path,
) -> Result<SharedSession> {
    let target = device_json;
    let mut state = match device_state {
        Some(s) => s,
        None => {
            let (loaded, existed) = crate::device::load_device_state(target);
            if (!existed || loaded.registered_at <= 0.0) && save_to_disk {
                let fresh = crate::device::default_device_state();
                let _ = crate::device::save_device_state(target, &fresh);
                fresh
            } else {
                loaded
            }
        }
    };
    let profile = state.profile.clone();
    let app_channel = APP_CHANNEL;
    let version = APP_VERSION;
    let mut identity = build_identity(&profile, app_channel, version);
    state.x_uid = identity.x_uid.clone();
    let client = HttpClient::new(r.args.timeout, r.args.insecure_tls)?;
    let app_start = app_start_flow(r, &client, &profile, &mut identity, app_channel, version).await?;
    let session_key = app_start.session_key;
    let cloud_guid = cloud_registration_flow(r, &identity, &identity.x_uid).await?;
    let cloud_guid = if cloud_guid.is_empty() && !state.cloud_guid.is_empty() {
        state.cloud_guid.clone()
    } else {
        cloud_guid
    };
    if cloud_guid.is_empty() {
        return Err(YsptpError::new("cloud device registration failed: empty guid, fallback to 1080p"));
    }
    let now = now_f64();
    state.x_uid = identity.x_uid.clone();
    state.updated_at = now;
    state.cloud_guid = cloud_guid.clone();
    if state.registered_at <= 0.0 {
        state.registered_at = now;
    }
    if save_to_disk {
        let _ = crate::device::save_device_state(target, &state);
    }
    let hb = heartbeat_flow(r, &profile, &identity, app_channel, version, &cloud_guid).await?;
    let identity_clone = identity.clone();
    let profile_clone = profile.clone();
    let guid_clone = cloud_guid.clone();
    let telemetry_client = client.clone();
    if let Some(arc_r) = r.app.resolver.read().as_ref().cloned() {
        tokio::spawn(async move {
            let rr = arc_r.as_ref();
            let common = build_report_common_value(
                &profile_clone, &identity_clone.x_uid, app_channel, version,
                COLLECT_SDK_VERSION, current_time_ms(),
            );
            let _ = collect_report_async(rr, &profile_clone, &json!({
                "key": "app_start_d1", "value": common
            })).await;
            let _ = dictionary_obtain(rr, version).await;
            let _ = device_info_report_flow(rr, &profile_clone, &identity_clone, app_channel, version, &guid_clone).await;
            let _ = index_flow(rr, &telemetry_client, &identity_clone, app_channel).await;
            let _ = warmup_flow(rr, &telemetry_client, &identity_clone, version).await;
        });
    }
    Ok(std::sync::Arc::new(parking_lot::Mutex::new(AppSession {
        profile,
        identity,
        client,
        session_key,
        cloud_guid,
        version: version.to_string(),
        screen_param: state.screen_param.clone(),
        cast_model: state.cast_model.clone(),
        created_at: now_f64(),
        generation,
        last_heartbeat_at: now_f64(),
        heartbeat_count: 1,
        last_heartbeat_result: hb,
        last_heartbeat_error: String::new(),
        linked_channels: std::collections::HashSet::new(),
    })))
}

/// 对应 `ensure_slot_session(slot)`：会话新鲜度 + TTL 守卫 + 串行重建。
pub async fn ensure_slot_session(r: &Resolver, slot: i32) -> Result<SharedSession> {
    {
        let st = r.state.lock();
        if let Some(sess) = st.sessions.get(&slot) {
            let created = sess.lock().created_at;
            if r.session_fresh(created) {
                let rem = r.session_ttl_remaining(created);
                if rem.map(|x| x > 300.0).unwrap_or(true) {
                    return Ok(sess.clone());
                }
            }
        }
    }
    let _g = r.control.lock().await;
    {
        let st = r.state.lock();
        if let Some(sess) = st.sessions.get(&slot) {
            let created = sess.lock().created_at;
            if r.session_fresh(created) {
                let rem = r.session_ttl_remaining(created);
                if rem.map(|x| x > 300.0).unwrap_or(true) {
                    return Ok(sess.clone());
                }
            }
        }
    }
    let gen = {
        let mut st = r.state.lock();
        st.session_generation += 1;
        st.session_generation
    };
    let target = r.slot_device_json_path(slot);
    drop(_g);
    let sess = bootstrap_session(r, gen, None, true, true, &target).await?;
    let _g2 = r.control.lock().await;
    let mut st = r.state.lock();
    let cur = st.sessions.get(&slot).cloned();
    let replace = match cur {
        None => true,
        Some(ref c) => {
            let cg = c.lock().generation;
            !r.session_fresh(c.lock().created_at) || cg < gen
        }
    };
    if replace {
        st.sessions.insert(slot, sess.clone());
        Ok(sess)
    } else {
        Ok(cur.unwrap())
    }
}

// ---------------------------------------------------------------- 取流

/// 对应 `live_v1_01_flow`：拿到 36p 优先的 live_url（可能需 AES-GCM 解密）。
pub async fn live_v1_01_flow(r: &Resolver, sess: &SharedSession, live_id: &str) -> Result<Live01Result> {
    let (client, identity, session_key, screen_param, cast_model) = {
        let s = sess.lock();
        (s.client.clone(), s.identity.clone(), s.session_key.clone(), s.screen_param.clone(), s.cast_model.clone())
    };
    let body = json!({
        "screenParam": screen_param, "rate": "", "systemType": "ios",
        "model": cast_model, "id": live_id, "userId": DEFAULT_LIVE_USER_ID,
        "clientSign": "cctvVideo",
        "deviceId": { "serial": "", "imei": "", "android_id": "" },
    });
    let headers = {
        let _guard = r.control_slot("live_v1_01").await;
        fresh_headers(&identity.headers, "application/json", Some("application/json"), None)
    };
    let resp = client.post_json(LIVE_V1_01_URL, &as_pairs(&headers), &body).await?;
    let (status, text, value) = body_json(&resp);
    if !(200..300).contains(&status) {
        return Err(YsptpError::new(format!("live/v1/01 HTTP {status}: {}", &text[..text.len().min(300)])));
    }
    let videos = value
        .as_ref()
        .and_then(|v| v.get("data"))
        .and_then(|d| d.get("videoList").or_else(|| d.get("videos")))
        .and_then(|v| v.as_array());
    let videos = match videos {
        Some(v) => v,
        None => return Err(YsptpError::new("live/v1/01 missing videos")),
    };
    let mut selected: Option<&Value> = None;
    let mut fallback: Option<&Value> = None;
    for item in videos {
        let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(item);
        }
        if item.get("rate").and_then(|v| v.as_str()) == Some("36p") {
            selected = Some(item);
            break;
        }
    }
    let video = selected.or(fallback).ok_or_else(|| YsptpError::new("live/v1/01 no usable URL"))?;
    let raw_url = video.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let live_url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
        raw_url.to_string()
    } else {
        aes_gcm_decrypt_b64(raw_url, &session_key)?
    };
    Ok(Live01Result {
        live_url,
        rate: video.get("rate").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        rate_name: video.get("rateName").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    })
}

/// 对应 `live_v1_02_flow`：换取 appSecret。
pub async fn live_v1_02_flow(r: &Resolver, sess: &SharedSession) -> Result<String> {
    let (client, identity, session_key) = {
        let s = sess.lock();
        (s.client.clone(), s.identity.clone(), s.session_key.clone())
    };
    let encrypted_guid = aes_gcm_encrypt_b64("", &session_key)?;
    let body = json!({ "guid": encrypted_guid });
    let headers = {
        let _guard = r.control_slot("live_v1_02").await;
        fresh_headers(&identity.headers, "application/json", Some("application/json"), None)
    };
    let resp = client.post_json(LIVE_V1_02_URL, &as_pairs(&headers), &body).await?;
    let (status, text, value) = body_json(&resp);
    if !(200..300).contains(&status) {
        return Err(YsptpError::new(format!("live/v1/02 HTTP {status}: {}", &text[..text.len().min(300)])));
    }
    let encrypted = value
        .as_ref()
        .and_then(|v| v.get("data"))
        .map(|d| match d {
            Value::Object(m) => m.get("appSecret").or_else(|| m.get("app_secret"))
                .and_then(|v| v.as_str()).unwrap_or("").to_string(),
            Value::String(s) => s.clone(),
            _ => String::new(),
        })
        .unwrap_or_default();
    if encrypted.is_empty() {
        return Err(YsptpError::new("live/v1/02 missing appSecret"));
    }
    aes_gcm_decrypt_b64(&encrypted, &session_key)
}

/// 对应 `vdn_getstream_flow`：MD5 签名换取最终播放地址。
pub async fn vdn_getstream_flow(
    r: &Resolver,
    identity: &Identity,
    version: &str,
    live_url: &str,
    app_secret: &str,
) -> Result<VdnGetStreamResult> {
    let (app_sign, random_str) = compute_vdn_code(app_secret, None);
    let mut headers = {
        let _guard = r.control_slot("vdn_getstream").await;
        fresh_headers(&identity.headers, "application/x-www-form-urlencoded", None, None)
    };
    set_header(&mut headers, "APPID", AK);
    set_header(&mut headers, "APPSIGN", &app_sign);
    set_header(&mut headers, "APPRANDOMSTR", &random_str);
    let appcommon = build_vdn_appcommon(version);
    let resp = r
        .generic_client
        .post_form(VDN_GETSTREAM_URL, &as_pairs(&headers), &[("appcommon", appcommon.as_str()), ("url", live_url)])
        .await?;
    let (status, text, value) = body_json(&resp);
    if !(200..300).contains(&status) {
        return Err(YsptpError::new(format!("VDN HTTP {status}: {}", &text[..text.len().min(300)])));
    }
    if let Some(v) = &value {
        let succeed = v.get("succeed");
        let succeed_str = match succeed {
            Some(Value::String(s)) => s.trim_matches('"').to_string(),
            Some(other) => serde_json::to_string(other).unwrap_or_default().trim_matches('"').to_string(),
            None => String::new(),
        };
        if succeed_str == "1" {
            if let Some(url) = v.get("url").and_then(|u| u.as_str()) {
                return Ok(VdnGetStreamResult {
                    final_url: url.to_string(),
                    app_sign,
                    app_random_str: random_str,
                });
            }
        }
    }
    Err(YsptpError::new(format!("VDN did not return final URL: {}", &text[..text.len().min(400)])))
}

/// 对应 `resolve_channel_once`。
pub async fn resolve_channel_once(
    r: &Resolver,
    sess: &SharedSession,
    channel: &str,
    live_id: &str,
    generation: i64,
) -> Result<ChannelEntry> {
    let live01 = live_v1_01_flow(r, sess, live_id).await?;
    let app_secret = live_v1_02_flow(r, sess).await?;
    let (identity, version) = {
        let s = sess.lock();
        (s.identity.clone(), s.version.clone())
    };
    let vdn = vdn_getstream_flow(r, &identity, &version, &live01.live_url, &app_secret).await?;
    let (android_id, x_uid, session_gen) = {
        let mut s = sess.lock();
        s.linked_channels.insert(channel.to_string());
        (s.profile.android_id.clone(), s.identity.x_uid.clone(), s.generation)
    };
    let mut playback_headers = default_cctv_playback_headers(&android_id);
    playback_headers.insert("APPRANDOMSTR".into(), vdn.app_random_str.clone());
    playback_headers.insert("APPSIGN".into(), vdn.app_sign.clone());
    let mut display_rate_name = if live01.rate_name.is_empty() {
        "高码率".to_string()
    } else {
        live01.rate_name.clone()
    };
    if matches!(channel, "cctv4k" | "cctv164k" | "cctv8k") {
        if display_rate_name.contains("8K") || matches!(display_rate_name.as_str(), "8K高清" | "8K超高清") {
            display_rate_name = "4K超高清".to_string();
        }
    } else if matches!(display_rate_name.as_str(), "8K高清" | "4K高清" | "8K超高清" | "4K超高清") {
        display_rate_name = "高码率".to_string();
    }
    let now = now_f64();
    Ok(ChannelEntry {
        channel: channel.to_string(),
        live_id: live_id.to_string(),
        final_url: vdn.final_url.clone(),
        playback_headers,
        android_id,
        x_uid,
        rate: live01.rate.clone(),
        rate_name: display_rate_name,
        raw_live_host: url_host(&live01.live_url),
        final_host: url_host(&vdn.final_url),
        refreshed_at: now,
        expires_at: now + r.args.cache_ttl,
        generation,
        last_refresh_error: String::new(),
        last_refresh_failed_at: 0.0,
        session_generation: session_gen,
    })
}

/// 对应 `refresh_channel_controlled`：单设备配额 + 全流程解析 + 缓存写入。
pub async fn refresh_channel_controlled(r: &Resolver, channel: &str) -> Result<ChannelEntry> {
    let wait = {
        let st = r.state.lock();
        r.args.refresh_interval - (now_f64() - st.last_business_end_at)
    };
    if wait > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
    }
    let generation = {
        let mut st = r.state.lock();
        st.refreshing_channel = channel.to_string();
        st.generation += 1;
        st.generation
    };
    log(format!("[{channel}] 正在向央视频协议引擎请求直播源…"), 10.0);
    let result: Result<ChannelEntry> = async {
        let slot = 0; // Python get_slot_for_channel 恒返回 0
        let limit = r.args.links_per_device;
        let sess = ensure_slot_session(r, slot).await?;
        if limit > 0 {
            let (has_link, linked) = {
                let s = sess.lock();
                (s.linked_channels.contains(channel), s.linked_channels.len())
            };
            if !has_link && linked >= limit {
                r.evict_idle_links(&sess, LINK_IDLE_TIMEOUT);
                if sess.lock().linked_channels.len() >= limit {
                    return Err(YsptpError::new(format!("device link quota reached ({limit}), fallback to 1080p")));
                }
            }
        }
        let live_id = crate::channels::channel_by_name(channel)
            .ok_or_else(|| YsptpError::new(format!("unknown channel {channel}")))?;
        resolve_channel_once(r, &sess, channel, &live_id, generation).await
    }
    .await;
    let entry = match result {
        Ok(e) => e,
        Err(e) => {
            let mut st = r.state.lock();
            st.last_business_end_at = now_f64();
            st.refreshing_channel.clear();
            return Err(e);
        }
    };
    {
        let mut st = r.state.lock();
        st.last_business_end_at = now_f64();
        st.refreshing_channel.clear();
        st.cache.insert(channel.to_string(), entry.clone());
        st.playlist_cache.remove(channel);
        st.channel_failures.remove(channel);
        st.identity_reset_error_count = 0;
        st.last_error.clear();
    }
    let rate_label = if matches!(channel, "cctv4k" | "cctv164k" | "cctv8k") {
        if entry.rate == "36p" { "4K超高清 (36M超高码率)" }
        else if entry.rate == "10p" { "4K (10M码率)" }
        else { "4K超高清" }
    } else if entry.rate == "10p" {
        "高码率 (10M码率)"
    } else if entry.rate == "36p" {
        "超高码率 (36M码率)"
    } else if !entry.rate_name.is_empty() {
        entry.rate_name.as_str()
    } else if !entry.rate.is_empty() {
        entry.rate.as_str()
    } else {
        "高码率"
    };
    log(format!("[{channel}] 直播源获取成功 (清晰度: {rate_label})"), 10.0);
    Ok(entry)
}

/// 对应 `send_heartbeat`：串行心跳 + 会话续期 + 空闲配额回收。
pub async fn send_heartbeat(r: &Resolver) -> Result<()> {
    let Ok(_g) = r.control.try_lock() else { return Ok(()) };
    let slots: Vec<i32> = { r.state.lock().sessions.keys().copied().collect() };
    if slots.is_empty() {
        return Ok(());
    }
    for slot in slots {
        let sess = { r.state.lock().sessions.get(&slot).cloned() };
        let Some(sess) = sess else { continue };
        r.evict_idle_links(&sess, LINK_IDLE_TIMEOUT);
        let (created, rem_ttl, profile, identity, version, cloud_guid) = {
            let s = sess.lock();
            (s.created_at, r.session_ttl_remaining(s.created_at), s.profile.clone(),
             s.identity.clone(), s.version.clone(), s.cloud_guid.clone())
        };
        if !r.session_fresh(created) || rem_ttl.map(|x| x <= 300.0).unwrap_or(false) {
            let _ = ensure_slot_session(r, slot).await;
            continue;
        }
        let wait = {
            let st = r.state.lock();
            r.args.refresh_interval - (now_f64() - st.last_business_end_at)
        };
        if wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        }
        match heartbeat_flow(r, &profile, &identity, APP_CHANNEL, &version, &cloud_guid).await {
            Ok(code) => {
                let mut st = r.state.lock();
                st.last_business_end_at = now_f64();
                let mut s = sess.lock();
                s.last_heartbeat_at = now_f64();
                s.heartbeat_count += 1;
                s.last_heartbeat_result = code;
                s.last_heartbeat_error.clear();
            }
            Err(e) => {
                let mut st = r.state.lock();
                if let Some(cur) = st.sessions.get(&slot) {
                    cur.lock().last_heartbeat_error = e.message().to_string();
                }
                st.last_error = format!("heartbeat failed on slot {slot}: {}", e.message());
                st.last_error_at = now_f64();
            }
        }
    }
    r.app.set_ready(true);
    Ok(())
}
