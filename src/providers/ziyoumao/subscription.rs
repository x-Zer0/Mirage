//! Online subscription envelope verified in ZIYOUMAO-NODE-ANALYSIS-2026-10-16.md.
//! This decoder belongs only to the provider; plain subscriptions never use it.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::subscriptions::{detect, import::MAX_CONFIG_BYTES};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use windows::Win32::Security::Cryptography::{
    BCRYPT_AES_CBC_ALG_HANDLE, BCRYPT_BLOCK_PADDING, BCRYPT_KEY_HANDLE, BCryptDecrypt,
    BCryptDestroyKey, BCryptGenerateSymmetricKey,
};
use zeroize::Zeroizing;

const STAGE: &str = "ziyoumao.subscription";
// Raw 16-byte protocol constants, not the 32-character hex representation.
const KEY: &[u8; 16] = b"efbf326c0a946d3f";
const IV: [u8; 16] = *b"ed287033263d145d";

/// Strict Base64 → AES-128-CBC/PKCS#7 → strict Base64 → UTF-8 Mihomo YAML.
/// Never fall back to plaintext or pass ciphertext to generic detection.
pub fn decode(raw: &[u8]) -> AppResult<Vec<u8>> {
    if raw.len() as u64 > MAX_CONFIG_BYTES {
        return Err(AppError::new(
            AppErrorKind::ResponseTooLarge,
            STAGE,
            "自由猫订阅超过 16 MiB",
        ));
    }
    let ciphertext = STANDARD
        .decode(raw)
        .map_err(|_| incompatible("外层 Base64 无效"))?;
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(16) {
        return Err(incompatible("AES 密文长度无效"));
    }
    let inner = decrypt(&ciphertext)?;
    let yaml = STANDARD
        .decode(inner.as_slice())
        .map_err(|_| incompatible("内层 Base64 无效"))?;
    let text = std::str::from_utf8(&yaml).map_err(|_| incompatible("解密内容不是 UTF-8"))?;
    // Require YAML directly: do not allow detect's extra Base64 fallback here.
    if !detect::is_mihomo_yaml(text) {
        return Err(incompatible("解密内容不是 Mihomo YAML"));
    }
    Ok(yaml)
}

fn decrypt(ciphertext: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
    let mut key = Key(BCRYPT_KEY_HANDLE::default());
    // SAFETY: the Windows 10+ pseudo-handle selects AES-CBC; CNG allocates the
    // key object and owns it until Key::drop. The key bytes live for the call.
    unsafe { BCryptGenerateSymmetricKey(BCRYPT_AES_CBC_ALG_HANDLE, &mut key.0, None, KEY, 0) }
        .ok()
        .map_err(|_| incompatible("无法初始化 AES-CBC"))?;
    let mut iv = IV;
    let mut plain = Zeroizing::new(vec![0; ciphertext.len()]);
    let mut written = 0;
    // SAFETY: all slices are live, output is at least ciphertext length, and
    // CNG validates/removes PKCS#7 padding with BCRYPT_BLOCK_PADDING.
    unsafe {
        BCryptDecrypt(
            key.0,
            Some(ciphertext),
            None,
            Some(&mut iv),
            Some(plain.as_mut_slice()),
            &mut written,
            BCRYPT_BLOCK_PADDING,
        )
    }
    .ok()
    .map_err(|_| incompatible("AES 解密或 PKCS#7 填充校验失败"))?;
    plain.truncate(written as usize);
    Ok(plain)
}

struct Key(BCRYPT_KEY_HANDLE);

impl Drop for Key {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: this handle is owned here and is destroyed exactly once.
            let _ = unsafe { BCryptDestroyKey(self.0) };
        }
    }
}

fn incompatible(detail: &str) -> AppError {
    AppError::new(
        AppErrorKind::ProviderCompat,
        STAGE,
        format!("自由猫 AES 订阅格式或解密不兼容：{detail}"),
    )
}
