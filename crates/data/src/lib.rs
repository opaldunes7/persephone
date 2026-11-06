//! Market data for Persephone: fetching from exchanges, the local tick store, and the CSV
//! importer that fills it.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub mod archive;
pub mod binance;
pub mod import;
pub mod news;
pub mod store;

/// Where to write a file before moving it into place at `path`: beside it, named for this process
/// and this write, so no two writers share one, even two Labs working on one folder.
#[must_use]
pub fn aside(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}-{}.partial",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}
