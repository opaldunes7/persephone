//! Opening CSV files: plain, gzipped, or zipped (every CSV inside, in name order).

use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Context;

/// How much of a file detection looks at.
const SAMPLE_BYTES: usize = 256 * 1024;

enum Packing {
    Plain,
    Gzip,
    Zip,
}

fn packing(path: &Path) -> anyhow::Result<Packing> {
    let mut magic = [0u8; 4];
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let n = file.read(&mut magic)?;
    Ok(match &magic[..n] {
        [0x1f, 0x8b, ..] => Packing::Gzip,
        [b'P', b'K', 3, 4] => Packing::Zip,
        _ => Packing::Plain,
    })
}

/// Counts the bytes read from the file on disk, for progress.
pub(crate) struct Counted<R> {
    inner: R,
    read: Arc<AtomicU64>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl<R: Seek> Seek for Counted<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Calls `each` with a reader for every CSV in `path` and its name. `read` counts the bytes
/// taken from disk (compressed bytes for packed files).
pub(crate) fn for_each_csv(
    path: &Path,
    read: &Arc<AtomicU64>,
    mut each: impl FnMut(&mut dyn Read, &str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let name = path.display().to_string();
    let file = File::open(path).with_context(|| format!("opening {name}"))?;
    let counted = Counted {
        inner: BufReader::with_capacity(1 << 20, file),
        read: Arc::clone(read),
    };
    match packing(path)? {
        Packing::Plain => each(&mut { counted }, &name),
        Packing::Gzip => each(&mut flate2::read::MultiGzDecoder::new(counted), &name),
        Packing::Zip => {
            let mut archive =
                zip::ZipArchive::new(counted).with_context(|| format!("reading {name}"))?;
            for index in csv_entries(&mut archive) {
                let mut entry = archive.by_index(index)?;
                let entry_name = format!("{name}:{}", entry.name()?);
                each(&mut entry, &entry_name)?;
            }
            Ok(())
        }
    }
}

/// The start of the first CSV in `path`, cut at a line end, for detecting its format.
///
/// # Errors
///
/// Returns an error if the file can't be read or holds no CSV.
pub fn sample(path: &Path) -> anyhow::Result<String> {
    let mut bytes = Vec::with_capacity(SAMPLE_BYTES);
    let mut first = true;
    for_each_csv(path, &Arc::new(AtomicU64::new(0)), |reader, _| {
        if first {
            first = false;
            reader.take(SAMPLE_BYTES as u64).read_to_end(&mut bytes)?;
        }
        Ok(())
    })?;
    anyhow::ensure!(!bytes.is_empty(), "{} is empty", path.display());
    if bytes.len() == SAMPLE_BYTES
        && let Some(end) = bytes.iter().rposition(|&b| b == b'\n')
    {
        bytes.truncate(end + 1);
    }
    // Old files are sometimes Latin-1; the columns that matter are ASCII either way.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn csv_entries<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Vec<usize> {
    let mut entries: Vec<(String, usize)> = (0..archive.len())
        .filter_map(|i| {
            let entry = archive.by_index_raw(i).ok()?;
            let name = entry.name().ok()?.into_owned();
            let lower = name.to_lowercase();
            (entry.is_file()
                && !lower.starts_with("__macosx/")
                && [".csv", ".txt", ".tsv"]
                    .iter()
                    .any(|ext| lower.ends_with(ext)))
            .then_some((name, i))
        })
        .collect();
    entries.sort();
    entries.into_iter().map(|(_, i)| i).collect()
}
