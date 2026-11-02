//! Per-user Windows proxy settings.
//!
//! Everything here lives under HKEY_CURRENT_USER, so no admin rights are needed.
//! The previous settings are saved to a small backup file before we change anything and put
//! back on exit. If the program is killed, run `torproxy reset` to restore them.

use anyhow::Result;

#[cfg(windows)]
pub fn enable(port: u16) -> Result<()> {
    imp::enable(port)
}

#[cfg(windows)]
pub fn restore() -> Result<()> {
    imp::restore()
}

#[cfg(not(windows))]
pub fn enable(_port: u16) -> Result<()> {
    println!("(Not Windows: skipping system proxy setup. Point apps at the SOCKS5 port yourself.)");
    Ok(())
}

#[cfg(not(windows))]
pub fn restore() -> Result<()> {
    Ok(())
}

#[cfg(windows)]
mod imp {
    use std::fs;
    use std::path::PathBuf;

    use anyhow::{Context, Result};
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    const OUR_PREFIX: &str = "socks=127.0.0.1:";

    #[link(name = "wininet")]
    extern "system" {
        fn InternetSetOptionW(
            hinternet: *mut core::ffi::c_void,
            option: u32,
            buffer: *mut core::ffi::c_void,
            len: u32,
        ) -> i32;
    }

    /// Tell running apps (browsers etc.) that the proxy settings changed.
    fn notify_change() {
        const INTERNET_OPTION_REFRESH: u32 = 37;
        const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
        unsafe {
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                std::ptr::null_mut(),
                0,
            );
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_REFRESH,
                std::ptr::null_mut(),
                0,
            );
        }
    }

    fn backup_file() -> PathBuf {
        crate::data_dir().join("proxy-backup.txt")
    }

    pub fn enable(port: u16) -> Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(KEY_PATH)
            .context("could not open your Internet Settings")?;

        // Only take a backup if we don't already have one. A leftover backup means an earlier
        // run was killed, and that file holds the *real* original settings.
        let backup = backup_file();
        if !backup.exists() {
            let mut text = String::new();
            if let Ok(v) = key.get_value::<u32, _>("ProxyEnable") {
                text.push_str(&format!("enable={v}\n"));
            }
            if let Ok(v) = key.get_value::<String, _>("ProxyServer") {
                text.push_str(&format!("server={v}\n"));
            }
            if let Ok(v) = key.get_value::<String, _>("ProxyOverride") {
                text.push_str(&format!("override={v}\n"));
            }
            if let Some(dir) = backup.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&backup, text).context("could not save a backup of your proxy settings")?;
        }

        key.set_value("ProxyServer", &format!("{OUR_PREFIX}{port}"))?;
        key.set_value("ProxyOverride", &"<local>".to_string())?;
        key.set_value("ProxyEnable", &1u32)?;
        notify_change();
        Ok(())
    }

    pub fn restore() -> Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(KEY_PATH)
            .context("could not open your Internet Settings")?;

        let backup = backup_file();
        match fs::read_to_string(&backup) {
            Ok(text) => {
                let (mut enable, mut server, mut overrides) =
                    (None::<u32>, None::<String>, None::<String>);
                for line in text.lines() {
                    match line.split_once('=') {
                        Some(("enable", v)) => enable = v.parse().ok(),
                        Some(("server", v)) => server = Some(v.to_string()),
                        Some(("override", v)) => overrides = Some(v.to_string()),
                        _ => {}
                    }
                }
                key.set_value("ProxyEnable", &enable.unwrap_or(0))?;
                match server {
                    Some(s) => key.set_value("ProxyServer", &s)?,
                    None => {
                        let _ = key.delete_value("ProxyServer");
                    }
                }
                match overrides {
                    Some(o) => key.set_value("ProxyOverride", &o)?,
                    None => {
                        let _ = key.delete_value("ProxyOverride");
                    }
                }
                let _ = fs::remove_file(&backup);
            }
            Err(_) => {
                // No backup: only switch the proxy off if it is clearly ours. Never wipe a
                // proxy the user (or their workplace) configured themselves.
                let current: String = key.get_value("ProxyServer").unwrap_or_default();
                if current.starts_with(OUR_PREFIX) {
                    key.set_value("ProxyEnable", &0u32)?;
                }
            }
        }
        notify_change();
        Ok(())
    }
}
