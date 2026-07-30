use std::path::{Path, PathBuf};

pub const BLOCK_SIZE: u64 = 65536;

#[derive(Debug, Clone)]
pub struct BlockBitfield {
    pub total_size: u64,
    pub block_size: u64,
    pub num_blocks: u32,
    bits: Vec<u8>,
}

impl BlockBitfield {
    pub fn new(total_size: u64, block_size: u64) -> Self {
        let num_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_size) as u32
        };
        let byte_len = if num_blocks == 0 {
            0
        } else {
            (num_blocks as usize).div_ceil(8)
        };
        Self {
            total_size,
            block_size,
            num_blocks,
            bits: vec![0u8; byte_len],
        }
    }

    pub fn byte_len(&self) -> usize {
        self.bits.len()
    }

    pub fn is_complete(&self, block_idx: u32) -> bool {
        if block_idx >= self.num_blocks {
            return true;
        }
        let byte = block_idx as usize / 8;
        let bit = block_idx as usize % 8;
        self.bits[byte] & (1u8 << bit) != 0
    }

    pub fn mark_complete(&mut self, block_idx: u32) {
        if block_idx >= self.num_blocks {
            return;
        }
        let byte = block_idx as usize / 8;
        let bit = block_idx as usize % 8;
        self.bits[byte] |= 1u8 << bit;
    }

    pub fn mark_incomplete(&mut self, block_idx: u32) {
        if block_idx >= self.num_blocks {
            return;
        }
        let byte = block_idx as usize / 8;
        let bit = block_idx as usize % 8;
        self.bits[byte] &= !(1u8 << bit);
    }

    pub fn blocks_for_range(&self, offset: u64, length: u64) -> (u32, u32) {
        let first = (offset / self.block_size) as u32;
        let last = ((offset + length - 1) / self.block_size) as u32;
        (first, last)
    }

    pub fn all_complete(&self) -> bool {
        self.total_downloaded() >= self.total_size
    }

    pub fn total_downloaded(&self) -> u64 {
        let full_blocks = self.count_set_bits() as u64;
        let full_bytes = full_blocks * self.block_size;
        if full_bytes > self.total_size {
            self.total_size
        } else {
            full_bytes
        }
    }

    fn count_set_bits(&self) -> u32 {
        self.bits.iter().map(|&b| b.count_ones()).sum()
    }

    pub fn progress_pct(&self) -> f64 {
        if self.total_size == 0 {
            return 0.0;
        }
        self.total_downloaded() as f64 / self.total_size as f64 * 100.0
    }

    pub fn missing_ranges(&self) -> Vec<(u64, u64)> {
        let mut ranges = Vec::new();
        let mut i = 0u32;
        while i < self.num_blocks {
            if !self.is_complete(i) {
                let start = i as u64 * self.block_size;
                let mut end = start + self.block_size;
                i += 1;
                while i < self.num_blocks && !self.is_complete(i) {
                    end = (i as u64 + 1) * self.block_size;
                    i += 1;
                }
                if end > self.total_size {
                    end = self.total_size;
                }
                ranges.push((start, end - start));
            } else {
                i += 1;
            }
        }
        ranges
    }

    pub fn remaining_blocks(&self) -> u32 {
        self.num_blocks - self.count_set_bits()
    }

    pub fn raw_bits(&self) -> &[u8] {
        &self.bits
    }

    pub fn raw_bits_mut(&mut self) -> &mut [u8] {
        &mut self.bits
    }
}

#[derive(Debug, Clone)]
pub struct ControlFile {
    pub version: u16,
    pub total_size: u64,
    pub block_size: u64,
    pub bitfield: BlockBitfield,
}

impl ControlFile {
    pub fn new(total_size: u64, block_size: u64) -> Self {
        Self {
            version: 2,
            total_size,
            block_size,
            bitfield: BlockBitfield::new(total_size, block_size),
        }
    }

    pub fn control_path(output_path: &Path) -> PathBuf {
        let mut p = output_path.to_path_buf();
        let mut name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "download".to_string());
        name.push_str(".zing");
        p.set_file_name(name);
        p
    }

    pub async fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(64 + self.bitfield.byte_len());
        buf.extend_from_slice(&[0x5A, 0x49]);
        buf.extend_from_slice(&self.version.to_be_bytes());
        buf.extend_from_slice(&self.total_size.to_be_bytes());
        buf.extend_from_slice(&(self.block_size as u32).to_be_bytes());
        let num_blocks = self.bitfield.num_blocks;
        buf.extend_from_slice(&num_blocks.to_be_bytes());
        let bf_len = self.bitfield.byte_len() as u32;
        buf.extend_from_slice(&bf_len.to_be_bytes());
        buf.extend_from_slice(self.bitfield.raw_bits());
        let tmp_path = path.with_extension("zing.tmp");
        tokio::fs::write(&tmp_path, &buf).await?;
        tokio::fs::rename(&tmp_path, path).await
    }

    pub async fn load(path: &Path) -> std::io::Result<Self> {
        let buf = tokio::fs::read(path).await?;
        if buf.len() < 22 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "control file too short",
            ));
        }
        if buf[0] != 0x5A || buf[1] != 0x49 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bad magic",
            ));
        }
        let version = u16::from_be_bytes([buf[2], buf[3]]);
        if !(1..=2).contains(&version) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported version {version}"),
            ));
        }
        let total_size = u64::from_be_bytes([
            buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
        ]);
        let block_size = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]) as u64;
        let _num_blocks = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let bf_len = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]) as usize;
        if buf.len() < 24 + bf_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "control file truncated",
            ));
        }
        let mut bitfield = BlockBitfield::new(total_size, block_size);
        if bf_len > 0 {
            bitfield
                .raw_bits_mut()
                .copy_from_slice(&buf[24..24 + bf_len]);
        }
        Ok(Self {
            version,
            total_size,
            block_size,
            bitfield,
        })
    }
}
