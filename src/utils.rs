use crate::constants::{ChunkFlags, FLAG_MAPPINGS};
use crate::error::DzipError;
use anyhow::Result;
use std::borrow::Cow;
use std::io::BufRead;
use std::path::{Component, Path, PathBuf};

pub fn decode_flags(bits: u16) -> Vec<Cow<'static, str>> {
    let flags = ChunkFlags::from_bits_truncate(bits);
    let mut list = Vec::new();

    // Special case: No flags usually implies plain COPY in this format
    if flags.is_empty() {
        list.push(Cow::Borrowed("COPY"));
        return list;
    }

    for (flag, name) in FLAG_MAPPINGS {
        if flags.contains(*flag) {
            list.push(Cow::Borrowed(*name));
        }
    }

    list
}

pub fn encode_flags<S: AsRef<str>>(flags_vec: &[S]) -> u16 {
    let mut res = ChunkFlags::empty();

    if flags_vec.is_empty() {
        return res.bits();
    }

    for f in flags_vec {
        let s = f.as_ref();
        // O(N) lookup is fine here as N (number of flag types) is very small (~10)
        if let Some((flag, _)) = FLAG_MAPPINGS.iter().find(|(_, name)| *name == s) {
            res.insert(*flag);
        }
    }

    // Fallback/Legacy handling:
    // If the result is empty but the user explicitly requested "COPY"
    // (and for some reason it wasn't caught by the loop, though it should be),
    // or to ensure safety for implicit copy behavior.
    if res.is_empty() && flags_vec.iter().any(|f| f.as_ref() == "COPY") {
        res.insert(ChunkFlags::COPYCOMP);
    }

    res.bits()
}

pub fn read_null_term_string<R: BufRead>(reader: &mut R) -> Result<String> {
    let mut bytes = Vec::new();
    reader.read_until(0, &mut bytes)?;
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

pub fn sanitize_path(base: &Path, rel_path_str: &str) -> Result<PathBuf> {
    let normalized = rel_path_str.replace('\\', "/");
    let rel_path = Path::new(&normalized);
    let mut safe_path = PathBuf::new();

    for component in rel_path.components() {
        match component {
            Component::Normal(os_str) => safe_path.push(os_str),
            Component::ParentDir => {
                // [Error] Use typed Security error
                return Err(DzipError::Security(format!(
                    "Directory traversal (..) detected in path: {}",
                    rel_path_str
                ))
                .into());
            }
            Component::RootDir => continue,
            Component::Prefix(_) => {
                // [Error] Use typed Security error
                return Err(DzipError::Security(format!(
                    "Absolute path or drive letter detected: {}",
                    rel_path_str
                ))
                .into());
            }
            Component::CurDir => continue,
        }
    }

    if safe_path.as_os_str().is_empty() {
        return Err(DzipError::Security(format!(
            "Invalid empty path resolution: {}",
            rel_path_str
        ))
        .into());
    }

    Ok(base.join(safe_path))
}
