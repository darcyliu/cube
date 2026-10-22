//! Runtime paths shared by cube and cubectl.

use std::ffi::CStr;
use std::path::PathBuf;

pub fn default_socket_path() -> Result<PathBuf, String> {
    socket_path_for(unsafe { libc::geteuid() })
}

pub fn socket_path_for(uid: u32) -> Result<PathBuf, String> {
    let (_name, home) = lookup_user(uid)?;
    Ok(home.join("Library/Application Support/cube/vmnet.sock"))
}

pub fn lookup_user(uid: u32) -> Result<(String, PathBuf), String> {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return Err(format!("getpwuid({uid}) failed"));
    }
    let name_ptr = unsafe { (*pw).pw_name };
    let dir_ptr = unsafe { (*pw).pw_dir };
    if name_ptr.is_null() || dir_ptr.is_null() {
        return Err(format!("getpwuid({uid}) returned an empty name or home"));
    }
    let name = unsafe { CStr::from_ptr(name_ptr) }
        .to_string_lossy()
        .into_owned();
    let home = unsafe { CStr::from_ptr(dir_ptr) }
        .to_string_lossy()
        .into_owned();
    if name.is_empty() || home.is_empty() {
        return Err(format!("getpwuid({uid}) returned an empty name or home"));
    }
    Ok((name, PathBuf::from(home)))
}
