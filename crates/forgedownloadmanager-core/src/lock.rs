//! Exclusive engine owner.
//!
//! `engine.lock.guard` is the OS file lock. `engine.lock` is `pid\nport\ntoken\n`,
//! written only after the loopback listener is bound. The desktop and the CLI share
//! this protocol so a second process forwards instead of opening the database.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Holds the exclusive engine lock until dropped.
pub struct Owner {
    _guard: File,
    pub data_dir: PathBuf,
}

pub enum Attach {
    Owner(Owner),
    Forward { addr: SocketAddr, token: String },
}

pub fn attach(data_dir: &Path) -> io::Result<Attach> {
    std::fs::create_dir_all(data_dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(data_dir.join("engine.lock.guard"))?;
    match file.try_lock() {
        Ok(()) => Ok(Attach::Owner(Owner { _guard: file, data_dir: data_dir.to_path_buf() })),
        Err(TryLockError::WouldBlock) => {
            let (addr, token) = wait_for_info(data_dir)?;
            Ok(Attach::Forward { addr, token })
        }
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// `Some` when another process owns the engine. Does not keep the lock.
pub fn supervisor(data_dir: &Path) -> io::Result<Option<(SocketAddr, String)>> {
    let guard = data_dir.join("engine.lock.guard");
    if !guard.exists() {
        return Ok(None);
    }
    let file = OpenOptions::new().read(true).write(true).open(guard)?;
    match file.try_lock() {
        Ok(()) => Ok(None),
        Err(TryLockError::WouldBlock) => Ok(Some(wait_for_info(data_dir)?)),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

pub fn write_info(data_dir: &Path, port: u16, token: &str) -> io::Result<()> {
    let text = format!("{}\n{port}\n{token}\n", std::process::id());
    let tmp = data_dir.join("engine.lock.tmp");
    {
        let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_data()?;
    }
    let dest = data_dir.join("engine.lock");
    if dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
    std::fs::rename(&tmp, &dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn clear_info(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join("engine.lock"));
}

/// Platform data directory. `FORGE_DM_DATA` wins over the OS path.
pub fn data_dir() -> PathBuf {
    resolve_data_dir(&DataDirEnv {
        forge_dm_data: std::env::var_os("FORGE_DM_DATA").map(PathBuf::from),
        local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        home: std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from),
        xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
    })
}

/// Inputs for [`resolve_data_dir`]. Tests pass these without changing the process environment.
#[derive(Debug, Default)]
pub struct DataDirEnv {
    pub forge_dm_data: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
}

pub fn resolve_data_dir(env: &DataDirEnv) -> PathBuf {
    if let Some(dir) = &env.forge_dm_data {
        return dir.clone();
    }
    if cfg!(windows) {
        if let Some(local) = &env.local_app_data {
            return local.join("ForgeDownloadManager");
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = &env.home {
            return home.join("Library").join("Application Support").join("ForgeDownloadManager");
        }
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        if let Some(xdg) = &env.xdg_data_home {
            return xdg.join("forgedownloadmanager");
        }
        if let Some(home) = &env.home {
            return home.join(".local").join("share").join("forgedownloadmanager");
        }
    }
    PathBuf::from("forgedownloadmanager-data")
}

/// The user's Downloads folder when it exists, otherwise [`data_dir`].
pub fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(home) = home {
        let downloads = PathBuf::from(home).join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
    }
    data_dir()
}

fn wait_for_info(data_dir: &Path) -> io::Result<(SocketAddr, String)> {
    let path = data_dir.join("engine.lock");
    for _ in 0..100 {
        if let Some(parsed) = read_info(&path) {
            return Ok(parsed);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "engine lock is held but engine.lock has no listener yet",
    ))
}

fn read_info(path: &Path) -> Option<(SocketAddr, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let _pid = lines.next()?;
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let token = lines.next()?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    Some((SocketAddr::from((Ipv4Addr::LOCALHOST, port)), token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_dm_data_wins() {
        let env = DataDirEnv {
            forge_dm_data: Some(PathBuf::from("custom")),
            local_app_data: Some(PathBuf::from("local")),
            home: Some(PathBuf::from("home")),
            xdg_data_home: Some(PathBuf::from("xdg")),
        };
        assert_eq!(resolve_data_dir(&env), PathBuf::from("custom"));
    }

    #[test]
    fn platform_dir_when_forge_is_unset() {
        let env = DataDirEnv {
            forge_dm_data: None,
            local_app_data: Some(PathBuf::from("local")),
            home: Some(PathBuf::from("home")),
            xdg_data_home: Some(PathBuf::from("xdg")),
        };
        let resolved = resolve_data_dir(&env);
        if cfg!(windows) {
            assert_eq!(resolved, PathBuf::from("local").join("ForgeDownloadManager"));
        } else if cfg!(target_os = "macos") {
            assert_eq!(
                resolved,
                PathBuf::from("home").join("Library").join("Application Support").join("ForgeDownloadManager")
            );
        } else {
            assert_eq!(resolved, PathBuf::from("xdg").join("forgedownloadmanager"));
        }
    }

    #[test]
    fn missing_platform_dir_falls_back() {
        assert_eq!(resolve_data_dir(&DataDirEnv::default()), PathBuf::from("forgedownloadmanager-data"));
    }
}
