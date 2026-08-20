use std::fs::{self, Permissions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone)]
pub struct DataPaths {
    pub root: PathBuf,
    pub db: PathBuf,
    pub state: PathBuf,
    pub frames: PathBuf,
    pub crops: PathBuf,
}

impl DataPaths {
    pub fn bare(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            db: root.join("rewind.db"),
            state: root.join("state.json"),
            frames: root.join("frames"),
            crops: root.join("crops"),
        }
    }

    pub fn prepare(root: &Path) -> Result<Self, String> {
        install_umask();
        secure_dir(root).map_err(|e| e.to_string())?;
        let paths = Self::bare(root);
        secure_dir(&paths.frames).map_err(|e| e.to_string())?;
        secure_dir(&paths.crops).map_err(|e| e.to_string())?;
        Ok(paths)
    }
}

pub fn default_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("REWIND_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("rewind");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/rewind")
}

pub fn install_umask() {
    #[cfg(unix)]
    unsafe {
        libc::umask(0o077);
    }
}

pub fn secure_dir(path: &Path) -> io::Result<()> {
    if !path.exists() {
        #[cfg(unix)]
        {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(path)?;
        }
    }
    #[cfg(unix)]
    {
        fs::set_permissions(path, Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn secure_file(path: &Path) -> io::Result<()> {
    if !path.exists() {
        #[cfg(unix)]
        {
            let _ = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .mode(0o600)
                .open(path)?;
        }
        #[cfg(not(unix))]
        {
            let _ = fs::File::create(path)?;
        }
    }
    #[cfg(unix)]
    {
        fs::set_permissions(path, Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn assert_private_tree(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        check_mode(root, 0o700)?;
        if root.is_dir() {
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let path = entry.path();
                let ft = entry.file_type()?;
                if ft.is_dir() {
                    check_mode(&path, 0o700)?;
                    assert_private_tree(&path)?;
                } else if ft.is_file() {
                    check_mode(&path, 0o600)?;
                }
            }
        }
    }
    let _ = root;
    Ok(())
}

#[cfg(unix)]
fn check_mode(path: &Path, want: u32) -> io::Result<()> {
    let meta = fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode != want {
        return Err(io::Error::other(format!(
            "{} mode {:o} != {:o}",
            path.display(),
            mode,
            want
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tree_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("rewind");
        let paths = DataPaths::prepare(&root).unwrap();
        std::fs::write(&paths.state, "{}").unwrap();
        secure_file(&paths.state).unwrap();
        assert_private_tree(&root).unwrap();
    }
}
