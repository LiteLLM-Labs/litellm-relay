use std::{
    env, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use uuid::Uuid;

pub fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

/// Creates an owner-only file that did not exist before and hands it to whoever
/// owns its directory. Relay's root daemon runs with HOME pinned to the
/// developer's home, so a root-owned secret would lock the developer's own
/// commands out, and creating exclusively keeps root from writing through a
/// symlink planted in that user-writable directory.
#[cfg(unix)]
pub fn create_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::{fchown, MetadataExt, OpenOptionsExt};
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let owner = fs::metadata(parent)?;
        if owner.uid() != file.metadata()?.uid() {
            fchown(&file, Some(owner.uid()), Some(owner.gid()))?;
        }
    }
    Ok(file)
}

#[cfg(not(unix))]
pub fn create_private(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Replaces `path` with an owner-only file holding `contents`, staged next to
/// it and renamed into place so a reader never sees a partial write.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let staged = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let written = create_private(&staged)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .and_then(|()| fs::rename(&staged, path));
    if written.is_err() {
        let _ = fs::remove_file(&staged);
    }
    written.with_context(|| format!("failed to write {}", path.display()))
}

pub fn lock_private(path: &Path) -> Result<fs::File> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    match create_private(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to create {}", path.display()))
        }
    }
    let file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.lock()
        .with_context(|| format!("failed to lock {}", path.display()))?;
    Ok(file)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn scratch_dir() -> PathBuf {
        let dir = env::temp_dir().join(format!("relay-system-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn should_write_an_owner_only_file_and_replace_it_on_the_next_write() {
        let dir = scratch_dir();
        let path = dir.join("secret.json");

        write_private(&path, "first").unwrap();
        write_private(&path, "second").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("secret.json")]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_replace_a_planted_symlink_instead_of_writing_through_it() {
        let dir = scratch_dir();
        let victim = dir.join("victim");
        let path = dir.join("secret.json");
        fs::write(&victim, "untouched").unwrap();
        symlink(&victim, &path).unwrap();

        write_private(&path, "secret").unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_refuse_to_create_over_an_existing_path() {
        let dir = scratch_dir();
        let victim = dir.join("victim");
        let planted = dir.join("planted");
        fs::write(&victim, "untouched").unwrap();
        symlink(&victim, &planted).unwrap();

        let error = create_private(&planted).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
        fs::remove_dir_all(&dir).unwrap();
    }
}
