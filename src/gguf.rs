//! Rewrites tensor shapes in a GGUF file's header, in place.
//!
//! Converters squeeze 5-D convolution weights into GGUF's 4 dimensions in different ways:
//! one keeps `[2, 2, 1, 96, 5120]`, another stores `[4, 1, 96, 5120]`. For an unquantized
//! tensor both are the same bytes in the same order, so relabelling the shape as the one
//! stable-diffusion.cpp expects is enough to load it, and needs no rewrite of the weights.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// How much of the file to read for the header; tensor info sits near the start.
const HEADER_LIMIT: u64 = 64 * 1024 * 1024;
/// GGUF types whose values are stored one by one (F32, F16, BF16), so any regrouping of
/// dimensions keeps the byte order. Quantized types pack blocks along the first dimension.
const PLAIN_TYPES: [u32; 3] = [0, 1, 30];

/// A tensor's entry in the header: where its dimensions are stored, and what they are.
struct TensorInfo {
    name: String,
    dims_at: usize,
    dims: Vec<u64>,
    kind: u32,
}

/// Relabels each named tensor with its wanted dimensions. Tensors already shaped so are left
/// alone, so this can run on every pull. Returns how many it changed.
pub fn reshape_in_place(path: &Path, wanted: &[(String, Vec<u64>)]) -> Result<usize, String> {
    let mut file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let mut header = vec![0u8; length.min(HEADER_LIMIT) as usize];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    let tensors = parse(&header)?;
    let mut changed = 0;
    for (name, dims) in wanted {
        let tensor = tensors.iter().find(|t| &t.name == name).ok_or_else(|| format!("{}: no tensor `{name}`", path.display()))?;
        if &tensor.dims == dims {
            continue;
        }
        if tensor.dims.len() != dims.len() {
            return Err(format!("`{name}` has {} dimensions; can only relabel to the same number", tensor.dims.len()));
        }
        if tensor.dims.iter().product::<u64>() != dims.iter().product::<u64>() {
            return Err(format!("`{name}` is {:?}, which does not hold the same values as {dims:?}", tensor.dims));
        }
        if !PLAIN_TYPES.contains(&tensor.kind) {
            return Err(format!("`{name}` is quantized (type {}), so its layout depends on its shape", tensor.kind));
        }
        let bytes: Vec<u8> = dims.iter().flat_map(|d| d.to_le_bytes()).collect();
        file.seek(SeekFrom::Start(tensor.dims_at as u64)).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        changed += 1;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(changed)
}

fn parse(data: &[u8]) -> Result<Vec<TensorInfo>, String> {
    let mut reader = Reader { data, pos: 0 };
    if reader.bytes(4)? != b"GGUF" {
        return Err("not a GGUF file".into());
    }
    let version = reader.u32()?;
    if version < 2 {
        return Err(format!("GGUF version {version} is not supported"));
    }
    let tensor_count = reader.u64()?;
    let kv_count = reader.u64()?;
    for _ in 0..kv_count {
        reader.string()?;
        let kind = reader.u32()?;
        reader.skip_value(kind)?;
    }
    let mut tensors = vec![];
    for _ in 0..tensor_count {
        let name = reader.string()?;
        let n_dims = reader.u32()? as usize;
        let dims_at = reader.pos;
        let dims = (0..n_dims).map(|_| reader.u64()).collect::<Result<Vec<_>, _>>()?;
        let kind = reader.u32()?;
        reader.u64()?; // offset
        tensors.push(TensorInfo { name, dims_at, dims, kind });
    }
    Ok(tensors)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, count: usize) -> Result<&[u8], String> {
        let end = self.pos.checked_add(count).filter(|&e| e <= self.data.len()).ok_or("GGUF header is longer than expected")?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().expect("4 bytes")))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().expect("8 bytes")))
    }

    fn string(&mut self) -> Result<String, String> {
        let length = self.u64()? as usize;
        Ok(String::from_utf8_lossy(self.bytes(length)?).into_owned())
    }

    fn skip_value(&mut self, kind: u32) -> Result<(), String> {
        let size = match kind {
            0 | 1 | 7 => 1,
            2 | 3 => 2,
            4..=6 => 4,
            10..=12 => 8,
            8 => {
                self.string()?;
                return Ok(());
            }
            9 => {
                let inner = self.u32()?;
                for _ in 0..self.u64()? {
                    self.skip_value(inner)?;
                }
                return Ok(());
            }
            other => return Err(format!("unknown GGUF value type {other}")),
        };
        self.bytes(size)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal GGUF file with one metadata string and the given tensors.
    fn sample(tensors: &[(&str, Vec<u64>, u32)]) -> Vec<u8> {
        let mut out = b"GGUF".to_vec();
        out.extend(3u32.to_le_bytes());
        out.extend((tensors.len() as u64).to_le_bytes());
        out.extend(1u64.to_le_bytes());
        let text = |out: &mut Vec<u8>, s: &str| {
            out.extend((s.len() as u64).to_le_bytes());
            out.extend(s.as_bytes());
        };
        text(&mut out, "general.name");
        out.extend(8u32.to_le_bytes());
        text(&mut out, "test");
        for (name, dims, kind) in tensors {
            text(&mut out, name);
            out.extend((dims.len() as u32).to_le_bytes());
            for d in dims {
                out.extend(d.to_le_bytes());
            }
            out.extend(kind.to_le_bytes());
            out.extend(0u64.to_le_bytes());
        }
        out.extend([0u8; 64]);
        out
    }

    #[test]
    fn relabels_plain_tensors_and_leaves_the_rest() {
        let path = std::env::temp_dir().join(format!("forgeflow-gguf-{}.gguf", uuid::Uuid::now_v7()));
        std::fs::write(&path, sample(&[("vace.weight", vec![4, 1, 96, 5120], 1), ("other", vec![8, 8], 1), ("q", vec![32, 4], 12)])).unwrap();
        let wanted = vec![("vace.weight".to_string(), vec![2, 2, 1, 491520])];
        assert_eq!(reshape_in_place(&path, &wanted).unwrap(), 1);
        let tensors = parse(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(tensors[0].dims, vec![2, 2, 1, 491520]);
        assert_eq!(tensors[1].dims, vec![8, 8]);
        // Running again changes nothing.
        assert_eq!(reshape_in_place(&path, &wanted).unwrap(), 0);
        // Different element counts and quantized tensors are refused.
        assert!(reshape_in_place(&path, &[("other".to_string(), vec![4, 8])]).is_err());
        assert!(reshape_in_place(&path, &[("q".to_string(), vec![64, 2])]).is_err());
        std::fs::remove_file(&path).ok();
    }
}
