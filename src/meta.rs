use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::channels_data::BACKEND_LIVE_IDS;
use crate::device::load_device_state;
use crate::resolver::Resolver;
use crate::state::ChannelEntry;
use crate::util::{now_f64, rust_round};

pub fn load_cache(path: &Path) -> HashMap<String, ChannelEntry> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let Some(detail) = value.get("cache_detail").and_then(|v| v.as_object()) else {
        return HashMap::new();
    };
    detail
        .iter()
        .filter(|(_, v)| v.is_object())
        .map(|(k, v)| (k.clone(), ChannelEntry::from_value(v)))
        .collect()
}

pub fn status_json(r: &Resolver) -> Value {
    let now = now_f64();
    let snapshot = {
        let st = r.state.lock();
        (
            st.cache.clone(),
            st.playlist_cache.clone(),
            st.background_refreshing.clone(),
            st.refreshing_channel.clone(),
            st.last_business_end_at,
            st.last_error.clone(),
            st.identity_reset_error_count,
            st.identity_reset_count,
            st.last_identity_reset_at,
            st.last_identity_reset_reason.clone(),
            st.ad_display,
            st.generation,
            st.session_generation,
            st.sessions.get(&0).cloned(),
            st.refresh_queue.queued.len(),
            st.refresh_queue.running_channel.clone(),
            st.refresh_queue.running_started_at,
            st.refresh_queue.enqueued_total,
            st.refresh_queue.dropped_total,
            st.refresh_queue.completed_total,
            st.refresh_queue.failed_total,
            st.refresh_queue.last_enqueued_at,
            st.refresh_queue.last_finished_at,
            st.refresh_queue.last_dropped_at,
            st.refresh_queue.last_drop_reason.clone(),
            st.http_queue.queued.len(),
            st.http_queue.active_workers,
            st.http_queue.enqueued_total,
            st.http_queue.rejected_total,
            st.http_queue.completed_total,
            st.http_queue.panicked_total,
            st.http_queue.last_enqueued_at,
            st.http_queue.last_completed_at,
            st.http_queue.last_rejected_at,
        )
    };
    let (
        cache, playlist_cache, background_refreshing, refreshing_channel, last_business_end_at,
        last_error, identity_reset_error_count, identity_reset_count, last_identity_reset_at,
        last_identity_reset_reason, ad_display, generation, session_generation, app_session,
        queued_depth, running_channel, running_started_at, enqueued_total, dropped_total,
        completed_total, failed_total, last_enqueued_at, last_finished_at, last_dropped_at,
        last_drop_reason, http_queued_depth, http_active_workers, http_enqueued_total,
        http_rejected_total, http_completed_total, http_panicked_total, http_last_enqueued_at,
        http_last_completed_at, http_last_rejected_at,
    ) = snapshot;

    let (loaded, existed) = load_device_state(&r.args.device_json);
    let device_state = if existed {
        json!({
            "exists": true,
            "android_id": loaded.profile.android_id,
            "mac": loaded.profile.mac,
            "brand": loaded.profile.brand,
            "model": loaded.profile.model,
            "screen_param": loaded.screen_param,
            "x_uid": loaded.x_uid,
            "cloud_guid_present": !loaded.cloud_guid.is_empty(),
            "registered_at": loaded.registered_at,
            "updated_at": loaded.updated_at,
        })
    } else {
        json!({ "exists": false })
    };

    let cache_json: Value = cache.iter().map(|(k, v)| {
        let age = rust_round((now - v.refreshed_at) * 1000.0) / 1000.0;
        let exp = rust_round((v.expires_at - now) * 1000.0) / 1000.0;
        (k.clone(), json!({
            "live_id": v.live_id, "fresh": v.fresh(now),
            "age_seconds": age, "expires_in_seconds": exp,
            "rate": v.rate, "rate_name": v.rate_name, "final_host": v.final_host,
            "generation": v.generation, "session_generation": v.session_generation,
            "last_refresh_error": v.last_refresh_error,
        }))
    }).collect::<serde_json::Map<String, Value>>().into();

    let playlist_cache_json: Value = playlist_cache.iter().map(|(k, v)| {
        let age = rust_round((now - v.cached_at) * 1000.0) / 1000.0;
        let exp = rust_round((v.expires_at - now) * 1000.0) / 1000.0;
        (k.clone(), json!({
            "fresh": now < v.expires_at, "age_seconds": age, "expires_in_seconds": exp,
            "bytes": v.body.len(), "content_type": v.content_type,
            "proxy_origin": v.proxy_origin, "proxy_prefix": v.proxy_prefix,
        }))
    }).collect::<serde_json::Map<String, Value>>().into();

    let session_json = if let Some(sess) = &app_session {
        let s = sess.lock();
        let expires_in = if r.args.session_ttl > 0.0 {
            Some(r.args.session_ttl - (now - s.created_at))
        } else { None };
        json!({
            "active": true, "generation": s.generation,
            "age_seconds": rust_round((now - s.created_at) * 1000.0) / 1000.0,
            "fresh": r.session_fresh(s.created_at), "expires_in_seconds": expires_in,
            "android_id": s.profile.android_id, "mac": s.profile.mac,
            "brand": s.profile.brand, "model": s.profile.model,
            "screen_param": s.screen_param, "cast_model": s.cast_model,
            "x_uid": s.identity.x_uid, "cloud_guid_present": !s.cloud_guid.is_empty(),
            "fingerprint_timestamp_ms": s.identity.fingerprint_timestamp_ms,
            "heartbeat_count": s.heartbeat_count,
            "last_heartbeat_age_seconds": rust_round((now - s.last_heartbeat_at) * 1000.0) / 1000.0,
            "last_heartbeat_result": s.last_heartbeat_result,
            "last_heartbeat_error": s.last_heartbeat_error,
        })
    } else {
        json!({ "active": false, "generation": session_generation })
    };

    let age_or_none = |t: f64| -> Value {
        if t > 0.0 { Value::from(rust_round((now - t) * 1000.0) / 1000.0) } else { Value::Null }
    };

    json!({
        "channels": BACKEND_LIVE_IDS.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        "cache_ttl_seconds": r.args.cache_ttl,
        "stale_while_refresh_ttl_seconds": r.args.stale_while_refresh_ttl,
        "refresh_error_cooldown_seconds": r.args.refresh_error_cooldown,
        "playlist_cache_ttl_seconds": r.args.playlist_cache_ttl,
        "background_refresh_queue_limit": r.args.background_refresh_queue_limit,
        "http_workers": r.args.http_workers.max(1),
        "http_queue_limit": r.args.http_queue_limit,
        "identity_reset_error_threshold": r.args.identity_reset_error_threshold,
        "identity_reset_cooldown_seconds": r.args.identity_reset_cooldown,
        "refresh_interval_seconds": r.args.refresh_interval,
        "control_step_jitter_min_ms": r.args.control_step_jitter_min_ms,
        "control_step_jitter_max_ms": r.args.control_step_jitter_max_ms,
        "heartbeat_interval_seconds": r.args.heartbeat_interval,
        "heartbeat_ttl_guard_seconds": r.args.heartbeat_ttl_guard,
        "session_ttl_seconds": r.args.session_ttl,
        "meta_json": r.args.meta_json.to_string_lossy(),
        "device_json": r.args.device_json.to_string_lossy(),
        "device_state": device_state,
        "refreshing_channel": refreshing_channel,
        "background_refreshing": background_refreshing.keys().collect::<Vec<_>>(),
        "background_refresh_queue": {
            "limit": r.args.background_refresh_queue_limit,
            "queued_depth": queued_depth,
            "running_channel": running_channel,
            "running_age_seconds": age_or_none(running_started_at),
            "enqueued_total": enqueued_total,
            "dropped_total": dropped_total,
            "completed_total": completed_total,
            "failed_total": failed_total,
            "last_enqueued_age_seconds": age_or_none(last_enqueued_at),
            "last_finished_age_seconds": age_or_none(last_finished_at),
            "last_dropped_age_seconds": age_or_none(last_dropped_at),
            "last_drop_reason": last_drop_reason,
        },
        "http_queue": {
            "workers": r.args.http_workers.max(1),
            "limit": r.args.http_queue_limit,
            "queued_depth": http_queued_depth,
            "active_workers": http_active_workers,
            "enqueued_total": http_enqueued_total,
            "rejected_total": http_rejected_total,
            "completed_total": http_completed_total,
            "panicked_total": http_panicked_total,
            "last_enqueued_age_seconds": age_or_none(http_last_enqueued_at),
            "last_completed_age_seconds": age_or_none(http_last_completed_at),
            "last_rejected_age_seconds": age_or_none(http_last_rejected_at),
        },
        "generation": generation,
        "app_session": session_json,
        "last_business_end_at": last_business_end_at,
        "last_error": last_error,
        "identity_reset_error_count": identity_reset_error_count,
        "identity_reset_count": identity_reset_count,
        "last_identity_reset_at": last_identity_reset_at,
        "last_identity_reset_reason": last_identity_reset_reason,
        "ad_display": ad_display,
        "device_profile_pool_size": crate::device_pool::DEVICE_PROFILE_POOL.len(),
        "cache": cache_json,
        "playlist_cache": playlist_cache_json,
    })
}

pub fn write_meta(r: &Resolver) -> std::io::Result<()> {
    let mut payload = status_json(r);
    let detail: serde_json::Map<String, Value> = r.cache_detail()
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("cache_detail".into(), Value::Object(detail));
        obj.insert(
            "session_detail_note".into(),
            Value::String("Session secrets are intentionally not persisted for reuse; Rust process keeps live session in memory only.".into()),
        );
        obj.insert(
            "device_state_note".into(),
            Value::String("Randomized TV device identity is persisted in device-state-rs.json after cloud registration succeeds; a new identity reset creates a new profile and MAC.".into()),
        );
    }
    let content = crate::device::json_sorted_pretty(&payload);
    crate::device::atomic_write(&r.args.meta_json, &content)
}
