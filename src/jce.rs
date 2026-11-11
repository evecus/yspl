//! 对应 Python `class W` / `class R` / `_qua` / `_head` / `_wrap` / `_unwrap`
//! / `jce_timeshift_url` / `_ckey` 调用方 / `bk_playurls` / `fetch_abs_playlist`。
//! JCE(Tars) 的 TLV 编解码按 Python 版逐字节对齐，无第三方 crate 可用。

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use crate::constants::{
    APP_ID, BIZ_ID, BK_H264_B64, BK_LIVEINFO_URL, CHAN_ID, CK_APPVER, CK_PLATFORM, JACC_URL,
    QMF_APP_ID, QMF_PLATFORM, VER_CODE_I32, VER_CODE_STR, VER_NAME,
};
use crate::crypto::tea::ckey;
use crate::error::{Result, YsptpError};
use crate::http_client::{HttpClient, HttpResponse};
use crate::util::{form_urlencode_value, url_host};

// ---------------------------------------------------------------- JCE W（编码）

pub struct W {
    pub b: Vec<u8>,
}

impl Default for W {
    fn default() -> Self {
        Self::new()
    }
}

impl W {
    pub fn new() -> Self {
        Self { b: Vec::new() }
    }

    fn head(&mut self, typ: u8, tag: u8) {
        if tag < 15 {
            self.b.push((tag & 15) << 4 | (typ & 15));
        } else {
            self.b.push(0xF0 | (typ & 15));
            self.b.push(tag);
        }
    }

    pub fn byte(&mut self, v: i8, tag: u8) {
        if v == 0 {
            self.head(12, tag);
        } else {
            self.head(0, tag);
            self.b.extend_from_slice(&v.to_be_bytes());
        }
    }

    pub fn short(&mut self, v: i16, tag: u8) {
        if (-128..=127).contains(&v) {
            self.byte(v as i8, tag);
        } else {
            self.head(1, tag);
            self.b.extend_from_slice(&v.to_be_bytes());
        }
    }

    pub fn int(&mut self, v: i32, tag: u8) {
        if (-32768..=32767).contains(&v) {
            self.short(v as i16, tag);
        } else {
            self.head(2, tag);
            self.b.extend_from_slice(&v.to_be_bytes());
        }
    }

    pub fn long(&mut self, v: i64, tag: u8) {
        if (i32::MIN as i64..=i32::MAX as i64).contains(&v) {
            self.int(v as i32, tag);
        } else {
            self.head(3, tag);
            self.b.extend_from_slice(&v.to_be_bytes());
        }
    }

    pub fn float(&mut self, v: f32, tag: u8) {
        self.head(4, tag);
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn double(&mut self, v: f64, tag: u8) {
        self.head(5, tag);
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn string(&mut self, s: &str, tag: u8) {
        let data = s.as_bytes();
        if data.len() > 255 {
            self.head(7, tag);
            self.b.extend_from_slice(&(data.len() as i32).to_be_bytes());
            self.b.extend_from_slice(data);
        } else {
            self.head(6, tag);
            self.b.push(data.len() as u8);
            self.b.extend_from_slice(data);
        }
    }

    pub fn bytes(&mut self, data: &[u8], tag: u8) {
        self.head(13, tag);
        self.head(0, 0);
        self.int(data.len() as i32, 0);
        self.b.extend_from_slice(data);
    }

    pub fn structure<F: FnOnce(&mut W)>(&mut self, f: F, tag: u8) {
        self.head(10, tag);
        f(self);
        self.head(11, 0);
    }

    pub fn list<T, F: Fn(&mut W, &T)>(&mut self, items: &[T], tag: u8, wf: Option<F>) {
        self.head(9, tag);
        self.int(items.len() as i32, 0);
        if let Some(f) = wf {
            for item in items {
                f(self, item);
            }
        }
    }

    pub fn out(self) -> Vec<u8> {
        self.b
    }
}

// ---------------------------------------------------------------- JCE R（解码）

#[derive(Debug, Clone)]
pub enum JceValue {
    Int(i64),
    Float(f32),
    Double(f64),
    Str(String),
    Bytes(Vec<u8>),
    Struct(HashMap<u8, JceValue>),
    List(Vec<JceValue>),
    Zero,
}

impl JceValue {
    pub fn as_int(&self) -> i64 {
        match self {
            JceValue::Int(v) => *v,
            JceValue::Zero => 0,
            JceValue::Float(v) => *v as i64,
            JceValue::Double(v) => *v as i64,
            _ => 0,
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            JceValue::Str(s) => s.clone(),
            JceValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            JceValue::Int(v) => v.to_string(),
            JceValue::Zero => "0".to_string(),
            _ => String::new(),
        }
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        match self {
            JceValue::Bytes(b) => b.clone(),
            JceValue::Str(s) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        }
    }
}

pub struct R<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Self { d, p: 0 }
    }

    fn rem(&self) -> usize {
        self.d.len().saturating_sub(self.p)
    }

    fn get(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.d.len() {
            return Err(YsptpError::new("JCE unexpected EOF"));
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.get(1)?[0])
    }

    fn head(&mut self) -> Result<(u8, u8)> {
        let b = self.u8()?;
        let typ = b & 15;
        let mut tag = (b & 0xF0) >> 4;
        if tag == 15 {
            tag = self.u8()?;
        }
        Ok((typ, tag))
    }

    fn field_value(&mut self) -> Result<JceValue> {
        let (t, _) = self.head()?;
        self.value(t)
    }

    fn field_int(&mut self) -> Result<i64> {
        Ok(self.field_value()?.as_int())
    }

    fn value(&mut self, typ: u8) -> Result<JceValue> {
        match typ {
            0 => Ok(JceValue::Int(i8::from_be_bytes([self.u8()?]) as i64)),
            1 => {
                let b = self.get(2)?;
                Ok(JceValue::Int(i16::from_be_bytes([b[0], b[1]]) as i64))
            }
            2 => {
                let b = self.get(4)?;
                Ok(JceValue::Int(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64))
            }
            3 => {
                let b = self.get(8)?;
                let mut a = [0u8; 8];
                a.copy_from_slice(b);
                Ok(JceValue::Int(i64::from_be_bytes(a)))
            }
            4 => {
                let b = self.get(4)?;
                let mut a = [0u8; 4];
                a.copy_from_slice(b);
                Ok(JceValue::Float(f32::from_be_bytes(a)))
            }
            5 => {
                let b = self.get(8)?;
                let mut a = [0u8; 8];
                a.copy_from_slice(b);
                Ok(JceValue::Double(f64::from_be_bytes(a)))
            }
            6 => {
                let n = self.u8()? as usize;
                Ok(JceValue::Str(String::from_utf8_lossy(self.get(n)?).into_owned()))
            }
            7 => {
                let b = self.get(4)?;
                let n = i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
                Ok(JceValue::Str(String::from_utf8_lossy(self.get(n)?).into_owned()))
            }
            8 => {
                let n = self.field_int()? as usize;
                let mut m = HashMap::new();
                for _ in 0..n {
                    let k = self.field_value()?;
                    let v = self.field_value()?;
                    m.insert(k.as_int() as u8, v);
                }
                Ok(JceValue::Struct(m))
            }
            9 => {
                let n = self.field_int()? as usize;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.field_value()?);
                }
                Ok(JceValue::List(out))
            }
            10 => Ok(JceValue::Struct(self.structure()?)),
            11 => Ok(JceValue::Zero),
            12 => Ok(JceValue::Zero),
            13 => {
                let _ = self.head()?; // 子类型头
                let n = self.field_int()? as usize;
                Ok(JceValue::Bytes(self.get(n)?.to_vec()))
            }
            other => Err(YsptpError::new(format!("JCE unknown type {other}"))),
        }
    }

    pub fn structure(&mut self) -> Result<HashMap<u8, JceValue>> {
        let mut m = HashMap::new();
        while self.rem() > 0 {
            let (t, tag) = self.head()?;
            if t == 11 {
                break;
            }
            let v = self.value(t)?;
            m.insert(tag, v);
        }
        Ok(m)
    }
}

// ---------------------------------------------------------------- QMF 封包

static GUID: OnceLock<String> = OnceLock::new();

fn guid() -> &'static str {
    GUID.get_or_init(|| {
        let mut b = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
        let mut s = String::with_capacity(32);
        for x in b {
            s.push_str(&format!("{x:02x}"));
        }
        s
    })
}

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(data)
        .map_err(|e| YsptpError::new(format!("gzip compress failed: {e}")))?;
    enc.finish()
        .map_err(|e| YsptpError::new(format!("gzip compress failed: {e}")))
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|e| YsptpError::new(format!("gzip decompress failed: {e}")))?;
    Ok(out)
}

fn qua(w: &mut W) {
    w.string(VER_NAME, 0);
    w.string(VER_CODE_STR, 1);
    w.int(1080, 2);
    w.int(2400, 3);
    w.int(3, 4);
    w.string("12", 5);
    w.int(1, 6);
    w.int(1, 7);
    w.int(420, 8);
    w.string(CHAN_ID, 9);
    for i in 10..15 {
        w.string("", i);
    }
    w.structure(
        |ww| {
            ww.int(0, 0);
            ww.byte(0, 1);
            ww.string("", 2);
        },
        15,
    );
    w.string("", 16);
    w.string("", 17);
    w.string("", 18);
    w.structure(
        |ww| {
            ww.int(0, 0);
            ww.float(0.0, 1);
            ww.float(0.0, 2);
            ww.double(0.0, 3);
        },
        19,
    );
    let g = guid();
    w.string(&g[..16], 20);
    w.string("Pixel 6", 21);
    w.int(1, 22);
    for i in 23..27 {
        w.int(0, i);
    }
    w.string("", 27);
    w.string("", 28);
    w.string(g, 29);
}

fn qmf_head(w: &mut W, cmd: u16, reqid: i32) {
    w.int(reqid, 0);
    w.int(cmd as i32, 1);
    w.structure(qua, 2);
    w.string(APP_ID, 3);
    w.string(guid(), 4);
    w.list::<u8, fn(&mut W, &u8)>(&[], 5, None);
    w.structure(|_| {}, 6);
    w.list::<u8, fn(&mut W, &u8)>(&[], 7, None);
    w.int(0, 8);
    w.int(0, 9);
    w.int(0, 10);
}

pub fn wrap(cmd: u16, body: &[u8], reqid: i32) -> Result<Vec<u8>> {
    let mut w = W::new();
    w.structure(|ww| qmf_head(ww, cmd, reqid), 0);
    w.bytes(body, 1);
    let reqcmd = w.out();

    let mut inner = Vec::with_capacity(reqcmd.len() + 17);
    inner.push(38u8); // '&'
    inner.extend_from_slice(&((reqcmd.len() + 17) as i32).to_be_bytes());
    inner.push(1u8);
    inner.extend_from_slice(&[0u8; 10]);
    inner.extend_from_slice(&reqcmd);
    inner.push(40u8); // '('
    let comp = gzip_compress(&inner)?;

    let mut out = Vec::new();
    out.push(19u8); // 0x13
    out.extend_from_slice(&0i32.to_be_bytes()); // 长度占位，末尾 patch
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&65281u16.to_be_bytes());
    out.extend_from_slice(&cmd.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(reqid as i64).to_be_bytes());
    out.extend_from_slice(&531i32.to_be_bytes());
    out.extend_from_slice(&QMF_APP_ID.to_be_bytes());
    out.extend_from_slice(&BIZ_ID.to_be_bytes());
    let g = guid().as_bytes();
    out.extend_from_slice(&g[..g.len().min(32)]);
    out.extend_from_slice(&[0u8; 32][..32usize.saturating_sub(g.len().min(32))]);
    out.extend_from_slice(&QMF_PLATFORM.to_be_bytes());
    out.extend_from_slice(&VER_CODE_I32.to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.push(0u8);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(inner.len() as i32).to_be_bytes());
    out.extend_from_slice(&comp);
    out.push(3u8);
    let len = out.len() as i32;
    out[1..5].copy_from_slice(&len.to_be_bytes());
    Ok(out)
}

pub fn unwrap(data: &[u8]) -> Result<Option<Vec<u8>>> {
    if data.first() != Some(&0x13) || data.len() < 90 {
        return Ok(None);
    }
    let flags = i32::from_be_bytes([data[21], data[22], data[23], data[24]]);
    let mut payload = data[89..data.len() - 1].to_vec();
    if flags & 2 != 0 {
        payload = gzip_decompress(&payload)?;
    }
    if payload.first() != Some(&38) || payload.last() != Some(&40) {
        return Ok(None);
    }
    let inner = &payload[16..payload.len() - 1];
    let m = R::new(inner).structure()?;
    Ok(m.get(&1).map(|v| v.as_bytes()).filter(|b| !b.is_empty()))
}

// ---------------------------------------------------------------- 时移 / bk

/// 对应 `jce_timeshift_url(pid, sid, start, end, stream='fhd')`。
pub async fn jce_timeshift_url(
    client: &HttpClient,
    pid: &str,
    sid: &str,
    start: i64,
    end: i64,
    stream: &str,
) -> Result<String> {
    let mut w = W::new();
    w.string(pid, 0);
    w.string(sid, 1);
    w.long(start, 2);
    w.long(end, 3);
    w.string(stream, 4);
    let body = w.out();
    let cmd: u16 = 25312;
    let reqid = (crate::util::current_time_ms() & 0x7FFF_FFFF) as i32;
    let packet = wrap(cmd, &body, reqid)?;
    let headers = [("Content-Type", "application/octet-stream")];
    let resp = client
        .send_timeout(
            reqwest::Method::POST,
            JACC_URL,
            &headers,
            Some(packet),
            Duration::from_secs(15),
        )
        .await?;
    let resp_body = unwrap(&resp.body)?
        .ok_or_else(|| YsptpError::new("bad response"))?;
    let m = R::new(&resp_body).structure()?;
    let err = m.get(&0).map(|v| v.as_int()).unwrap_or(0);
    if err != 0 {
        let msg = m
            .get(&1)
            .map(|v| v.as_str())
            .unwrap_or_else(|| format!("errCode={err}"));
        return Err(YsptpError::new(msg));
    }
    let url = m.get(&2).map(|v| v.as_str()).unwrap_or_default();
    if url.is_empty() {
        return Err(YsptpError::new("empty m3u8"));
    }
    if url.contains("liverecord.video.cloud.cctv.com") {
        return Err(YsptpError::dead_host("dead cdn host"));
    }
    Ok(url)
}

/// 对应 `bk_playurls(channel_id, live_pid, defn='fhd')`。
pub async fn bk_playurls(client: &HttpClient, channel_id: &str, live_pid: &str, defn: &str) -> Result<Vec<String>> {
    let t = ckey(channel_id);
    let ck = t["cKey"].as_str().unwrap_or("");
    let guid = t["guid"].as_str().unwrap_or("");
    let ts = t["ts"].as_u64().unwrap_or(0);
    let flow_id = t["flowId"].as_str().unwrap_or("");
    let platform_str = CK_PLATFORM.to_string();
    let ts_str = ts.to_string();
    let pairs: Vec<(&str, &str)> = vec![
        ("atime", "120"),
        ("livepid", live_pid),
        ("cnlid", channel_id),
        ("appVer", CK_APPVER),
        ("app_version", "300090"),
        ("caplv", "1"),
        ("cmd", "2"),
        ("defn", defn),
        ("device", "iPhone"),
        ("encryptVer", "4.2"),
        ("getpreviewinfo", "0"),
        ("hevclv", "0"),
        ("lang", "zh-Hans_CN"),
        ("livequeue", "0"),
        ("logintype", "1"),
        ("nettype", "1"),
        ("newnettype", "1"),
        ("newplatform", platform_str.as_str()),
        ("platform", platform_str.as_str()),
        ("sdtfrom", "v3021"),
        ("spacode", "23"),
        ("spaudio", "1"),
        ("spdemuxer", "6"),
        ("spdrm", "2"),
        ("spdynamicrange", "1"),
        ("spflv", "1"),
        ("spflvaudio", "1"),
        ("sphdrfps", "60"),
        ("sphttps", "1"),
        ("spvcode", BK_H264_B64),
        ("spvideo", "4"),
        ("stream", "1"),
        ("system", "1"),
        ("sysver", "ios18.2.1"),
        ("uhd_flag", "0"),
        ("cKey", ck),
        ("guid", guid),
        ("fntick", ts_str.as_str()),
        ("flowid", flow_id),
        ("playbacktime", "0"),
    ];
    let query = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_urlencode_value(k), form_urlencode_value(v)))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("{BK_LIVEINFO_URL}?{query}");
    let headers = [("User-Agent", "qqlive"), ("Accept", "application/json")];
    let resp = client
        .send_timeout(reqwest::Method::GET, &url, &headers, None, Duration::from_secs(5))
        .await?;
    let p: Value = serde_json::from_slice(&resp.body)
        .map_err(|e| YsptpError::new(format!("bk response json: {e}")))?;
    let iretcode = p.get("iretcode").and_then(|v| v.as_i64()).unwrap_or(-1);
    if iretcode != 0 {
        return Err(YsptpError::new(format!(
            "iretcode={} {}",
            iretcode,
            p.get("errinfo").and_then(|v| v.as_str()).unwrap_or("")
        )));
    }
    let mut urls: Vec<String> = Vec::new();
    if let Some(u) = p.get("playurl").and_then(|v| v.as_str()) {
        urls.push(u.to_string());
    }
    let bu = p
        .get("backurl_list")
        .or_else(|| p.get("backurlList"))
        .or_else(|| p.get("backurl"));
    match bu {
        Some(Value::Array(arr)) => {
            for it in arr {
                let u = match it {
                    Value::String(s) => s.clone(),
                    Value::Object(_) => it
                        .get("url")
                        .or_else(|| it.get("playurl"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    _ => String::new(),
                };
                urls.push(u);
            }
        }
        Some(Value::String(s)) => {
            urls.extend(s.split([',', ';']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()));
        }
        _ => {}
    }
    // Python: [u for u in dict.fromkeys(urls) if u and '.cctv.' in u]
    let mut seen = std::collections::HashSet::new();
    urls.retain(|u| !u.is_empty() && u.contains(".cctv.") && seen.insert(u.clone()));
    if urls.is_empty() {
        return Err(YsptpError::new("no playurl"));
    }
    urls.sort_by(|a, b| {
        let ka = if a.contains("bklive-") { 0 } else { 1 };
        let kb = if b.contains("bklive-") { 0 } else { 1 };
        ka.cmp(&kb).then_with(|| a.cmp(b))
    });
    Ok(urls)
}

/// 对应 `fetch_abs_playlist(url, depth=0)`。
pub async fn fetch_abs_playlist(client: &HttpClient, url: &str, depth: u8) -> Result<String> {
    let headers = [
        ("User-Agent", "qqlive"),
        ("Referer", "https://live.cctv.cn/"),
        ("Accept", "application/vnd.apple.mpegurl,application/json,*/*"),
    ];
    let resp: HttpResponse = client
        .send_timeout(reqwest::Method::GET, url, &headers, None, Duration::from_secs(10))
        .await?;
    let text = resp.text();
    let final_url = resp.final_url.clone();
    let lines: Vec<&str> = text.lines().collect();
    if depth < 2 {
        for (i, ln) in lines.iter().enumerate() {
            if ln.trim_start().starts_with("#EXT-X-STREAM-INF") {
                for s in &lines[i + 1..] {
                    let t = s.trim();
                    if !t.is_empty() && !t.starts_with('#') {
                        let joined = url::Url::parse(&final_url)
                            .ok()
                            .and_then(|b| b.join(t).ok())
                            .map(|u| u.to_string())
                            .unwrap_or_else(|| t.to_string());
                        return Box::pin(fetch_abs_playlist(client, &joined, depth + 1)).await;
                    }
                }
                break;
            }
        }
    }
    let mut out = Vec::new();
    for ln in &lines {
        let s = ln.trim();
        if !s.is_empty() && !s.starts_with('#') {
            out.push(
                url::Url::parse(&final_url)
                    .ok()
                    .and_then(|b| b.join(s).ok())
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| s.to_string()),
            );
        } else {
            out.push((*ln).to_string());
        }
    }
    Ok(out.join("\n"))
}

/// 供 HLS 层判断 CDN 主机是否需要签名回放头。
pub fn host_needs_signed_headers(host: &str) -> bool {
    let lower = host.to_lowercase();
    lower.contains("liveali") || lower.contains("liveten")
}

pub fn url_host_safe(url: &str) -> String {
    url_host(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jce_w_roundtrip_struct() {
        let mut w = W::new();
        w.string("pid", 0);
        w.long(1_700_000_000, 2);
        w.int(0, 8);
        let m = R::new(&w.out()).structure().unwrap();
        assert_eq!(m.get(&0).unwrap().as_str(), "pid");
        assert_eq!(m.get(&2).unwrap().as_int(), 1_700_000_000);
        assert_eq!(m.get(&8).unwrap().as_int(), 0);
    }

    #[test]
    fn qmf_wrap_unwrap_self_check() {
        let body = b"hello-jce-body";
        let pkt = wrap(25312, body, 12345).unwrap();
        assert_eq!(pkt[0], 0x13);
        let len = i32::from_be_bytes([pkt[1], pkt[2], pkt[3], pkt[4]]) as usize;
        assert_eq!(len, pkt.len());
        // unwrap 期望完整的 QMF 响应结构，这里仅校验包体长度字段与 magic
        assert!(pkt.len() > 90);
    }

    #[test]
    fn normalized_lines() {
        assert!(host_needs_signed_headers("liveali.cctv.cn"));
        assert!(!host_needs_signed_headers("bklive.cctv.cn"));
    }
}
