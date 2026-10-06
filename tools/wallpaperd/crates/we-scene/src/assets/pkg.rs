//! Derived from Yueosa/lianpkg@64b75310730e78a3aa8b1aed427e924121da1854.
use anyhow::{Result, anyhow};
use std::io::Read;

pub(super) struct PkgInfo {
    pub version: String,
    pub entries: Vec<PkgEntry>,
    pub data_start: usize,
}

pub(super) struct PkgEntry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
}

/// Parse only the PKG index; payloads are read by validated range on demand.
pub(super) fn parse_pkg(reader: impl Read, length: usize) -> Result<PkgInfo> {
    let mut r = Reader {
        reader,
        pos: 0,
        length,
    };

    // 读取版本
    let version = r.read_string()?;

    // 读取文件数量
    let file_count = r.read_u32()?;

    // 合理性检查：防止恶意/损坏文件导致巨量内存分配
    if file_count > 100_000 {
        return Err(anyhow!(
            "Invalid data: file_count {} exceeds limit 100000",
            file_count
        ));
    }

    // 读取文件条目
    let mut entries = Vec::with_capacity(file_count as usize);
    for _ in 0..file_count {
        let name = r.read_string()?;
        let offset = r.read_u32()?;
        let size = r.read_u32()?;
        entries.push(PkgEntry { name, offset, size });
    }

    // 记录数据区起始位置
    let data_start = r.position();

    // 验证所有 entry 的 offset+size 不超出 data 范围
    for entry in &entries {
        let abs_end = data_start
            .checked_add(entry.offset as usize)
            .and_then(|start| start.checked_add(entry.size as usize))
            .ok_or_else(|| anyhow!("PKG entry offset overflow"))?;
        if abs_end > length {
            return Err(anyhow!(
                "Invalid data: entry '{}': data_start({}) + offset({}) + size({}) = {} exceeds data length({})",
                entry.name,
                data_start,
                entry.offset,
                entry.size,
                abs_end,
                length
            ));
        }
    }

    Ok(PkgInfo {
        version,
        entries,
        data_start,
    })
}

/// 二进制数据读取器
struct Reader<R> {
    reader: R,
    pos: usize,
    length: usize,
}

impl<R: Read> Reader<R> {
    /// 获取当前读取位置
    fn position(&self) -> usize {
        self.pos
    }

    /// 读取 u32（小端序）
    ///
    /// 越界时返回错误
    fn read_u32(&mut self) -> Result<u32> {
        if self.length.saturating_sub(self.pos) < 4 {
            return Err(anyhow!(
                "Invalid data: read_u32: need 4 bytes at offset {}, but buffer length is {}",
                self.pos,
                self.length
            ));
        }
        let mut bytes = [0; 4];
        self.reader.read_exact(&mut bytes)?;
        self.pos += 4;
        Ok(u32::from_le_bytes(bytes))
    }

    /// 读取字符串（长度前缀 u32 + UTF-8 内容）
    ///
    /// 越界或非法 UTF-8 时返回错误
    fn read_string(&mut self) -> Result<String> {
        let len = self.read_u32()? as usize;
        if len > self.length.saturating_sub(self.pos) {
            return Err(anyhow!(
                "Invalid data: read_string: need {} bytes at offset {}, but buffer length is {}",
                len,
                self.pos,
                self.length
            ));
        }
        let mut bytes = vec![0; len];
        self.reader.read_exact(&mut bytes)?;
        let s = String::from_utf8(bytes).map_err(|e| {
            anyhow!(
                "Invalid data: read_string: invalid UTF-8 at offset {}: {}",
                self.pos,
                e
            )
        })?;
        self.pos += len;
        Ok(s)
    }
}
