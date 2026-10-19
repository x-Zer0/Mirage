//! SecretStore (DESIGN.md §14.1): Windows Credential Manager, with a DPAPI
//! encrypted file under `secrets\` for values above the 2560-byte blob limit.
//! Keys are opaque `SecretRef` strings such as `profile/<uuid>/url`.

use crate::domain::error::{AppError, AppResult};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use windows::Win32::Foundation::{ERROR_NOT_FOUND, HLOCAL, LocalFree};
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::{HRESULT, PCWSTR, PWSTR};

const STAGE: &str = "platform.secret";
const TARGET_PREFIX: &str = "Mirage/";
/// Credential blob content meaning "the value lives in an encrypted file".
const EXTERNAL_MARKER: &[u8] = b"mirage:external:v1";

#[derive(Debug, Clone)]
pub struct SecretStore {
    /// `%LOCALAPPDATA%\Mirage\secrets` (private ACL inherited from the root).
    dir: PathBuf,
}

impl SecretStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn put(&self, key: &str, value: &[u8]) -> AppResult<()> {
        if value.len() <= CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            cred_write(key, value)?; // 先写新值
            self.remove_file(key); // 成功后删旧文件
            return Ok(());
        }
        let sealed = dpapi(value, true)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| AppError::storage(STAGE, e))?;
        crate::storage::atomic::write(&self.file_for(key), &sealed)
            .map_err(|e| AppError::storage(STAGE, e))?;
        match cred_write(key, EXTERNAL_MARKER) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.remove_file(key); // 回滚：删除刚写的文件
                Err(e)
            }
        }
    }

    pub fn get(&self, key: &str) -> AppResult<Option<Vec<u8>>> {
        let Some(blob) = cred_read(key)? else {
            return Ok(None);
        };
        if blob != EXTERNAL_MARKER {
            return Ok(Some(blob));
        }
        let sealed = std::fs::read(self.file_for(key)).map_err(|e| AppError::storage(STAGE, e))?;
        dpapi(&sealed, false).map(Some)
    }

    pub fn delete(&self, key: &str) -> AppResult<()> {
        self.remove_file(key);
        let target = wide(&format!("{TARGET_PREFIX}{key}"));
        // SAFETY: NUL-terminated target name alive for the call.
        match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(()),
            Err(e) if e.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) => Ok(()),
            Err(e) => Err(AppError::storage(STAGE, e)),
        }
    }

    fn file_for(&self, key: &str) -> PathBuf {
        let digest = hex::encode(Sha256::digest(key.as_bytes()));
        self.dir.join(format!("{}.bin", &digest[..32]))
    }

    fn remove_file(&self, key: &str) {
        let _ = std::fs::remove_file(self.file_for(key));
    }
}

fn cred_write(key: &str, value: &[u8]) -> AppResult<()> {
    let mut target = wide(&format!("{TARGET_PREFIX}{key}"));
    let mut blob = value.to_vec();
    let cred = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    // SAFETY: every pointer in `cred` refers to buffers alive for the call.
    let result = unsafe { CredWriteW(&cred, 0) };
    blob.fill(0);
    result.map_err(|e| AppError::storage(STAGE, e))
}

fn cred_read(key: &str) -> AppResult<Option<Vec<u8>>> {
    let target = wide(&format!("{TARGET_PREFIX}{key}"));
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: on success `cred` points to a buffer owned by the system that is
    // copied and then released with CredFree.
    unsafe {
        match CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut cred) {
            Ok(()) => {}
            Err(e) if e.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) => return Ok(None),
            Err(e) => return Err(AppError::storage(STAGE, e)),
        }
        let c = &*cred;
        let value = if c.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
        };
        CredFree(cred as *const std::ffi::c_void);
        Ok(Some(value))
    }
}

/// Current-user DPAPI seal (`protect`) or unseal.
fn dpapi(data: &[u8], protect: bool) -> AppResult<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input points to `data` for the call; the output buffer is
    // allocated by the system, copied, then released with LocalFree.
    unsafe {
        let result = if protect {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        result.map_err(|e| AppError::storage(STAGE, e))?;
        let value = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(value)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
