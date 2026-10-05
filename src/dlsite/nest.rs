//! What DLsiteNest knows about the games it installed: its LiteDB database holds, per
//! work, the folder it was put in (`DownloadPath`) and the work ID (`WorkId`, such as
//! `RJ01464588`). windows-link only reads it, so the mapping can be kept after
//! DLsiteNest is gone.

use std::{collections::HashMap, path::PathBuf};

/// `%APPDATA%\DLsiteNest`'s database and its write-ahead log (newer changes).
pub fn database_files() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let dir = PathBuf::from(appdata).join("DLsiteNest");
    vec![dir.join("dlsite.db"), dir.join("dlsite-log.db")]
}

/// Folder (lower case) -> work ID, from the BSON documents in a LiteDB file. Records
/// split across pages are skipped; later records win.
pub fn records(data: &[u8]) -> Vec<(String, String)> {
    const KEY: &[u8] = b"\x02WorkId\x00";
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = find(&data[at..], KEY) {
        let start = at + offset;
        if let Some(fields) = document(&data[start..])
            && let (Some(id), Some(folder)) = (fields.get("WorkId"), fields.get("DownloadPath"))
        {
            found.push((folder.to_lowercase(), id.clone()));
        }
        at = start + KEY.len();
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The string fields of the BSON elements starting at `data`, up to the document's
/// end; `None` when an element is cut or of a kind these records never have.
fn document(data: &[u8]) -> Option<HashMap<String, String>> {
    let mut fields = HashMap::new();
    let mut at = 0;
    loop {
        let kind = *data.get(at)?;
        if kind == 0 {
            return Some(fields);
        }
        let name_end = at + 1 + data.get(at + 1..)?.iter().take(64).position(|&b| b == 0)?;
        let name = String::from_utf8(data[at + 1..name_end].to_vec()).ok()?;
        at = name_end + 1;
        at += match kind {
            // String: length (with the closing NUL) and the text.
            0x02 => {
                let len =
                    usize::try_from(i32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
                        .ok()?;
                if !(1..=4096).contains(&len) || *data.get(at + 4 + len - 1)? != 0 {
                    return None;
                }
                let text = String::from_utf8(data[at + 4..at + 4 + len - 1].to_vec()).ok()?;
                fields.insert(name, text);
                4 + len
            }
            0x08 => 1,
            0x10 => 4,
            0x01 | 0x09 | 0x12 => 8,
            0x0a => 0,
            _ => return None,
        };
    }
}

/// Folder (lower case) -> work ID from DLsiteNest's files, if it is installed.
pub fn read() -> HashMap<String, String> {
    database_files()
        .iter()
        .filter_map(|file| std::fs::read(file).ok())
        .flat_map(|data| records(&data))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::records;

    /// A BSON string element.
    fn string(name: &str, value: &str) -> Vec<u8> {
        let mut out = vec![0x02];
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(&i32::try_from(value.len() + 1).unwrap().to_le_bytes());
        out.extend_from_slice(value.as_bytes());
        out.push(0);
        out
    }

    fn work(id: &str, path: &str) -> Vec<u8> {
        let mut out = vec![0x10];
        out.extend_from_slice(b"_id\0");
        out.extend_from_slice(&7i32.to_le_bytes());
        out.extend(string("WorkId", id));
        out.extend(string("WorkName", "作品"));
        out.extend([0x08]);
        out.extend_from_slice(b"Favorite\0");
        out.push(0);
        out.extend(string("DownloadPath", path));
        out.extend([0x09]);
        out.extend_from_slice(b"DownloadedAt\0");
        out.extend_from_slice(&[1; 8]);
        out.push(0);
        out
    }

    #[test]
    fn reads_work_ids_and_folders_from_documents() {
        let mut data = vec![0xff; 40];
        data.extend(work(
            "RJ01686872",
            r"D:\DLsiteNest\game\SPLUSH WAVE\Grand Order 戦記",
        ));
        data.extend([0u8; 30]);
        data.extend(work("RJ316251", r"D:\DLsiteNest\game\Maker\Title"));
        assert_eq!(
            records(&data),
            [
                (
                    r"d:\dlsitenest\game\splush wave\grand order 戦記".to_owned(),
                    "RJ01686872".to_owned()
                ),
                (
                    r"d:\dlsitenest\game\maker\title".to_owned(),
                    "RJ316251".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn skips_cut_or_broken_records() {
        let whole = work("RJ1", r"D:\a\b");
        // Cut in the middle of the folder.
        let cut = &whole[..whole.len() - 20];
        assert!(records(cut).is_empty());
        // A record without a folder, and garbage after the ID.
        let mut data = string("WorkId", "RJ2");
        data.extend([0x7f, 0x01, 0x02]);
        data.extend(string("WorkId", "RJ3"));
        data.push(0);
        assert!(records(&data).is_empty());
    }
}
