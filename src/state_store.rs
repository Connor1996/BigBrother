use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::PersistentPrState;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistentStateFile {
    pub prs: BTreeMap<String, PersistentPrState>,
}

#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn load(&self) -> Result<PersistentStateFile> {
        if !self.path.exists() {
            return Ok(PersistentStateFile::default());
        }

        let contents = fs::read_to_string(&self.path)
            .with_context(|| format!("failed reading {}", self.path.display()))?;

        serde_json::from_str(&contents)
            .with_context(|| format!("failed parsing {}", self.path.display()))
    }

    pub fn save(&self, state: &PersistentStateFile) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating {}", parent.display()))?;
        }

        let contents = serde_json::to_string_pretty(state)?;
        fs::write(&self.path, contents)
            .with_context(|| format!("failed writing {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn load_defaults_when_file_is_missing() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symphony-rs-state-{unique}.json"));
        let store = StateStore::new(&path);

        let loaded = store.load().expect("state should load");
        assert!(loaded.prs.is_empty());
    }

    #[test]
    fn load_backfills_new_fields_for_older_state_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symphony-rs-state-{unique}.json"));
        fs::write(
            &path,
            r#"{
  "prs": {
    "openai/symphony#1": {
      "last_run_status": "success"
    }
  }
}"#,
        )
        .expect("fixture state file should write");

        let store = StateStore::new(&path);
        let loaded = store.load().expect("state should load");
        let pr = loaded
            .prs
            .get("openai/symphony#1")
            .expect("fixture PR should load");

        assert!(!pr.paused, "missing paused field should default to false");
        assert_eq!(
            pr.consecutive_failures, 0,
            "missing retry counter should default to zero",
        );
    }
}
