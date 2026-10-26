//! Recorded replies: one small JSON file per question, named by a hash of the question and
//! holding the question itself, so a hash collision can never hand back another answer.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use persephone_core::UnixNanos;
use serde::{Deserialize, Serialize};

use super::Request;

const RELEASES: &str = "releases.json";

#[derive(Serialize, Deserialize)]
struct Entry {
    request: Request,
    reply: String,
    /// When the reply was recorded, in seconds since 1970.
    recorded: u64,
}

/// The folder of recorded replies.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The recorded reply to exactly this request.
    #[must_use]
    pub fn get(&self, request: &Request) -> Option<String> {
        let text = fs::read_to_string(self.path(request)).ok()?;
        let entry: Entry = serde_json::from_str(&text).ok()?;
        (entry.request == *request).then_some(entry.reply)
    }

    /// Records `reply` as the answer to `request`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file can't be written.
    pub fn put(&self, request: &Request, reply: &str) -> anyhow::Result<()> {
        let path = self.path(request);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let entry = Entry {
            request: request.clone(),
            reply: reply.to_owned(),
            recorded: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&entry)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// How many replies are recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        let Ok(shards) = fs::read_dir(&self.dir) else {
            return 0;
        };
        shards
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| fs::read_dir(e.path()).ok())
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Forgets every recorded reply.
    ///
    /// # Errors
    ///
    /// Returns an error if the folder can't be deleted.
    pub fn clear(&self) -> anyhow::Result<()> {
        if self.dir.exists() {
            fs::remove_dir_all(&self.dir)
                .with_context(|| format!("deleting {}", self.dir.display()))?;
        }
        Ok(())
    }

    pub(super) fn releases(&self) -> HashMap<String, Option<UnixNanos>> {
        fs::read_to_string(self.dir.join(RELEASES))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub(super) fn save_releases(
        &self,
        releases: &HashMap<String, Option<UnixNanos>>,
    ) -> anyhow::Result<()> {
        fs::create_dir_all(&self.dir)?;
        fs::write(
            self.dir.join(RELEASES),
            serde_json::to_string_pretty(releases)?,
        )?;
        Ok(())
    }

    pub(super) fn path(&self, request: &Request) -> PathBuf {
        let key = serde_json::to_string(request).unwrap_or_default();
        let hash = hash(key.as_bytes());
        self.dir.join(&hash[..2]).join(format!("{hash}.json"))
    }
}

/// 128 bits of FNV-1a (two passes with different starting points): stable across builds and
/// platforms, unlike the standard library's hasher.
fn hash(bytes: &[u8]) -> String {
    const PRIME: u64 = 0x0100_0000_01b3;
    let pass = |start: u64, bytes: &mut dyn Iterator<Item = &u8>| {
        bytes.fold(start, |h, &b| (h ^ u64::from(b)).wrapping_mul(PRIME))
    };
    let forward = pass(0xcbf2_9ce4_8422_2325, &mut bytes.iter());
    let backward = pass(0x8422_2325_cbf2_9ce4, &mut bytes.iter().rev());
    format!("{forward:016x}{backward:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable() {
        assert_eq!(hash(b""), "cbf29ce48422232584222325cbf29ce4");
        assert_ne!(hash(b"ab"), hash(b"ba"));
    }
}
