use std::fs::File;
use std::io;

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
