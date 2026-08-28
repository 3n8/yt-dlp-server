use serde::Serialize;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::UNIX_EPOCH;

const MAX_TREE_DEPTH: usize = 32;

#[derive(Debug, Serialize, Clone)]
pub struct FileEntry {
    pub name: String,
    pub modified: Option<f64>,
    pub created: Option<f64>,
    pub size: Option<u64>,
    pub directory: bool,
    pub children: Option<Vec<FileEntry>>,
}

pub fn build_tree(root: &Path) -> Vec<FileEntry> {
    let mut seen = Vec::new();
    walk(root, &mut seen, 0)
}

fn walk(root: &Path, seen: &mut Vec<(u64, u64)>, depth: usize) -> Vec<FileEntry> {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Error scanning {}: {e}", root.display());
            return vec![];
        }
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Error accessing {}: {e}", entry.path().display());
                continue;
            }
        };
        let is_dir = meta.is_dir();
        let children = if is_dir {
            let key = (meta.dev(), meta.ino());
            if depth < MAX_TREE_DEPTH && !seen.contains(&key) {
                seen.push(key);
                Some(walk(&entry.path(), seen, depth + 1))
            } else {
                Some(vec![])
            }
        } else {
            None
        };
        files.push(FileEntry {
            name: name.into_owned(),
            modified: unix_ts(&meta.modified().ok()),
            created: unix_ts(&meta.created().ok().or_else(|| meta.modified().ok())),
            size: if is_dir { None } else { Some(meta.len()) },
            directory: is_dir,
            children,
        });
    }
    files
}

fn unix_ts(t: &Option<std::time::SystemTime>) -> Option<f64> {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}
