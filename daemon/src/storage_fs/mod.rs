//! Archive filesystem boundary; each platform owns redirect/open semantics.
//! Same-user paths remain trusted against deliberate concurrent replacement.
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
};
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

pub fn plain(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::other("absolute archive path required"));
    }
    // Windows normalizes `..` while iterating `Path::components()`. Inspect
    // the lexical spelling first so a redirected archive cannot hide behind
    // that normalization. Backslash is included for the cross-platform test
    // and is not a valid separator in Unix archive names used by the daemon.
    if path
        .as_os_str()
        .to_string_lossy()
        .split(['/', '\\'])
        .any(|part| part == "..")
    {
        return Err(io::Error::other("archive parent traversal refused"));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        if matches!(part, Component::ParentDir) {
            return Err(io::Error::other("archive parent traversal refused"));
        }
        current.push(part);
        if matches!(part, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(md) if platform::redirected(&md) => {
                return Err(io::Error::other("redirected archive path refused"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
pub fn private_dir(path: &Path) -> io::Result<()> {
    plain(path)?;
    fs::create_dir_all(path)?;
    platform::private_dir(path)
}
pub fn regular(path: &Path) -> io::Result<()> {
    plain(path)?;
    match fs::symlink_metadata(path) {
        Ok(md) if !md.is_file() => Err(io::Error::other("archive file must be regular")),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
pub fn private_open(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    regular(path)?;
    platform::open_options(options);
    options.open(path)
}
pub fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing archive parent"))?;
    plain(parent)?;
    platform::sync_dir(parent)
}
pub fn sync_directory(path: &Path) -> io::Result<()> {
    plain(path)?;
    platform::sync_dir(path)
}
pub fn sync_file(path: &Path) -> io::Result<()> {
    regular(path)?;
    platform::sync_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_parent_traversal_and_non_regular_archive_files_are_refused() {
        let f = crate::testing::TempDir::new();
        assert!(plain(Path::new("relative-archive")).is_err());
        let traversal = PathBuf::from(format!(r"{}\sub\..\archive", f.path().display()));
        assert!(plain(&traversal).is_err());
        assert!(regular(f.path()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn private_writes_and_redirected_ancestors_are_checked() {
        use std::os::unix::fs::PermissionsExt;
        let f = crate::testing::TempDir::new();
        let path = f.path().join("private");
        private_dir(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = path.join("entry");
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        private_open(&file, &mut options).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = f.path().join("redirect");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(private_dir(&link).is_err());
        assert!(regular(&link.join("entry")).is_err());
    }
}
