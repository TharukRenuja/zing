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

    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let mut store = libc::fp_allocstore {
            po_alloc: len as u64,
        };
        let mut spec = libc::fstore {
            fst_flags: libc::F_ALLOCATECONTIG,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: len as i64,
            fst_bytesalloc: 0,
        };
        let ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &spec) };
        if ret == 0 {
            return file.set_len(len);
        }
        // Try non-contiguous allocation
        spec.fst_flags = libc::F_ALLOCATEALL;
        let ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &spec) };
        if ret == 0 {
            return file.set_len(len);
        }
        file.set_len(len)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = &file;
        let _ = len;
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
