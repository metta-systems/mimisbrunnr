//! File-backed [`BlockDevice`] implementation.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Mutex,
};

use {
    crate::{block_device::BlockDevice, error::StorageError},
    log::trace,
};

/// A block device backed by a regular file.
pub struct FileBlockDevice {
    file: Mutex<File>,
    capacity: u64,
    read_only: bool,
}

impl FileBlockDevice {
    /// Open or create a file-backed block device.
    ///
    /// If `capacity` is 0, uses the existing file size (for opening existing
    /// disks). Otherwise extends the file to `capacity` bytes if needed.
    pub fn open(path: &Path, capacity: u64) -> Result<Self, StorageError> {
        trace!(
            "FileBlockDevice::open path={} capacity={}",
            path.display(),
            capacity
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        let meta = file.metadata()?;
        let actual_capacity = if capacity == 0 {
            meta.len()
        } else {
            if meta.len() < capacity {
                file.set_len(capacity)?;
            }
            capacity
        };

        Ok(Self {
            file: Mutex::new(file),
            capacity: actual_capacity,
            read_only: false,
        })
    }

    /// Open an existing file-backed block device read-only. Any subsequent
    /// `write_at` / `sync` call returns [`StorageError::ReadOnly`]; reads
    /// proceed normally.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        trace!("FileBlockDevice::open_read_only path={}", path.display());
        let file = OpenOptions::new().read(true).open(path)?;
        let meta = file.metadata()?;
        Ok(Self {
            file: Mutex::new(file),
            capacity: meta.len(),
            read_only: true,
        })
    }

    /// `true` if the device was opened read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Build a [`FileBlockDevice`] from an already-open file.
    pub fn from_file(file: File, capacity: u64) -> Result<Self, StorageError> {
        let meta = file.metadata()?;
        if meta.len() < capacity {
            file.set_len(capacity)?;
        }
        Ok(Self {
            file: Mutex::new(file),
            capacity,
            read_only: false,
        })
    }
}

impl BlockDevice for FileBlockDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        let end = offset + buf.len() as u64;
        if end > self.capacity {
            return Err(StorageError::OutOfBounds {
                offset,
                length: buf.len() as u64,
                capacity: self.capacity,
            });
        }
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)?;
        Ok(())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
        if self.read_only {
            return Err(StorageError::ReadOnly);
        }
        let end = offset + buf.len() as u64;
        if end > self.capacity {
            return Err(StorageError::OutOfBounds {
                offset,
                length: buf.len() as u64,
                capacity: self.capacity,
            });
        }
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(buf)?;
        Ok(())
    }

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn sync(&self) -> Result<(), StorageError> {
        if self.read_only {
            return Err(StorageError::ReadOnly);
        }
        let file = self.file.lock().unwrap();
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, tempfile::NamedTempFile};

    fn test_device(capacity: u64) -> (NamedTempFile, FileBlockDevice) {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), capacity).unwrap();
        (tmp, dev)
    }

    #[test]
    fn write_and_read_back() {
        let (_tmp, dev) = test_device(4096);
        dev.write_at(0, b"hello").unwrap();
        let mut buf = [0u8; 5];
        dev.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn read_out_of_bounds_errors() {
        let (_tmp, dev) = test_device(4096);
        let mut buf = [0u8; 10];
        let err = dev.read_at(4090, &mut buf).unwrap_err();
        assert!(matches!(err, StorageError::OutOfBounds { .. }));
    }

    #[test]
    fn capacity_reported() {
        let (_tmp, dev) = test_device(8192);
        assert_eq!(dev.capacity(), 8192);
    }

    #[test]
    fn sync_succeeds() {
        let (_tmp, dev) = test_device(4096);
        dev.sync().unwrap();
    }

    #[test]
    fn open_read_only_rejects_writes_and_syncs() {
        let tmp = NamedTempFile::new().unwrap();
        // First open RW, write a marker, drop.
        {
            let dev = FileBlockDevice::open(tmp.path(), 4096).unwrap();
            dev.write_at(0, b"hello").unwrap();
            dev.sync().unwrap();
        }
        // Now reopen read-only.
        let ro = FileBlockDevice::open_read_only(tmp.path()).unwrap();
        assert!(ro.is_read_only());
        assert_eq!(ro.capacity(), 4096);

        // Read still works.
        let mut buf = [0u8; 5];
        ro.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");

        // Writes are rejected with ReadOnly.
        let err = ro.write_at(0, b"nope").unwrap_err();
        assert!(matches!(err, StorageError::ReadOnly));
        let err = ro.sync().unwrap_err();
        assert!(matches!(err, StorageError::ReadOnly));
    }
}
