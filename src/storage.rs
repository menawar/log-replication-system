use crate::log_entry::LogEntry;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::{info, warn, debug};

/// Persistent storage for log entries
pub struct Storage {
    /// Path to the log file
    log_file_path: PathBuf,
    /// Path to the metadata file
    metadata_file_path: PathBuf,
    /// In-memory cache of log entries
    entries: HashMap<u64, LogEntry>,
    /// Current term (for leader election)
    current_term: u64,
    /// Last log index
    last_log_index: u64,
    /// Index of highest committed entry
    commit_index: u64,
}

/// Metadata stored persistently
#[derive(Debug, Serialize, Deserialize)]
struct Metadata {
    current_term: u64,
    last_log_index: u64,
    commit_index: u64,
}

impl Storage {
    /// Create a new storage instance
    pub fn new<P: AsRef<Path>>(data_dir: P) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir)
            .context("Failed to create data directory")?;

        let log_file_path = data_dir.join("log.jsonl");
        let metadata_file_path = data_dir.join("metadata.json");

        let mut storage = Self {
            log_file_path,
            metadata_file_path,
            entries: HashMap::new(),
            current_term: 0,
            last_log_index: 0,
            commit_index: 0,
        };

        // Load existing data
        storage.load()?;
        
        info!("Storage initialized with {} entries, term={}, commit_index={}", 
              storage.entries.len(), storage.current_term, storage.commit_index);

        Ok(storage)
    }

    /// Append a new log entry
    pub fn append(&mut self, entry: LogEntry) -> Result<()> {
        debug!("Appending entry {} to storage", entry.id);
        
        // Update in-memory state
        self.entries.insert(entry.id, entry.clone());
        self.last_log_index = entry.id.max(self.last_log_index);

        // Persist to disk
        self.write_entry_to_disk(&entry)?;
        self.save_metadata()?;

        Ok(())
    }

    /// Get a log entry by ID
    pub fn get(&self, id: u64) -> Option<&LogEntry> {
        self.entries.get(&id)
    }

    /// Get all entries from start_id onwards
    pub fn get_entries_from(&self, start_id: u64) -> Vec<LogEntry> {
        let mut entries: Vec<_> = self.entries
            .values()
            .filter(|entry| entry.id >= start_id)
            .cloned()
            .collect();
        entries.sort_by_key(|entry| entry.id);
        entries
    }

    /// Get all entries
    pub fn get_all_entries(&self) -> Vec<LogEntry> {
        let mut entries: Vec<_> = self.entries.values().cloned().collect();
        entries.sort_by_key(|entry| entry.id);
        entries
    }

    /// Commit entries up to the given index
    pub fn commit_up_to(&mut self, index: u64) -> Result<Vec<LogEntry>> {
        let mut committed_entries = Vec::new();
        
        for entry_id in (self.commit_index + 1)..=index {
            if let Some(entry) = self.entries.get_mut(&entry_id) {
                if !entry.is_committed() {
                    entry.commit();
                    committed_entries.push(entry.clone());
                }
            }
        }
        
        self.commit_index = index;
        self.save_metadata()?;
        
        info!("Committed {} entries up to index {}", committed_entries.len(), index);
        Ok(committed_entries)
    }

    /// Get the current term
    pub fn get_current_term(&self) -> u64 {
        self.current_term
    }

    /// Set the current term
    pub fn set_current_term(&mut self, term: u64) -> Result<()> {
        self.current_term = term;
        self.save_metadata()?;
        Ok(())
    }

    /// Get the last log index
    pub fn get_last_log_index(&self) -> u64 {
        self.last_log_index
    }

    /// Get the commit index
    pub fn get_commit_index(&self) -> u64 {
        self.commit_index
    }

    /// Get the last log term
    pub fn get_last_log_term(&self) -> u64 {
        if let Some(entry) = self.entries.get(&self.last_log_index) {
            entry.term
        } else {
            0
        }
    }

    /// Truncate log from the given index onwards (used for conflict resolution)
    pub fn truncate_from(&mut self, index: u64) -> Result<()> {
        let mut to_remove = Vec::new();
        for &entry_id in self.entries.keys() {
            if entry_id >= index {
                to_remove.push(entry_id);
            }
        }

        for entry_id in to_remove {
            self.entries.remove(&entry_id);
        }

        // Update last_log_index
        self.last_log_index = self.entries.keys().max().copied().unwrap_or(0);
        
        // Rewrite the entire log file (simple approach)
        self.rewrite_log_file()?;
        self.save_metadata()?;

        warn!("Truncated log from index {}", index);
        Ok(())
    }

    /// Load data from disk
    fn load(&mut self) -> Result<()> {
        // Load metadata
        if self.metadata_file_path.exists() {
            let metadata_content = std::fs::read_to_string(&self.metadata_file_path)
                .context("Failed to read metadata file")?;
            let metadata: Metadata = serde_json::from_str(&metadata_content)
                .context("Failed to parse metadata")?;
            
            self.current_term = metadata.current_term;
            self.last_log_index = metadata.last_log_index;
            self.commit_index = metadata.commit_index;
        }

        // Load log entries
        if self.log_file_path.exists() {
            let file = File::open(&self.log_file_path)
                .context("Failed to open log file")?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.context("Failed to read line from log file")?;
                if line.trim().is_empty() {
                    continue;
                }
                
                let entry: LogEntry = serde_json::from_str(&line)
                    .context("Failed to parse log entry")?;
                self.entries.insert(entry.id, entry);
            }
        }

        Ok(())
    }

    /// Write a single entry to disk
    fn write_entry_to_disk(&self, entry: &LogEntry) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file_path)
            .context("Failed to open log file for writing")?;

        let entry_json = serde_json::to_string(entry)
            .context("Failed to serialize log entry")?;
        
        writeln!(file, "{}", entry_json)
            .context("Failed to write log entry to file")?;
        
        file.sync_all()
            .context("Failed to sync log file")?;

        Ok(())
    }

    /// Rewrite the entire log file (used after truncation)
    fn rewrite_log_file(&self) -> Result<()> {
        let temp_path = self.log_file_path.with_extension("tmp");
        
        {
            let file = File::create(&temp_path)
                .context("Failed to create temporary log file")?;
            let mut writer = BufWriter::new(file);

            let mut entries: Vec<_> = self.entries.values().collect();
            entries.sort_by_key(|entry| entry.id);

            for entry in entries {
                let entry_json = serde_json::to_string(entry)
                    .context("Failed to serialize log entry")?;
                writeln!(writer, "{}", entry_json)
                    .context("Failed to write log entry")?;
            }
            
            writer.flush()
                .context("Failed to flush writer")?;
        }

        std::fs::rename(&temp_path, &self.log_file_path)
            .context("Failed to replace log file")?;

        Ok(())
    }

    /// Save metadata to disk
    fn save_metadata(&self) -> Result<()> {
        let metadata = Metadata {
            current_term: self.current_term,
            last_log_index: self.last_log_index,
            commit_index: self.commit_index,
        };

        let metadata_json = serde_json::to_string_pretty(&metadata)
            .context("Failed to serialize metadata")?;

        std::fs::write(&self.metadata_file_path, metadata_json)
            .context("Failed to write metadata file")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_entry::Command;
    use tempfile::tempdir;

    #[test]
    fn test_storage_operations() -> Result<()> {
        let temp_dir = tempdir()?;
        let mut storage = Storage::new(temp_dir.path())?;

        // Test append
        let entry = LogEntry::new(1, 1, Command::operation("test".to_string()));
        storage.append(entry.clone())?;

        // Test get
        assert_eq!(storage.get(1), Some(&entry));
        assert_eq!(storage.get_last_log_index(), 1);

        // Test persistence by creating new storage instance
        let storage2 = Storage::new(temp_dir.path())?;
        assert_eq!(storage2.get(1), Some(&entry));
        assert_eq!(storage2.get_last_log_index(), 1);

        Ok(())
    }

    #[test]
    fn test_commit_operations() -> Result<()> {
        let temp_dir = tempdir()?;
        let mut storage = Storage::new(temp_dir.path())?;

        let entry1 = LogEntry::new(1, 1, Command::operation("test1".to_string()));
        let entry2 = LogEntry::new(2, 1, Command::operation("test2".to_string()));
        
        storage.append(entry1)?;
        storage.append(entry2)?;

        assert_eq!(storage.get_commit_index(), 0);
        
        let committed = storage.commit_up_to(2)?;
        assert_eq!(committed.len(), 2);
        assert_eq!(storage.get_commit_index(), 2);

        Ok(())
    }
} 