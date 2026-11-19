pub const VERSION: &str = "0.1.0";

pub const AK: &str = "9f5c54c4ed0e50109b800f7e28fec205";
pub const RSA_PUBLIC_KEY_B64: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAkKeLy4ywWLSnBkwRyqYgF3HMIj05V5uuh5HjyEsZOWnu1NHu3jPQv3sr32wwQNYv5qapsNXmNgLUDHtgHZxqPQAYXltjSRc0qhcD286t62wOIHId8zXS3s1Jy4rgU4qjQWzI9rp/1sE0pMsmwTaJa4zuJ5iz8VwF8Av5oJ1k+HxY+/HLnjNlW1hmWLpuDYmkZYuAoTHa1VGeHQh9FEKI8ZcL3GTQphShUoC+Kg3P1hGUVTtCYapmzPS5lkAdwebuzwvTCfGiTErYZCnPBUSeV7BVlgjtLYIi29KvF0a8FHsJMfe/UdHcyW/RihsIYOtDQcRRpFGXyPXbVrzFJse24QIDAQAB";

pub const CLOUD_GET_URL: &str = "https://ytpcloudws.cctv.cn/cloudps/wssapi/device/v2/get";
pub const CLOUD_REGISTER_URL: &str = "https://ytpcloudws.cctv.cn/cloudps/wssapi/device/v2/register";
pub const APP_START_URL: &str = "https://ytpaddr.cctv.cn/gsnw/api/app/start/v1/01";
pub const DRM_CONFIG_URL: &str = "https://ytpaddr.cctv.cn/gsnw/drm/config/obtain/v1";
pub const VERSION_CONFIG_URL: &str = "https://ytpaddr.cctv.cn/gsnw/version/config/obtain/v1";
pub const DICTIONARY_URL: &str = "https://ytpaddr.cctv.cn/gsnw/player/dictionary/obtain/v1";
pub const INDEX_URL: &str = "https://ytpaddr.cctv.cn/gsnw/api/index/v1/01";
pub const REPORT_SINGLE_URL: &str = "https://ytpdata.cctv.cn/das/app/data/message/single";
pub const COLLECT_REPORT_URL: &str = "https://collect.cctv.cn/cctvmobileinf/rest/cctv/receive/new/app";
pub const LIVE_V1_01_URL: &str = "https://ytpaddr.cctv.cn/gsnw/api/live/v1/01";
pub const LIVE_V1_02_URL: &str = "https://ytpaddr.cctv.cn/gsnw/api/live/v1/02";
pub const VDN_GETSTREAM_URL: &str = "https://ytpvdn.cctv.cn/cctvmobileinf/rest/cctv/videoliveUrl/getstream";
pub const JACC_URL: &str = "https://jacc.ysp.cctv.cn";
pub const BK_LIVEINFO_URL: &str = "https://bkliveinfo.ysp.cctv.cn/";

pub const DEFAULT_LIVE_USER_ID: &str = "BAEBFF2B-C516-4F34-ABC0-A824A6461CBD";
pub const DEFAULT_DEVICE_NAME: &str = "央视频电视投屏助手";
pub const VDN_APP_NAME: &str = "央视频电视投屏助手";
pub const REPORT_APP_KEY: &str = "1178c84d-4818-44ff-b415-02106e87e144";
pub const COLLECT_SDK_VERSION: &str = "1.0.0";
pub const DEFAULT_PAGE_NAME: &str = "com.cctv.tv.mvp.ui.activity.MainActivity";
pub const DEFAULT_ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.8";

pub const RESULT_OK: i64 = 0;
pub const RESULT_NEEDS_REGISTER: i64 = 601;
pub const RESULT_GET_MISSING_OR_INVALID: i64 = 2;
pub const RESULT_REGISTERED_ELSEWHERE: i64 = 694;
pub const RESULT_REGISTER_RETRY_LATER: i64 = 695;

pub const LOGO_BASE: &str = "https://garysclub.sharewithyou.dpdns.org/logos/ysp-live-logos";
pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

pub const REFRESH_INTERVAL: f64 = 10.0;
pub const IDLE_TIMEOUT: f64 = 120.0;
pub const LINK_IDLE_TIMEOUT: f64 = 15.0;
pub const WINDOW: i64 = 300;
pub const MAX_SEGS: usize = 120;
pub const BK_URL_TTL: f64 = 300.0;
pub const DEFAULT_SERVER_PORT: u16 = 8767;
pub const DEFAULT_ENGINE_PORT: u16 = 8787;
pub const TS_MAX_BYTES: usize = 35 * 1024 * 1024;

pub const VER_NAME: &str = "3.2.7.26212";
pub const VER_CODE_STR: &str = "302070";
pub const VER_CODE_I32: i32 = 302070;
pub const APP_ID: &str = "1200013";
pub const QMF_APP_ID: i32 = 10012;
pub const QMF_PLATFORM: i8 = 1;
pub const BIZ_ID: i64 = 0;
pub const CHAN_ID: &str = "10070";

pub const CK_PLATFORM: u32 = 4330403;
pub const CK_APPVER: &str = "V8.22.1035.3031";
pub const CK_TEA: [u8; 16] = [
    0x59, 0xb2, 0xf7, 0xcf, 0x72, 0x5e, 0xf4, 0x3c,
    0x34, 0xfd, 0xd7, 0xc1, 0x23, 0x41, 0x1e, 0xd3,
];
pub const CK_GTEA: [u8; 16] = [
    0x11, 0x0d, 0xbe, 0xc1, 0x0c, 0x23, 0xe7, 0xd2,
    0xe5, 0x6a, 0x1c, 0xad, 0x69, 0x14, 0xef, 0x1b,
];
pub const CK_XOR: [u8; 16] = [
    132, 46, 237, 8, 240, 102, 230, 234, 72, 180, 202, 169, 145, 237, 111, 243,
];
pub const CK_GXOR: [u8; 8] = [179, 201, 83, 160, 105, 19, 173, 77];
pub const BK_H264_B64: &str = "SCgzMDoxMDgwLDYwOjEwODB8MzA6MTA4MCw2MDoxMDgwKQ==";

pub const APP_CHANNEL: &str = "dangbei";
pub const APP_VERSION: &str = "1.4.1";

pub fn high_lock_timeout() -> f64 {
    std::env::var("YSP_HIGH_LOCK_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(25.0)
        .max(1.0)
}

pub fn env_only_4k() -> bool {
    matches!(
        std::env::var("YSP_ONLY_4K_DEVICE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    )
}

pub fn env_links_per_device() -> usize {
    std::env::var("YSP_LINKS_PER_DEVICE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(6)
}

pub fn env_zap_debounce() -> f64 {
    std::env::var("YSP_ZAP_DEBOUNCE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(10.0)
}
