use std::{
    fs::{self, Metadata, OpenOptions},
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
pub fn redirected(md: &Metadata) -> bool {
    md.file_type().is_symlink()
}
pub fn private_dir(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
pub fn open_options(options: &mut OpenOptions) {
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}
pub fn sync_dir(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

// Sync only metadata/content buffers; never read a clipboard payload.
pub fn sync_file(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    open_options(&mut options);
    options.open(path)?.sync_all()
}
