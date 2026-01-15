use crate::{session::Session, Result, TmsError};
use error_stack::{report, ResultExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct RepoCache {
    version: u32,
    /// Map of session name -> list of session data
    sessions: HashMap<String, Vec<CachedSession>>,
    /// Last scan timestamps for each search directory
    dir_timestamps: HashMap<PathBuf, SystemTime>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CachedSession {
    name: String,
    path: PathBuf,
    is_worktree: bool,
    is_bare: bool,
}

impl Default for RepoCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RepoCache {
    pub fn new() -> Self {
        Self {
            version: CACHE_VERSION,
            sessions: HashMap::new(),
            dir_timestamps: HashMap::new(),
        }
    }

    /// Load cache from disk
    pub fn load() -> Result<Option<Self>> {
        let cache_path = Self::cache_path()?;

        if !cache_path.exists() {
            return Ok(None);
        }

        let contents = fs::read_to_string(&cache_path).change_context(TmsError::IoError)?;

        let cache: RepoCache = toml::from_str(&contents).change_context(TmsError::ConfigError)?;

        // Check version compatibility
        if cache.version != CACHE_VERSION {
            eprintln!("Cache version mismatch, rebuilding cache");
            return Ok(None);
        }

        Ok(Some(cache))
    }

    /// Save cache to disk
    pub fn save(&self) -> Result<()> {
        let cache_path = Self::cache_path()?;

        // Ensure cache directory exists
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent).change_context(TmsError::IoError)?;
        }

        let contents = toml::to_string_pretty(&self).change_context(TmsError::ConfigError)?;

        fs::write(&cache_path, contents).change_context(TmsError::IoError)?;

        Ok(())
    }

    /// Get the cache file path
    fn cache_path() -> Result<PathBuf> {
        let cache_dir = dirs::cache_dir().ok_or_else(|| {
            report!(TmsError::ConfigError).attach_printable("Could not determine cache directory")
        })?;

        Ok(cache_dir.join("tms").join("repos.cache"))
    }

    /// Check if a directory needs to be rescanned based on modification time
    pub fn needs_rescan(&self, dir: &Path) -> Result<bool> {
        let current_mtime = Self::get_dir_mtime(dir)?;

        match self.dir_timestamps.get(dir) {
            Some(&cached_mtime) => Ok(current_mtime > cached_mtime),
            None => Ok(true), // Never scanned before
        }
    }

    /// Update the modification time for a directory
    pub fn update_dir_timestamp(&mut self, dir: PathBuf) -> Result<()> {
        let mtime = Self::get_dir_mtime(&dir)?;
        self.dir_timestamps.insert(dir, mtime);
        Ok(())
    }

    /// Get directory modification time
    fn get_dir_mtime(dir: &Path) -> Result<SystemTime> {
        let metadata = fs::metadata(dir)
            .change_context(TmsError::IoError)
            .attach_printable_lazy(|| format!("Failed to get metadata for {:?}", dir))?;

        metadata.modified().change_context(TmsError::IoError)
    }

    /// Add sessions from a directory scan
    pub fn add_sessions(&mut self, sessions: HashMap<String, Vec<Session>>) {
        use crate::session::SessionType;

        for (name, session_list) in sessions {
            let cached_list: Vec<CachedSession> = session_list
                .into_iter()
                .map(|session| {
                    let (path, is_worktree, is_bare) = match &session.session_type {
                        SessionType::Git(repo) => (
                            session.path().to_path_buf(),
                            repo.is_worktree(),
                            repo.is_bare(),
                        ),
                        SessionType::Bookmark(path) => (path.clone(), false, false),
                    };

                    CachedSession {
                        name: session.name.clone(),
                        path,
                        is_worktree,
                        is_bare,
                    }
                })
                .collect();

            self.sessions.insert(name, cached_list);
        }
    }

    /// Remove sessions from a specific directory (for rescanning)
    pub fn remove_dir_sessions(&mut self, dir: &Path) {
        self.sessions
            .retain(|_, session_list| session_list.iter().any(|s| !s.path.starts_with(dir)));
    }

    /// Convert cache back to session HashMap
    pub fn to_sessions(&self) -> HashMap<String, Vec<Session>> {
        use crate::configs::Config;
        use crate::repos::RepoProvider;
        use crate::session::{Session, SessionType};

        let mut result = HashMap::new();
        let config = Config::new().ok();

        for (name, cached_list) in &self.sessions {
            let session_list: Vec<Session> = cached_list
                .iter()
                .filter_map(|cached| {
                    // Try to reopen the repository
                    if let Some(ref cfg) = config {
                        if let Ok(repo) = RepoProvider::open(&cached.path, cfg) {
                            return Some(Session::new(cached.name.clone(), SessionType::Git(repo)));
                        }
                    }

                    // Fallback to bookmark if path still exists
                    if cached.path.exists() {
                        Some(Session::new(
                            cached.name.clone(),
                            SessionType::Bookmark(cached.path.clone()),
                        ))
                    } else {
                        None
                    }
                })
                .collect();

            if !session_list.is_empty() {
                result.insert(name.clone(), session_list);
            }
        }

        result
    }

    /// Get cache statistics for debugging
    pub fn stats(&self) -> CacheStats {
        let total_sessions = self.sessions.values().map(|v| v.len()).sum();
        let total_dirs = self.dir_timestamps.len();

        CacheStats {
            total_repos: self.sessions.len(),
            total_sessions,
            tracked_dirs: total_dirs,
        }
    }
}

#[derive(Debug)]
pub struct CacheStats {
    pub total_repos: usize,
    pub total_sessions: usize,
    pub tracked_dirs: usize,
}
