//! 用户范围 DPAPI 加密：设置表里的敏感值（当前是 API Key）不再以明文落库。
//!
//! 存储格式：`dpapi1:` + base64(CryptProtectData 输出)。没有前缀的值按旧版明文处理。

use base64::{engine::general_purpose::STANDARD, Engine as _};

const SEALED_PREFIX: &str = "dpapi1:";
const UI_FORBIDDEN: u32 = 0x1;

pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(SEALED_PREFIX)
}

/// 加密成可入库的字符串。
pub fn seal(plain: &str) -> Result<String, String> {
    let protected = protect(plain.as_bytes())?;
    Ok(format!("{SEALED_PREFIX}{}", STANDARD.encode(protected)))
}

/// 解密；没有加密前缀的旧值原样返回（由调用方决定何时重新加密）。
pub fn open(stored: &str) -> Result<String, String> {
    let Some(encoded) = stored.strip_prefix(SEALED_PREFIX) else {
        return Ok(stored.to_owned());
    };
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|e| format!("无法解码已加密的配置：{e}"))?;
    let plain = unprotect(&bytes)?;
    String::from_utf8(plain).map_err(|_| "已加密的配置不是合法 UTF-8".to_owned())
}

#[cfg(windows)]
fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let len = u32::try_from(data.len()).map_err(|_| "配置过长，无法加密".to_owned())?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: len,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!("DPAPI 加密失败（错误码 {}）", unsafe { GetLastError() }));
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
    };
    unsafe {
        LocalFree(output.pbData as *mut _);
    }
    Ok(bytes)
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let len = u32::try_from(data.len()).map_err(|_| "加密配置过长".to_owned())?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: len,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "DPAPI 解密失败（错误码 {}）",
            unsafe { GetLastError() }
        ));
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
    };
    unsafe {
        LocalFree(output.pbData as *mut _);
    }
    Ok(bytes)
}

#[cfg(not(windows))]
fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI 仅支持 Windows".to_owned())
}

#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI 仅支持 Windows".to_owned())
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
