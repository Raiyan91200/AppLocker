//! Settings stored in `%APPDATA%\AppLocker\config.dat`, encrypted with Windows DPAPI
//! (tied to the current Windows account).

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::{fs, io};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct LockedApp {
    /// Lower-cased exe file name, used for matching (e.g. `chrome.exe`).
    pub name: String,
    /// Full path the user picked, shown in Settings.
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Config {
    /// Argon2id PHC string of the master password.
    pub password_hash: String,
    pub apps: Vec<LockedApp>,
}

/// Extra secret mixed into DPAPI so other programs can't decrypt the file by accident.
const ENTROPY: &[u8] = b"AppLocker.config.v1";

thread_local! {
    static CURRENT: RefCell<Config> = RefCell::new(Config::default());
}

fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("AppLocker")
}

fn file() -> PathBuf {
    dir().join("config.dat")
}

/// Reads and decrypts the config. `None` if missing or unreadable (first run).
pub fn load() -> Option<Config> {
    let raw = fs::read(file()).ok()?;
    let plain = unprotect(&raw).ok()?;
    serde_json::from_slice(&plain).ok()
}

pub fn save(cfg: &Config) -> io::Result<()> {
    fs::create_dir_all(dir())?;
    let encrypted = protect(&serde_json::to_vec(cfg)?)?;
    let tmp = file().with_extension("tmp");
    fs::write(&tmp, encrypted)?;
    fs::rename(tmp, file())
}

/// The in-memory config used by the UI thread.
pub fn current() -> Config {
    CURRENT.with(|c| c.borrow().clone())
}

pub fn set_current(cfg: Config) {
    CURRENT.with(|c| *c.borrow_mut() = cfg);
}

/// Changes the config and saves it to disk.
pub fn update(f: impl FnOnce(&mut Config)) -> io::Result<()> {
    CURRENT.with(|c| {
        let mut cfg = c.borrow().clone();
        f(&mut cfg);
        save(&cfg)?;
        *c.borrow_mut() = cfg;
        Ok(())
    })
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

fn take_blob(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: DPAPI allocated `cbData` bytes at `pbData`; we copy and free them.
    unsafe {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        v
    }
}

pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
    // SAFETY: all pointers are valid for the call.
    let ok = unsafe {
        CryptProtectData(&input, null(), &entropy, null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_blob(out))
}

pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
    // SAFETY: all pointers are valid for the call.
    let ok = unsafe {
        CryptUnprotectData(&input, null_mut(), &entropy, null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_blob(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip() {
        let secret = b"some secret config bytes";
        let enc = protect(secret).unwrap();
        assert_ne!(&enc[..], &secret[..]);
        assert_eq!(unprotect(&enc).unwrap(), secret);
    }

    #[test]
    fn tampered_data_fails() {
        let mut enc = protect(b"hello").unwrap();
        let last = enc.len() - 1;
        enc[last] ^= 0xFF;
        assert!(unprotect(&enc).is_err());
    }

    #[test]
    fn config_json_round_trip() {
        let cfg = Config {
            password_hash: "$argon2id$x".into(),
            apps: vec![LockedApp { name: "chrome.exe".into(), path: r"C:\chrome.exe".into() }],
        };
        let enc = protect(&serde_json::to_vec(&cfg).unwrap()).unwrap();
        let back: Config = serde_json::from_slice(&unprotect(&enc).unwrap()).unwrap();
        assert_eq!(back, cfg);
    }
}
