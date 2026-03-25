use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Mutex,
};

use {
    crate::{BlockDevice, StorageError},
    log::trace,
};

/// A block device backed by a regular file, for use in std environments.
pub struct FileBlockDevice {
    file: Mutex<File>,
    capacity: u64,
}

impl FileBlockDevice {
    /// Open or create a file-backed block device.
    ///
    /// If `capacity` is 0, uses the existing file size (for opening existing disks).
    /// Otherwise, extends the file to `capacity` bytes if needed.
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
        })
    }

    /// Create a file-backed block device from an already-open file.
    pub fn from_file(file: File, capacity: u64) -> Result<Self, StorageError> {
        let meta = file.metadata()?;
        if meta.len() < capacity {
            file.set_len(capacity)?;
        }

        Ok(Self {
            file: Mutex::new(file),
            capacity,
        })
    }
}

impl BlockDevice for FileBlockDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        trace!(
            "block_device::read_at offset={:#x} len={}",
            offset,
            buf.len()
        );
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
        trace!(
            "block_device::write_at offset={:#x} len={}",
            offset,
            buf.len()
        );
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
        trace!("block_device::sync");
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
        let data = b"hello mimisbrunnr";
        dev.write_at(0, data).unwrap();

        let mut buf = vec![0u8; data.len()];
        dev.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, data);
    }

    #[test]
    fn write_at_offset() {
        let (_tmp, dev) = test_device(4096);
        let data = b"world";
        dev.write_at(100, data).unwrap();

        let mut buf = vec![0u8; data.len()];
        dev.read_at(100, &mut buf).unwrap();
        assert_eq!(&buf, data);
    }

    #[test]
    fn read_out_of_bounds() {
        let (_tmp, dev) = test_device(4096);
        let mut buf = vec![0u8; 10];
        let err = dev.read_at(4090, &mut buf).unwrap_err();
        assert!(matches!(err, StorageError::OutOfBounds { .. }));
    }

    #[test]
    fn write_out_of_bounds() {
        let (_tmp, dev) = test_device(4096);
        let data = vec![0u8; 10];
        let err = dev.write_at(4090, &data).unwrap_err();
        assert!(matches!(err, StorageError::OutOfBounds { .. }));
    }

    #[test]
    fn capacity_correct() {
        let (_tmp, dev) = test_device(8192);
        assert_eq!(dev.capacity(), 8192);
    }

    #[test]
    fn sync_succeeds() {
        let (_tmp, dev) = test_device(4096);
        dev.sync().unwrap();
    }

    #[test]
    fn multiple_writes_dont_interfere() {
        let (_tmp, dev) = test_device(4096);
        dev.write_at(0, b"aaaa").unwrap();
        dev.write_at(100, b"bbbb").unwrap();

        let mut a = [0u8; 4];
        let mut b = [0u8; 4];
        dev.read_at(0, &mut a).unwrap();
        dev.read_at(100, &mut b).unwrap();
        assert_eq!(&a, b"aaaa");
        assert_eq!(&b, b"bbbb");
    }
}
