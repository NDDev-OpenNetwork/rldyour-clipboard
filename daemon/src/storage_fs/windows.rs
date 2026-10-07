use std::{
    fs::{Metadata, OpenOptions},
    io,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
pub fn redirected(md: &Metadata) -> bool {
    md.file_type().is_symlink() || md.file_attributes() & 0x400 != 0
}
pub fn private_dir(_path: &Path) -> io::Result<()> {
    Ok(())
} // inherited private user-profile ACL
pub fn open_options(options: &mut OpenOptions) {
    options.custom_flags(0x0020_0000);
} // FILE_FLAG_OPEN_REPARSE_POINT
pub fn sync_dir(_path: &Path) -> io::Result<()> {
    Ok(())
} // Windows has no portable directory-fsync contract; file buffers and SQLite WAL are flushed.

// Sync only metadata/content buffers; never read a clipboard payload.
pub fn sync_file(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    open_options(&mut options);
    options.open(path)?.sync_all()
}
