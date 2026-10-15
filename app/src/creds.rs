//! Password storage in Windows Credential Manager (generic credentials, target "UI3Native:<server>|<user>").
//! UI3 stored credentials Base64-encoded in localStorage; this is the native equivalent done properly.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Security::Credentials::{CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW};
use windows::core::{PCWSTR, PWSTR};

fn target(server: &str, user: &str) -> Vec<u16> {
    format!("UI3Native:{server}|{user}").encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn save_password(server: &str, user: &str, password: &str) -> anyhow::Result<()> {
    let mut t = target(server, user);
    let mut u: Vec<u16> = user.encode_utf16().chain(std::iter::once(0)).collect();
    let blob = password.as_bytes().to_vec();
    let cred = CREDENTIALW {
        Flags: Default::default(),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(t.as_mut_ptr()),
        Comment: PWSTR::null(),
        LastWritten: FILETIME::default(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: PWSTR::null(),
        UserName: PWSTR(u.as_mut_ptr()),
    };
    unsafe { CredWriteW(&cred, 0)? };
    Ok(())
}

pub fn load_password(server: &str, user: &str) -> Option<String> {
    let t = target(server, user);
    let mut p: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        CredReadW(PCWSTR(t.as_ptr()), CRED_TYPE_GENERIC, None, &mut p).ok()?;
        let c = &*p;
        let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec();
        CredFree(p as *const _);
        String::from_utf8(bytes).ok()
    }
}

pub fn delete_password(server: &str, user: &str) {
    let t = target(server, user);
    unsafe {
        let _ = CredDeleteW(PCWSTR(t.as_ptr()), CRED_TYPE_GENERIC, None);
    }
}
