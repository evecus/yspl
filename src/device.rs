use std::fs;
use std::path::Path;

use rand::seq::SliceRandom as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::device_pool::{DeviceTemplate, DEVICE_PROFILE_POOL};
use crate::util::{
    java_string_hashcode, java_uuid_from_hashes, now_f64, random_hex_string, random_mac_address,
    resolution_from_screen_param, sanitize_profile_id, sha1_upper,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceProfile {
    pub android_id: String,
    pub mac: String,
    pub hardware: String,
    pub board: String,
    pub brand: String,
    pub device: String,
    pub manufacturer: String,
    pub model: String,
    pub product: String,
    pub tags: String,
    pub build_type: String,
    pub user: String,
    pub resolution: String,
    pub display: String,
    pub version_id: String,
    pub host: String,
    pub fingerprint: String,
    pub report_model: String,
}

fn str_field(v: &Value, key: &str) -> String {
    match v.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

impl DeviceProfile {

    pub fn from_value(v: &Value) -> Self {
        Self {
            android_id: str_field(v, "android_id"),
            mac: str_field(v, "mac"),
            hardware: str_field(v, "hardware"),
            board: str_field(v, "board"),
            brand: str_field(v, "brand"),
            device: str_field(v, "device"),
            manufacturer: str_field(v, "manufacturer"),
            model: str_field(v, "model"),
            product: str_field(v, "product"),
            tags: str_field(v, "tags"),
            build_type: str_field(v, "build_type"),
            user: str_field(v, "user"),
            resolution: str_field(v, "resolution"),
            display: str_field(v, "display"),
            version_id: str_field(v, "version_id"),
            host: str_field(v, "host"),
            fingerprint: str_field(v, "fingerprint"),
            report_model: str_field(v, "report_model"),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceState {
    pub schema_version: i64,
    pub profile_source: String,
    pub profile: DeviceProfile,
    pub screen_param: String,
    pub cast_model: String,
    pub x_uid: String,
    pub cloud_guid: String,
    pub registered_at: f64,
    pub updated_at: f64,
}

pub fn random_device_template() -> &'static DeviceTemplate {
    let pool8k: Vec<&'static DeviceTemplate> = DEVICE_PROFILE_POOL
        .iter()
        .filter(|t| t.source.contains("8k"))
        .collect();
    let pool: Vec<&'static DeviceTemplate> = if pool8k.is_empty() {
        DEVICE_PROFILE_POOL.iter().collect()
    } else {
        pool8k
    };
    pool.choose(&mut rand::thread_rng()).copied().unwrap_or(&DEVICE_PROFILE_POOL[0])
}

pub fn device_profile_from_template(t: &DeviceTemplate, android_id: &str, mac: &str) -> DeviceProfile {
    let brand_id = sanitize_profile_id(t.brand);
    let model_id = sanitize_profile_id(t.model);
    let device = format!("{brand_id}_{model_id}");
    let product = device.clone();
    DeviceProfile {
        android_id: android_id.to_string(),
        mac: mac.to_string(),
        hardware: t.hardware.to_string(),
        board: t.board.to_string(),
        brand: t.brand.to_string(),
        device: device.clone(),
        manufacturer: t.manufacturer.to_string(),
        model: t.model.to_string(),
        product: product.clone(),
        tags: "release-keys".to_string(),
        build_type: "user".to_string(),
        user: "build".to_string(),
        resolution: resolution_from_screen_param(t.screen_param),
        display: format!("{}-user 13 {} 2024 release-keys", t.model, t.version_id),
        version_id: t.version_id.to_string(),
        host: format!("{brand_id}-tv-build"),
        fingerprint: format!(
            "{}/{}/{}:13/{}/2024:user/release-keys",
            t.manufacturer, product, device, t.version_id
        ),
        report_model: t.report_model.to_string(),
    }
}

pub fn compute_x_uid(p: &DeviceProfile) -> String {
    let build_identity_str = format!(
        "1698{}{}{}{}{}{}{}{}{}{}{}{}",
        p.hardware, p.board, p.brand, p.device, p.manufacturer, p.model,
        p.product, p.tags, p.build_type, p.user, p.resolution, p.mac
    );

    let uuid_part = java_uuid_from_hashes(
        java_string_hashcode(&build_identity_str),
        java_string_hashcode(&p.model),
    );
    sha1_upper(&format!("{}|{}", p.android_id, uuid_part))
}

pub fn infer_os_version(p: &DeviceProfile) -> String {
    if let Some((_, tail)) = p.fingerprint.split_once(':') {
        if let Some((v, _)) = tail.split_once('/') {
            return v.to_string();
        }
    }
    String::new()
}

pub fn infer_sdk_int(p: &DeviceProfile) -> String {
    let major = infer_os_version(p).split('.').next().unwrap_or("").to_string();
    match major.as_str() {
        "13" => "33", "12" => "31", "11" => "30", "10" => "29",
        "9" => "28", "8" => "26", "7" => "24", "6" => "23", _ => "",
    }
    .to_string()
}

pub fn report_model_from_build(p: &DeviceProfile) -> String {
    p.model.replace(&p.manufacturer, "").replace(' ', "")
}

pub fn default_device_state() -> DeviceState {
    let t = random_device_template();
    let profile = device_profile_from_template(t, &random_hex_string(16), &random_mac_address());
    DeviceState {
        schema_version: 1,
        profile_source: t.source.to_string(),
        profile: profile.clone(),
        screen_param: t.screen_param.to_string(),
        cast_model: t.cast_model.to_string(),
        x_uid: compute_x_uid(&profile),
        cloud_guid: String::new(),
        registered_at: 0.0,
        updated_at: now_f64(),
    }
}

pub fn create_and_save_new_device(path: &Path) -> DeviceState {
    let mut state = default_device_state();
    state.updated_at = now_f64();
    let _ = save_device_state(path, &state);
    state
}

pub fn binary_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

pub(crate) fn json_sorted_pretty<T: Serialize>(v: &T) -> String {

    let value = serde_json::to_value(v).unwrap_or(Value::Null);
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

pub(crate) fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {

    let tmp = std::path::PathBuf::from(format!("{}.tmp.{}", path.display(), std::process::id()));
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    {
        use std::io::Write as _;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        f.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub fn save_device_state(path: &Path, state: &DeviceState) -> std::io::Result<()> {
    let content = json_sorted_pretty(state);
    atomic_write(path, &content)
}

pub fn load_device_state(path: &Path) -> (DeviceState, bool) {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (default_device_state(), false),
    };
    let data: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return (default_device_state(), false),
    };
    let num = |k: &str, d: f64| -> f64 { data.get(k).and_then(|v| v.as_f64()).unwrap_or(d) };
    let profile_val = data.get("profile").cloned().unwrap_or(Value::Null);
    let profile = DeviceProfile::from_value(&profile_val);
    let mut state = DeviceState {
        schema_version: data.get("schema_version").and_then(|v| v.as_i64()).unwrap_or(1),
        profile_source: str_field(&data, "profile_source"),
        profile: profile.clone(),
        screen_param: str_field(&data, "screen_param"),
        cast_model: str_field(&data, "cast_model"),
        x_uid: str_field(&data, "x_uid"),
        cloud_guid: str_field(&data, "cloud_guid"),
        registered_at: num("registered_at", 0.0),
        updated_at: num("updated_at", 0.0),
    };
    if state.profile.android_id.len() != 16 || state.profile.mac.is_empty() {
        return (default_device_state(), false);
    }
    state.x_uid = compute_x_uid(&state.profile);
    if state.screen_param.is_empty() {
        state.screen_param = "7680-4320-280".to_string();
    }
    if state.cast_model.is_empty() {
        state.cast_model = state.profile.model.clone();
    }
    (state, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_uid_is_upper_sha1_of_40_chars() {
        let s = default_device_state();
        assert_eq!(s.x_uid.len(), 40);
        assert_eq!(s.x_uid, s.x_uid.to_uppercase());
        assert_eq!(s.x_uid, compute_x_uid(&s.profile));
    }

    #[test]
    fn device_pool_8k_only() {
        for _ in 0..20 {
            assert!(random_device_template().source.contains("8k"));
        }
    }

    #[test]
    fn fingerprint_fields_are_derived() {
        let s = default_device_state();
        assert_eq!(infer_os_version(&s.profile), "13");
        assert_eq!(infer_sdk_int(&s.profile), "33");
    }

    #[test]
    fn persistence_roundtrip_and_sort_keys() {
        let dir = std::env::temp_dir().join(format!("ysp-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("device-state-rs.json");
        let state = default_device_state();
        save_device_state(&path, &state).unwrap();
        let raw = fs::read_to_string(&path).unwrap();

        assert!(raw.starts_with("{\n  \"cast_model\""), "got: {raw}");
        let (loaded, existed) = load_device_state(&path);
        assert!(existed);
        assert_eq!(loaded.profile.android_id, state.profile.android_id);
        assert_eq!(loaded.x_uid, state.x_uid);
        let _ = fs::remove_dir_all(&dir);
    }
}
