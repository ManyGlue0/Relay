//! API keys live in Windows Credential Manager (generic credentials named
//! `Relay/provider/<id>`), never in the database or the settings file.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND, FILETIME};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn set(target: &str, secret: &str) -> Result<(), String> {
        let mut name = wide(target);
        let mut user = wide("Relay");
        let blob = secret.as_bytes();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: name.as_mut_ptr(),
            Comment: std::ptr::null_mut(),
            LastWritten: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        if unsafe { CredWriteW(&cred, 0) } == 0 {
            return Err(format!("Windows couldn't save the key (error {})", unsafe { GetLastError() }));
        }
        Ok(())
    }

    pub fn get(target: &str) -> Option<String> {
        let name = wide(target);
        let mut out: *mut CREDENTIALW = std::ptr::null_mut();
        unsafe {
            if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut out) == 0 || out.is_null() {
                return None;
            }
            let c = &*out;
            let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec();
            CredFree(out as *const _);
            String::from_utf8(bytes).ok()
        }
    }

    pub fn delete(target: &str) -> Result<(), String> {
        let name = wide(target);
        unsafe {
            if CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) == 0 && GetLastError() != ERROR_NOT_FOUND {
                return Err(format!("Windows couldn't remove the key (error {})", GetLastError()));
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn set(_: &str, _: &str) -> Result<(), String> {
        Err("Keys are only stored on Windows".into())
    }
    pub fn get(_: &str) -> Option<String> {
        None
    }
    pub fn delete(_: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Dev and release builds share the data folder, so they share keys too.
fn target(provider_id: &str) -> String {
    format!("Relay/provider/{provider_id}")
}

pub fn set_key(provider_id: &str, key: &str) -> Result<(), String> {
    imp::set(&target(provider_id), key)
}

pub fn key(provider_id: &str) -> Option<String> {
    imp::get(&target(provider_id)).filter(|k| !k.is_empty())
}

pub fn delete_key(provider_id: &str) -> Result<(), String> {
    imp::delete(&target(provider_id))
}

/// Any secret Relay keeps, by its Credential Manager name.
pub fn get(target: &str) -> Option<String> {
    imp::get(target)
}

pub fn set(target: &str, value: &str) -> Result<(), String> {
    imp::set(target, value)
}

pub fn delete(target: &str) -> Result<(), String> {
    imp::delete(target)
}
