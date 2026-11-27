// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a zip file, as Notion exports one: entries stored or deflated, Zip64 for big
//! exports, and the export split into `Part-N.zip` files inside the outer zip.
//!
//! Only reading, and only what an export needs: no encryption, no other compression methods.
//! Each entry is read when asked for, so a large export isn't all held in memory at once.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The largest entry read (a single file in an export).
const MAX_ENTRY: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    #[error("not a zip file")]
    NotZip,
    #[error("the zip file is damaged: {0}")]
    Damaged(&'static str),
    #[error("{0} is compressed in a way Drusite can't read (method {1})")]
    Method(String, u16),
    #[error("{0} is too big to import")]
    TooBig(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    method: u16,
    compressed: u64,
    size: u64,
    offset: u64,
}

#[derive(Debug)]
enum Data {
    Mem(Vec<u8>),
    File(PathBuf),
}

impl Data {
    fn len(&self) -> Result<u64, ZipError> {
        Ok(match self {
            Self::Mem(b) => b.len() as u64,
            Self::File(p) => std::fs::metadata(p)?.len(),
        })
    }

    fn read(&self, at: u64, len: u64) -> Result<Vec<u8>, ZipError> {
        let len = usize::try_from(len).map_err(|_| ZipError::Damaged("an entry is too long"))?;
        match self {
            Self::Mem(b) => {
                let at = usize::try_from(at).map_err(|_| ZipError::Damaged("bad offset"))?;
                b.get(at..at.checked_add(len).ok_or(ZipError::Damaged("bad length"))?)
                    .map(<[u8]>::to_vec)
                    .ok_or(ZipError::Damaged("an entry runs past the end"))
            }
            Self::File(p) => {
                let mut f = std::fs::File::open(p)?;
                f.seek(SeekFrom::Start(at))?;
                let mut out = vec![0; len];
                f.read_exact(&mut out)?;
                Ok(out)
            }
        }
    }
}

/// An open zip file.
#[derive(Debug)]
pub struct Zip {
    data: Data,
    entries: Vec<Entry>,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(a)
}

impl Zip {
    pub fn open(path: &Path) -> Result<Self, ZipError> {
        Self::new(Data::File(path.to_path_buf()))
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ZipError> {
        Self::new(Data::Mem(bytes))
    }

    fn new(data: Data) -> Result<Self, ZipError> {
        let len = data.len()?;
        if len < 22 {
            return Err(ZipError::NotZip);
        }
        // The end of central directory record is in the last 64 KiB + 22 bytes.
        let tail_len = len.min(65_557);
        let tail = data.read(len - tail_len, tail_len)?;
        let eocd = (0..=tail.len() - 22)
            .rev()
            .find(|&i| u32_at(&tail, i) == 0x0605_4b50)
            .ok_or(ZipError::NotZip)?;
        let mut count = u64::from(u16_at(&tail, eocd + 10));
        let mut cd_size = u64::from(u32_at(&tail, eocd + 12));
        let mut cd_at = u64::from(u32_at(&tail, eocd + 16));
        // Zip64: the locator sits just before the end record.
        if eocd >= 20 && u32_at(&tail, eocd - 20) == 0x0706_4b50 {
            let at = u64_at(&tail, eocd - 20 + 8);
            let rec = data.read(at, 56)?;
            if u32_at(&rec, 0) != 0x0606_4b50 {
                return Err(ZipError::Damaged("bad Zip64 record"));
            }
            count = u64_at(&rec, 32);
            cd_size = u64_at(&rec, 40);
            cd_at = u64_at(&rec, 48);
        }
        if cd_at.checked_add(cd_size).is_none_or(|e| e > len) {
            return Err(ZipError::Damaged("the directory runs past the end"));
        }
        let cd = data.read(cd_at, cd_size)?;
        let mut entries = Vec::new();
        let mut i = 0usize;
        for _ in 0..count {
            if i + 46 > cd.len() || u32_at(&cd, i) != 0x0201_4b50 {
                return Err(ZipError::Damaged("bad directory entry"));
            }
            let method = u16_at(&cd, i + 10);
            let mut compressed = u64::from(u32_at(&cd, i + 20));
            let mut size = u64::from(u32_at(&cd, i + 24));
            let name_len = usize::from(u16_at(&cd, i + 28));
            let extra_len = usize::from(u16_at(&cd, i + 30));
            let comment_len = usize::from(u16_at(&cd, i + 32));
            let mut offset = u64::from(u32_at(&cd, i + 42));
            let end = i + 46 + name_len + extra_len + comment_len;
            if end > cd.len() {
                return Err(ZipError::Damaged("bad directory entry"));
            }
            let name_bytes = &cd[i + 46..i + 46 + name_len];
            let name = String::from_utf8_lossy(name_bytes).replace('\\', "/");
            // Zip64 sizes and offset, in the extra field, for the ones marked 0xFFFFFFFF.
            let mut extra = &cd[i + 46 + name_len..i + 46 + name_len + extra_len];
            while extra.len() >= 4 {
                let id = u16_at(extra, 0);
                let n = usize::from(u16_at(extra, 2));
                let body = &extra[4..(4 + n).min(extra.len())];
                if id == 1 {
                    let mut at = 0;
                    let mut take = |v: &mut u64| {
                        if *v == 0xFFFF_FFFF && at + 8 <= body.len() {
                            *v = u64_at(body, at);
                            at += 8;
                        }
                    };
                    take(&mut size);
                    take(&mut compressed);
                    take(&mut offset);
                }
                extra = &extra[(4 + n).min(extra.len())..];
            }
            if !name.ends_with('/') {
                entries.push(Entry {
                    name,
                    method,
                    compressed,
                    size,
                    offset,
                });
            }
            i = end;
        }
        Ok(Self { data, entries })
    }

    /// Every file's path, `/` between parts.
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// A file's bytes.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, ZipError> {
        let e = self
            .entries
            .iter()
            .find(|e| e.name == name)
            .ok_or(ZipError::Damaged("no such entry"))?;
        if e.size > MAX_ENTRY || e.compressed > MAX_ENTRY {
            return Err(ZipError::TooBig(e.name.clone()));
        }
        let header = self.data.read(e.offset, 30)?;
        if u32_at(&header, 0) != 0x0403_4b50 {
            return Err(ZipError::Damaged("bad local header"));
        }
        let skip = 30 + u64::from(u16_at(&header, 26)) + u64::from(u16_at(&header, 28));
        let raw = self.data.read(e.offset + skip, e.compressed)?;
        let limit = usize::try_from(e.size).unwrap_or(usize::MAX);
        match e.method {
            0 => Ok(raw),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(&raw, limit.max(1))
                .map_err(|_| ZipError::Damaged("an entry won't inflate")),
            m => Err(ZipError::Method(e.name.clone(), m)),
        }
    }
}

/// A Notion export: one zip, or one zip of `Part-N.zip`s, or the folder it unzipped to. Paths
/// are relative, with `/`.
#[derive(Debug)]
pub enum Export {
    Zips(Vec<Zip>),
    Folder(PathBuf),
}

impl Export {
    pub fn open(path: &Path) -> Result<Self, ZipError> {
        if path.is_dir() {
            return Ok(Self::Folder(path.to_path_buf()));
        }
        Self::from_zip(Zip::open(path)?)
    }

    pub fn from_zip(zip: Zip) -> Result<Self, ZipError> {
        let names = zip.names();
        let parts: Vec<&String> = names
            .iter()
            .filter(|n| n.to_lowercase().ends_with(".zip"))
            .collect();
        if !parts.is_empty() && parts.len() == names.len() {
            let mut zips = Vec::new();
            for p in parts {
                zips.push(Zip::from_bytes(zip.read(p)?)?);
            }
            return Ok(Self::Zips(zips));
        }
        Ok(Self::Zips(vec![zip]))
    }

    pub fn names(&self) -> Vec<String> {
        match self {
            Self::Zips(z) => {
                let mut all: Vec<String> = z.iter().flat_map(Zip::names).collect();
                all.sort();
                all.dedup();
                all
            }
            Self::Folder(root) => {
                let mut out = Vec::new();
                let mut dirs = vec![(root.clone(), String::new())];
                while let Some((dir, rel)) = dirs.pop() {
                    let Ok(read) = std::fs::read_dir(&dir) else {
                        continue;
                    };
                    for entry in read.flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with('.') {
                            continue;
                        }
                        let path = if rel.is_empty() {
                            name
                        } else {
                            format!("{rel}/{name}")
                        };
                        match entry.file_type() {
                            Ok(t) if t.is_dir() => dirs.push((entry.path(), path)),
                            Ok(t) if t.is_file() => out.push(path),
                            _ => {}
                        }
                    }
                }
                out.sort();
                out
            }
        }
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, ZipError> {
        match self {
            Self::Zips(z) => {
                for zip in z {
                    if zip.entries.iter().any(|e| e.name == name) {
                        return zip.read(name);
                    }
                }
                Err(ZipError::Damaged("no such entry"))
            }
            Self::Folder(root) => {
                let path = root.join(name);
                if std::fs::metadata(&path)?.len() > MAX_ENTRY {
                    return Err(ZipError::TooBig(name.to_owned()));
                }
                Ok(std::fs::read(path)?)
            }
        }
    }
}

/// A zip of `files`, stored or deflated: for tests, which need export-shaped zips.
#[doc(hidden)]
pub fn write_zip(files: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let body = if deflate {
            miniz_oxide::deflate::compress_to_vec(data, 6)
        } else {
            data.to_vec()
        };
        let method: u16 = if deflate { 8 } else { 0 };
        let offset = u32::try_from(out.len()).unwrap_or(0);
        let push_common = |v: &mut Vec<u8>| {
            v.extend(20u16.to_le_bytes());
            v.extend(0x0800u16.to_le_bytes());
            v.extend(method.to_le_bytes());
            v.extend([0u8; 4]);
            v.extend(0u32.to_le_bytes());
            v.extend(u32::try_from(body.len()).unwrap_or(0).to_le_bytes());
            v.extend(u32::try_from(data.len()).unwrap_or(0).to_le_bytes());
            v.extend(u16::try_from(name.len()).unwrap_or(0).to_le_bytes());
            v.extend(0u16.to_le_bytes());
        };
        out.extend(0x0403_4b50u32.to_le_bytes());
        push_common(&mut out);
        out.extend(name.as_bytes());
        out.extend(&body);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        push_common(&mut central);
        central.extend([0u8; 6]);
        central.extend(0u32.to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let cd_at = u32::try_from(out.len()).unwrap_or(0);
    let n = u16::try_from(files.len()).unwrap_or(0);
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(n.to_le_bytes());
    out.extend(n.to_le_bytes());
    out.extend(u32::try_from(central.len()).unwrap_or(0).to_le_bytes());
    out.extend(cd_at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn reads_stored_and_deflated_entries_and_part_zips() {
        for deflate in [false, true] {
            let z = Zip::from_bytes(write_zip(
                &[("a/b.md", b"# Hello\n"), ("a/c.csv", b"Name\nx\n")],
                deflate,
            ))
            .unwrap();
            assert_eq!(z.names(), ["a/b.md", "a/c.csv"]);
            assert_eq!(z.read("a/b.md").unwrap(), b"# Hello\n");
        }
        let inner = write_zip(&[("x.md", b"x")], true);
        let outer = write_zip(&[("Export-Part-1.zip", &inner)], false);
        let e = Export::from_zip(Zip::from_bytes(outer).unwrap()).unwrap();
        assert_eq!(e.names(), ["x.md"]);
        assert_eq!(e.read("x.md").unwrap(), b"x");
        assert!(Zip::from_bytes(b"not a zip at all, honestly".to_vec()).is_err());
    }
}
