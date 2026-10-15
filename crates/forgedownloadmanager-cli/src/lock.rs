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