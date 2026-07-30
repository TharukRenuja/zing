use std::fs::File;
use std::io;

/// Pre-allocate space for a file on disk.
/// Uses `fallocate` on Linux, `F_PREALLOCATE` on macOS, and falls back
/// to `set_len` (sparse files) on other platforms.
pub fn preallocate(file: &File, len: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let ret = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, len as libc::off_t) };
        if ret == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // If fallocate is not supported by the filesystem, fall back to set_len
        if err.raw_os_error() == Some(libc::EOPNOTSUPP) || err.raw_os_error() == Some(libc::ENOSYS)
        {
            file.set_len(len)
        } else {
            Err(err)
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = &file;
        file.set_len(len)
    }
}

/// Write all bytes at a given offset, regardless of platform.
/// Uses `pwrite` on Unix and `seek_write` on Windows.
pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buf, offset)
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        file.seek_write(buf, offset)?;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        use std::io::{Seek, SeekFrom, Write};
        (&*file).seek(SeekFrom::Start(offset))?;
        (&*file).write_all(buf)
    }
}
