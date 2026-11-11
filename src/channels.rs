use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::channels_data::*;
use crate::constants::LOGO_BASE;
use crate::util::normalize_channel;

#[derive(Debug, Clone)]
pub struct ChannelMeta {
    pub slug: String,
    pub name: String,
    pub sid: String,
    pub pid: String,
    pub defn: String,
    pub logo: String,
    pub group: String,
    pub tvg_id: String,
}

fn alias_map() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    M.get_or_init(|| ALIAS_MAP.iter().copied().collect())
}

fn tvg_map() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    M.get_or_init(|| TVG_IDS.iter().copied().collect())
}

fn live_id_map() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    M.get_or_init(|| BACKEND_LIVE_IDS.iter().copied().collect())
}

fn backend_set() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| BACKEND_CHANNELS.iter().copied().collect())
}
fn true4k_set() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| TRUE_4K_CHANNELS.iter().copied().collect())
}
fn timeshift_set() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| TIMESHIFT_SUPPORTED.iter().copied().collect())
}
fn force_bk_set() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| FORCE_BK.iter().copied().collect())
}

/// Python `get_channel_group`: cctv*/cgtn* -> 「央视」，其余「卫视」。
pub fn get_channel_group(slug: &str) -> String {
    let s = canonical(slug);
    if s.starts_with("cctv") || s.starts_with("cgtn") {
        "央视".to_string()
    } else {
        "卫视".to_string()
    }
}

/// 归一化 + 别名映射后的规范 slug。
pub fn canonical(raw: &str) -> String {
    let key = normalize_channel(raw);
    match alias_map().get(key.as_str()) {
        Some(v) => (*v).to_string(),
        None => key,
    }
}

pub fn channel_by_name(name: &str) -> Option<String> {
    live_id_map().get(name).map(|s| (*s).to_string())
}

pub fn is_backend_channel(slug: &str) -> bool {
    backend_set().contains(slug)
}
pub fn is_true_4k(slug: &str) -> bool {
    true4k_set().contains(slug)
}
pub fn supports_timeshift(slug: &str) -> bool {
    timeshift_set().contains(slug)
}
pub fn is_force_bk(slug: &str) -> bool {
    force_bk_set().contains(slug)
}

#[derive(Default)]
pub struct ChannelDirectory {
    pub channels_meta: Vec<ChannelMeta>,
    pub canonical_slugs: Vec<String>,
}

impl ChannelDirectory {
    /// 对应 `load_defaults()`。
    pub fn new() -> Self {
        let mut dir = ChannelDirectory::default();
        for (slug, name, sid, pid, defn) in DEFAULT_CHANNELS {
            let group = get_channel_group(slug);
            let tvg_id = tvg_map().get(slug).copied().unwrap_or(slug).to_string();
            let logo = format!("{LOGO_BASE}/{slug}.png");
            dir.channels_meta.push(ChannelMeta {
                slug: (*slug).to_string(),
                name: (*name).to_string(),
                sid: (*sid).to_string(),
                pid: (*pid).to_string(),
                defn: (*defn).to_string(),
                logo,
                group,
                tvg_id,
            });
            dir.canonical_slugs.push((*slug).to_string());
        }
        dir
    }

    pub fn get_meta(&self, slug: &str) -> Option<ChannelMeta> {
        let c = canonical(slug);
        self.channels_meta.iter().find(|m| m.slug == c).cloned()
    }

    pub fn unique_channels(&self) -> Vec<ChannelMeta> {
        self.channels_meta.clone()
    }

    pub fn count(&self) -> usize {
        self.channels_meta.len()
    }

    pub fn count_cctv(&self) -> usize {
        self.channels_meta.iter().filter(|m| m.group == "央视").count()
    }

    pub fn count_weishi(&self) -> usize {
        self.channels_meta.iter().filter(|m| m.group == "卫视").count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_64_channels() {
        let d = ChannelDirectory::new();
        assert_eq!(d.count(), 64);
        assert_eq!(d.count_cctv() + d.count_weishi(), 64);
    }

    #[test]
    fn alias_resolution() {
        assert_eq!(canonical("anhuiws"), "ahws");
        // Python ALIAS_MAP 无 'cctv-5+' 条目，原样保留
        assert_eq!(canonical("CCTV-5+.m3u8"), "cctv-5+");
        assert_eq!(canonical("cctv5plus"), "cctv5p");
        assert_eq!(canonical("cctv16_4k"), "cctv164k");
    }

    #[test]
    fn backend_live_ids_are_26() {
        assert_eq!(BACKEND_LIVE_IDS.len(), 26);
        assert_eq!(channel_by_name("cctv1").as_deref(), Some("Live1717729995180256"));
    }

    #[test]
    fn tvg_ids_cover_all() {
        let d = ChannelDirectory::new();
        assert!(d.get_meta("cctv1").unwrap().tvg_id == "CCTV1");
        assert!(d.get_meta("bjws").unwrap().tvg_id == "北京卫视");
        assert!(d.get_meta("cctv1").unwrap().group == "央视");
    }
}
