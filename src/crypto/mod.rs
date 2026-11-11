//! 对应 Python 手写密码学段落（_aes256_key_schedule / aes_gcm_encrypt /
//! aes_gcm_decrypt_b64 / rsa_oaep_sha256_encrypt / rsa_encrypt_device_id）。
//! 语义完全一致，实现改用成熟 crate。

pub mod tea;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::rngs::OsRng;
use rsa::oaep::Oaep;
use rsa::pkcs8::DecodePublicKey as _;
use rsa::RsaPublicKey;
use sha2::Sha256;

use crate::constants::RSA_PUBLIC_KEY_B64;
use crate::error::{Result, YsptpError};
use crate::util::{b64_standard_decode, b64_standard_encode};

/// Python `normalize_aes_key`: 字符串 UTF-8 字节后补 \\0 到 32 字节（不是哈希）。
pub fn normalize_aes_key(value: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    let raw = value.as_bytes();
    let n = raw.len().min(32);
    key[..n].copy_from_slice(&raw[..n]);
    key
}

fn aes_gcm_encrypt(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| YsptpError::new("AES-GCM key length invalid"))?;
    cipher
        .encrypt(Nonce::from_slice(nonce), plaintext)
        .map_err(|_| YsptpError::new("AES-GCM encrypt failed"))
}

fn aes_gcm_decrypt(key: &[u8; 32], nonce: &[u8; 12], ct_and_tag: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| YsptpError::new("AES-GCM key length invalid"))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ct_and_tag)
        .map_err(|_| YsptpError::new("AES-GCM decrypt failed"))
}

/// 对应 `aes_gcm_decrypt_b64(value, key)`：b64 -> nonce(12) || ct||tag -> 明文 UTF-8。
pub fn aes_gcm_decrypt_b64(value: &str, key: &str) -> Result<String> {
    let raw = b64_standard_decode(value)
        .ok_or_else(|| YsptpError::new("base64 decode failed"))?;
    if raw.len() <= 12 {
        return Err(YsptpError::new("AES-GCM payload too short"));
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&raw[..12]);
    let plain = aes_gcm_decrypt(&normalize_aes_key(key), &nonce, &raw[12..])?;
    String::from_utf8(plain).map_err(|e| YsptpError::new(format!("AES-GCM plaintext is not UTF-8: {e}")))
}

/// 对应 `aes_gcm_encrypt_b64(value, key)`：随机 12 字节 nonce + ct||tag，再 b64。
pub fn aes_gcm_encrypt_b64(value: &str, key: &str) -> Result<String> {
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = aes_gcm_encrypt(&normalize_aes_key(key), &nonce, value.as_bytes())?;
    let mut out = Vec::with_capacity(12 + encrypted.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted);
    Ok(b64_standard_encode(&out))
}

/// 对应 `rsa_encrypt_device_id`：SPKI DER 公钥，RSA-OAEP-SHA256/MGF1-SHA256，
/// 按 k-2*hlen-2 分块加密后拼接再 base64。
pub fn rsa_encrypt_device_id(device_id: &str) -> Result<String> {
    let der = b64_standard_decode(RSA_PUBLIC_KEY_B64)
        .ok_or_else(|| YsptpError::new("RSA public key base64 decode failed"))?;
    let pubkey = RsaPublicKey::from_public_key_der(&der)
        .map_err(|e| YsptpError::new(format!("RSA SPKI parse failed: {e}")))?;
    let k = rsa::traits::PublicKeyParts::size(&pubkey);
    let chunk_size = k - 2 * 32 - 2;
    let mut rng = OsRng;
    let mut out = Vec::new();
    for chunk in device_id.as_bytes().chunks(chunk_size) {
        let enc = pubkey
            .encrypt(&mut rng, Oaep::new::<Sha256>(), chunk)
            .map_err(|e| YsptpError::new(format!("RSA-OAEP encrypt failed: {e}")))?;
        out.extend_from_slice(&enc);
    }
    Ok(b64_standard_encode(&out))
}

use rand::RngCore as _;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_gcm_b64_roundtrip() {
        let key = "0123456789abcdef0123456789abcdef";
        let enc = aes_gcm_encrypt_b64("hello 世界", key).unwrap();
        assert_eq!(aes_gcm_decrypt_b64(&enc, key).unwrap(), "hello 世界");
    }

    #[test]
    fn key_is_padded_not_hashed() {
        let k = normalize_aes_key("abc");
        assert_eq!(&k[..3], b"abc");
        assert_eq!(k[3], 0);
    }

    #[test]
    fn rsa_device_id_is_nonempty_b64() {
        let out = rsa_encrypt_device_id("BAEBFF2B-C516-4F34-ABC0-A824A6461CBD").unwrap();
        assert_eq!(b64_standard_decode(&out).unwrap().len(), 256);
    }
}
