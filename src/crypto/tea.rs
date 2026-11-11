use rand::RngCore as _;
use serde_json::{json, Value};

use crate::constants::{AK, CK_APPVER, CK_GTEA, CK_GXOR, CK_PLATFORM, CK_TEA, CK_XOR};
use crate::util::{hex_upper, md5_hex};

const DELTA: u32 = 2_654_435_769; // 0x9E3779B9

/// 标准 TEA 单块加密（16 轮），大端 u32。
fn tea_blk(blk: &[u8; 8], key: &[u8; 16]) -> [u8; 8] {
    let mut y = u32::from_be_bytes([blk[0], blk[1], blk[2], blk[3]]);
    let mut z = u32::from_be_bytes([blk[4], blk[5], blk[6], blk[7]]);
    let k = [
        u32::from_be_bytes([key[0], key[1], key[2], key[3]]),
        u32::from_be_bytes([key[4], key[5], key[6], key[7]]),
        u32::from_be_bytes([key[8], key[9], key[10], key[11]]),
        u32::from_be_bytes([key[12], key[13], key[14], key[15]]),
    ];
    let mut s: u32 = 0;
    for _ in 0..16 {
        s = s.wrapping_add(DELTA);
        y = y.wrapping_add(
            (z.wrapping_shl(4).wrapping_add(k[0]))
                ^ z.wrapping_add(s)
                ^ (z.wrapping_shr(5).wrapping_add(k[1])),
        );
        z = z.wrapping_add(
            (y.wrapping_shl(4).wrapping_add(k[2]))
                ^ y.wrapping_add(s)
                ^ (y.wrapping_shr(5).wrapping_add(k[3])),
        );
    }
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&y.to_be_bytes());
    out[4..].copy_from_slice(&z.to_be_bytes());
    out
}

/// Python `_cksum`: `v = (131*v + b) & 0x7FFFFFFF`。
fn cksum(buf: &[u8]) -> u32 {
    let mut v: u32 = 0;
    for &b in buf {
        v = v.wrapping_mul(131).wrapping_add(b as u32) & 0x7FFF_FFFF;
    }
    v
}

/// Python `_tea_pkt`：随机填充 + CBC 链式 TEA，返回密文。
fn tea_pkt(data: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let pad = (8 - (data.len() + 10) % 8) % 8;
    let mut plain = Vec::with_capacity(1 + pad + 2 + data.len() + 7);
    let mut head = [0u8; 1];
    rand::rngs::OsRng.fill_bytes(&mut head);
    plain.push((head[0] & 0xF8) | pad as u8);
    if pad > 0 {
        let mut rnd = vec![0u8; pad];
        rand::rngs::OsRng.fill_bytes(&mut rnd);
        plain.extend_from_slice(&rnd);
    }
    let mut rnd2 = [0u8; 2];
    rand::rngs::OsRng.fill_bytes(&mut rnd2);
    plain.extend_from_slice(&rnd2);
    plain.extend_from_slice(data);
    plain.extend_from_slice(&[0u8; 7]);

    let mut out = Vec::with_capacity(plain.len());
    let mut pp = [0u8; 8];
    let mut pc = [0u8; 8];
    for chunk in plain.chunks_exact(8) {
        let mut mixed = [0u8; 8];
        for i in 0..8 {
            mixed[i] = chunk[i] ^ pc[i];
        }
        let enc = tea_blk(&mixed, key);
        let mut cipher = [0u8; 8];
        for i in 0..8 {
            cipher[i] = enc[i] ^ pp[i];
        }
        out.extend_from_slice(&cipher);
        pp = mixed;
        pc = cipher;
    }
    out
}

/// Python `_lp`: 2 字节大端长度前缀。
fn lp(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + s.len());
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s);
    out
}

fn tail(v: &str) -> &[u8] {
    let bytes = v.as_bytes();
    let start = bytes.len().saturating_sub(5);
    &bytes[start..]
}

/// Python `_ck_guard(ts, guid)` -> 大写 hex 字符串。
fn ck_guard(ts: u32, guid: &str) -> String {
    let mut body = Vec::new();
    body.extend_from_slice(&ts.to_be_bytes());
    body.extend_from_slice(&lp(tail(guid)));
    body.extend_from_slice(&lp(b"null"));
    body.extend_from_slice(&lp(b"null"));
    body.extend_from_slice(&lp(b"-1"));
    let plain = lp(&body);
    let mut enc = tea_pkt(&plain, &CK_GTEA);
    enc.extend_from_slice(&cksum(&plain).to_be_bytes());
    for (i, b) in enc.iter_mut().enumerate() {
        *b ^= CK_GXOR[i & 7];
    }
    hex_upper(&enc)
}

/// Python `_ckey(channel_id)`。返回 {cKey, guid, ts, flowId}。
pub fn ckey(channel_id: &str) -> Value {
    let ts = crate::util::now_f64() as u32;
    let mut guid_raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut guid_raw);
    let guid = hex_lower_bytes(&guid_raw);
    let guard = ck_guard(ts, &guid);
    let mut uid_raw = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut uid_raw);
    let uid = hex_upper(&uid_raw);

    let mut body = Vec::new();
    body.extend_from_slice(&[0x00, 0x00, 0x00, 0x42, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x04, 0xd2]);
    body.extend_from_slice(&CK_PLATFORM.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&ts.to_be_bytes());
    body.extend_from_slice(&lp(b"dcgh"));
    body.extend_from_slice(&lp(b"_zj1A5Gh6QYcxWjIUGos2w=="));
    body.extend_from_slice(&lp(CK_APPVER.as_bytes()));
    body.extend_from_slice(&lp(channel_id.as_bytes()));
    body.extend_from_slice(&lp(guid.as_bytes()));
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&lp(uid.as_bytes()));
    body.extend_from_slice(&lp(b"nil"));
    body.extend_from_slice(&lp(b"57eab0c4-2c58-44c6-8ae9-dd2757525dc5"));
    body.extend_from_slice(&lp(b"nil"));
    body.extend_from_slice(&lp(b"v0.1.000"));
    body.extend_from_slice(&lp(b"com.cctv.yangshipin.app.iphone"));
    body.extend_from_slice(&lp(CK_PLATFORM.to_string().as_bytes()));
    body.extend_from_slice(&lp(b"ex_json_bus"));
    body.extend_from_slice(&lp(b"ex_json_vs"));
    body.extend_from_slice(&lp(guard.as_bytes()));

    let mut pkt = Vec::with_capacity(2 + body.len());
    pkt.extend_from_slice(&(body.len() as u16).to_be_bytes());
    pkt.extend_from_slice(&body);
    // Python: pkt[18:22] = pack('>I', _cksum(bytes(pkt)))
    let sum = cksum(&pkt);
    pkt[18..22].copy_from_slice(&sum.to_be_bytes());

    let mut enc = tea_pkt(&pkt, &CK_TEA);
    enc.extend_from_slice(&cksum(&pkt).to_be_bytes());
    for (i, b) in enc.iter_mut().enumerate() {
        *b ^= CK_XOR[i & 15];
    }
    let b64 = crate::util::b64_standard_encode(&enc)
        .replace('+', "_")
        .replace('/', "-")
        .trim_end_matches('=')
        .to_string();
    let flow_id = format!(
        "{}_{}",
        uuid::Uuid::new_v4().simple().to_string().to_uppercase(),
        CK_PLATFORM
    );
    json!({
        "cKey": format!("--01{b64}"),
        "guid": guid,
        "ts": ts,
        "flowId": flow_id,
    })
}

fn hex_lower_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Python `generate_app_random_str`。
pub fn generate_app_random_str() -> String {
    let mut rng = rand::rngs::OsRng;
    let a = rng.next_u32();
    let b = rng.next_u32() & 0xFFFF;
    let c = rng.next_u32() & 0xFFFF;
    format!("{a:08x}-0000-{b:04x}-0000-00000000{c:04x}")
}

/// Python `compute_vdn_code(app_secret, random_str=None)` -> (app_sign, random_str)。
pub fn compute_vdn_code(app_secret: &str, random_str: Option<String>) -> (String, String) {
    let r = random_str.unwrap_or_else(generate_app_random_str);
    (md5_hex(&format!("{AK}{app_secret}{r}")), r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tea_blk_known_vector() {
        // 标准 TEA：全零密钥/明文，16 轮后的已知结果
        let out = tea_blk(&[0u8; 8], &[0u8; 16]);
        // 仅做结构校验（非零且长度 8），具体向量在 golden 对拍中验证
        assert_eq!(out.len(), 8);
        assert_ne!(out, [0u8; 8]);
    }

    #[test]
    fn cksum_is_bounded() {
        assert!(cksum(b"hello world") < 0x8000_0000);
    }

    #[test]
    fn vdn_code_is_md5_hex() {
        let (sign, r) = compute_vdn_code("secret", Some("fixed".into()));
        assert_eq!(sign, md5_hex(&format!("{AK}secretfixed")));
        assert_eq!(r, "fixed");
    }

    #[test]
    fn ckey_shape() {
        let v = ckey("2040728201");
        assert!(v["cKey"].as_str().unwrap().starts_with("--01"));
        assert_eq!(v["guid"].as_str().unwrap().len(), 32);
    }
}
