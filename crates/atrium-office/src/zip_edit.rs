//! Locality-oriented ZIP32 rewriting. Unchanged local records (including any
//! data descriptor) and central metadata are copied, with only offsets patched.
use crate::{Limits, package::PackageSaveError as SaveError};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
};

fn bad(s: impl std::fmt::Display) -> SaveError {
    SaveError::InvalidPart(s.to_string())
}
fn n16(b: &[u8], p: usize) -> Result<u16, SaveError> {
    Ok(u16::from_le_bytes(
        b.get(p..p + 2)
            .ok_or_else(|| bad("truncated ZIP"))?
            .try_into()
            .unwrap(),
    ))
}
fn n32(b: &[u8], p: usize) -> Result<u32, SaveError> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or_else(|| bad("truncated ZIP"))?
            .try_into()
            .unwrap(),
    ))
}
fn put32(b: &mut [u8], p: usize, v: u32) {
    b[p..p + 4].copy_from_slice(&v.to_le_bytes());
}
fn narrow(n: usize) -> Result<u32, SaveError> {
    n.try_into().map_err(|_| bad("ZIP32 overflow"))
}

struct Entry {
    central: usize,
    local: usize,
    data: usize,
    compressed: usize,
    descriptor: usize,
    changed: Option<(Vec<u8>, u32, u32)>,
}

pub(crate) fn rewrite(
    bytes: &[u8],
    replacements: &BTreeMap<&str, &[u8]>,
    limits: Limits,
) -> Result<Vec<u8>, SaveError> {
    let end = (bytes.len().saturating_sub(65557)..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            bytes.get(i..i + 4) == Some(b"PK\x05\x06")
                && n16(bytes, i + 20).is_ok_and(|n| i + 22 + usize::from(n) == bytes.len())
        })
        .ok_or_else(|| bad("missing ZIP end record"))?;
    let directory = n32(bytes, end + 16)? as usize;
    let mut p = directory;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(bad)?;
    if zip.len() != usize::from(n16(bytes, end + 10)?) {
        return Err(bad("ambiguous entries"));
    }
    let mut entries = Vec::with_capacity(zip.len());
    let mut total = 0u64;
    let mut new: Vec<(&str, &[u8])> = replacements
        .iter()
        .filter(|(name, _)| zip.index_for_name(name).is_none())
        .map(|(name, data)| (*name, *data))
        .collect();
    let count = zip
        .len()
        .checked_add(new.len())
        .ok_or_else(|| bad("entry count overflow"))?;
    if count > limits.entries || count >= 65535 {
        return Err(bad("entry count limit"));
    }
    for (_, data) in &new {
        total = total
            .checked_add(data.len() as u64)
            .ok_or_else(|| bad("inflate limit"))?;
    }
    if total > limits.inflated_bytes {
        return Err(bad("inflate limit"));
    }
    // Measure and validate every replacement before compression/output allocation.
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(bad)?;
        let size = replacements
            .get(entry.name())
            .map_or(entry.size(), |b| b.len() as u64);
        total = total
            .checked_add(size)
            .ok_or_else(|| bad("inflate limit"))?;
        if total > limits.inflated_bytes {
            return Err(bad("inflate limit"));
        }
    }
    let mut changed_count = 0;
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(bad)?;
        let local = n32(bytes, p + 42)? as usize;
        let compressed = n32(bytes, p + 20)? as usize;
        let name_len = usize::from(n16(bytes, p + 28)?);
        let next =
            p + 46 + name_len + usize::from(n16(bytes, p + 30)?) + usize::from(n16(bytes, p + 32)?);
        if entry.header_start() != local as u64
            || bytes.get(p..p + 4) != Some(b"PK\x01\x02")
            || next > end
            || bytes.get(local..local + 4) != Some(b"PK\x03\x04")
        {
            return Err(bad("ambiguous ZIP layout"));
        }
        let data = local
            + 30
            + usize::from(n16(bytes, local + 26)?)
            + usize::from(n16(bytes, local + 28)?);
        let mut descriptor = 0;
        let changed = if let Some(part) = replacements.get(entry.name()) {
            let flags = n16(bytes, p + 8)?;
            if flags != n16(bytes, local + 6)?
                || flags & (1 | 64) != 0
                || n16(bytes, p + 10)? != n16(bytes, local + 8)?
            {
                return Err(bad("edited entry has unsupported local flags"));
            }
            if flags & 8 != 0 {
                let pos = data
                    .checked_add(compressed)
                    .ok_or_else(|| bad("data descriptor overflow"))?;
                let crc = n32(bytes, p + 16)?;
                let check = |q: usize| -> bool {
                    n32(bytes, q).ok() == Some(crc)
                        && n32(bytes, q + 4).ok() == Some(compressed as u32)
                        && n32(bytes, q + 8).ok() == n32(bytes, p + 24).ok()
                };
                descriptor = if n32(bytes, pos)? == 0x08074b50 && check(pos + 4) {
                    16
                } else if check(pos) {
                    12
                } else {
                    return Err(bad("invalid data descriptor"));
                };
            }
            let comp = match entry.compression() {
                zip::CompressionMethod::Stored => part.to_vec(),
                zip::CompressionMethod::Deflated => {
                    let mut enc = flate2::write::DeflateEncoder::new(
                        Vec::new(),
                        flate2::Compression::default(),
                    );
                    enc.write_all(part)?;
                    enc.finish()?
                }
                _ => return Err(bad("edited entry compression unsupported")),
            };
            changed_count += 1;
            Some((comp, crc32fast::hash(part), narrow(part.len())?))
        } else {
            None
        };
        entries.push(Entry {
            central: p,
            local,
            data,
            compressed,
            descriptor,
            changed,
        });
        p = next;
    }
    if p != end || changed_count + new.len() != replacements.len() {
        return Err(bad("missing target part"));
    }
    let mut order = (0..entries.len()).collect::<Vec<_>>();
    order.sort_unstable_by_key(|&i| entries[i].local);
    let mut output = Vec::new();
    let mut offsets = vec![0; entries.len()];
    let mut cursor = 0;
    for i in order {
        let e = &entries[i];
        if e.local < cursor || e.data + e.compressed + e.descriptor > directory {
            return Err(bad("overlapping ZIP records"));
        }
        output.extend_from_slice(&bytes[cursor..e.local]);
        offsets[i] = narrow(output.len())?;
        let header = output.len();
        output.extend_from_slice(&bytes[e.local..e.data]);
        if let Some((data, crc, size)) = &e.changed {
            if e.descriptor == 0 {
                put32(&mut output, header + 14, *crc);
                put32(&mut output, header + 18, narrow(data.len())?);
                put32(&mut output, header + 22, *size);
            } else {
                output[header + 14..header + 26].fill(0);
            }
            output.extend_from_slice(data);
            if e.descriptor == 16 {
                output.extend_from_slice(b"PK\x07\x08");
            }
            if e.descriptor != 0 {
                output.extend_from_slice(&crc.to_le_bytes());
                output.extend_from_slice(&narrow(data.len())?.to_le_bytes());
                output.extend_from_slice(&size.to_le_bytes());
            }
        } else {
            output.extend_from_slice(&bytes[e.data..e.data + e.compressed]);
        }
        cursor = e.data + e.compressed + e.descriptor;
        if output.len() as u64 > limits.file_bytes {
            return Err(bad("saved archive exceeds file limit"));
        }
    }
    output.extend_from_slice(&bytes[cursor..directory]);
    // Append local records for new relationship entries without changing any
    // originals. Let the ZIP library emit the UTF-8 name and compression headers.
    let mut new_central = Vec::new();
    if !new.is_empty() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, payload) in new.drain(..) {
            writer
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .map_err(bad)?;
            writer.write_all(payload)?;
        }
        let staged = writer.finish().map_err(bad)?.into_inner();
        let staged_end = staged
            .len()
            .checked_sub(22)
            .ok_or_else(|| bad("new ZIP footer"))?;
        let staged_dir = n32(&staged, staged_end + 16)? as usize;
        let base = narrow(output.len())?;
        output.extend_from_slice(&staged[..staged_dir]);
        new_central.extend_from_slice(&staged[staged_dir..staged_end]);
        let mut i = 0;
        while i < new_central.len() {
            let prior = n32(&new_central, i + 42)?;
            put32(
                &mut new_central,
                i + 42,
                base.checked_add(prior)
                    .ok_or_else(|| bad("ZIP32 offset overflow"))?,
            );
            i += 46
                + usize::from(n16(&new_central, i + 28)?)
                + usize::from(n16(&new_central, i + 30)?)
                + usize::from(n16(&new_central, i + 32)?);
        }
    }
    let new_directory = narrow(output.len())?;
    for (i, e) in entries.iter().enumerate() {
        let next = entries.get(i + 1).map_or(end, |other| other.central);
        let header = output.len();
        output.extend_from_slice(&bytes[e.central..next]);
        put32(&mut output, header + 42, offsets[i]);
        if let Some((data, crc, size)) = &e.changed {
            put32(&mut output, header + 16, *crc);
            put32(&mut output, header + 20, narrow(data.len())?);
            put32(&mut output, header + 24, *size);
        }
    }
    output.extend_from_slice(&new_central);
    let header = output.len();
    let directory_size = narrow(header - new_directory as usize)?;
    output.extend_from_slice(&bytes[end..]);
    let encoded = (count as u16).to_le_bytes();
    output[header + 8..header + 10].copy_from_slice(&encoded);
    output[header + 10..header + 12].copy_from_slice(&encoded);
    put32(&mut output, header + 12, directory_size);
    put32(&mut output, header + 16, new_directory);
    if output.len() as u64 > limits.file_bytes {
        return Err(bad("saved archive exceeds file limit"));
    }
    Ok(output)
}
