use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Represents a single entry in the replicated log
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogEntry {
    /// Unique identifier for this entry
    pub id: u64,
    /// Term when this entry was created (for leader election)
    pub term: u64,
    /// Timestamp when this entry was created
    pub timestamp: u64,
    /// The actual command/data being replicated
    pub command: Command,
    /// Whether this entry has been committed (applied to state machine)
    pub committed: bool,
}

/// Commands that can be replicated in the log
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Command {
    /// A user-defined operation with arbitrary data
    Operation { data: String },
    /// System command for cluster management
    SystemCommand { action: SystemAction },
    /// No-op entry (used for heartbeats)
    NoOp,
}

/// System actions for cluster management
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SystemAction {
    /// Add a new node to the cluster
    AddNode { node_id: String, address: String },
    /// Remove a node from the cluster
    RemoveNode { node_id: String },
    /// Leader election related
    LeaderElection { candidate_id: String },
}

impl LogEntry {
    /// Create a new log entry
    pub fn new(id: u64, term: u64, command: Command) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            id,
            term,
            timestamp,
            command,
            committed: false,
        }
    }

    /// Create a no-op entry (useful for heartbeats)
    #[allow(dead_code)]
    pub fn no_op(id: u64, term: u64) -> Self {
        Self::new(id, term, Command::NoOp)
    }

    /// Mark this entry as committed
    pub fn commit(&mut self) {
        self.committed = true;
    }

    /// Check if this entry is committed
    pub fn is_committed(&self) -> bool {
        self.committed
    }
}

impl Command {
    /// Create a new operation command
    pub fn operation(data: String) -> Self {
        Command::Operation { data }
    }

    /// Create a system command
    #[allow(dead_code)]
    pub fn system(action: SystemAction) -> Self {
        Command::SystemCommand { action }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_entry_creation() {
        let entry = LogEntry::new(1, 1, Command::operation("test".to_string()));
        assert_eq!(entry.id, 1);
        assert_eq!(entry.term, 1);
        assert!(!entry.is_committed());
    }

    #[test]
    fn test_commit_entry() {
        let mut entry = LogEntry::new(1, 1, Command::NoOp);
        assert!(!entry.is_committed());
        entry.commit();
        assert!(entry.is_committed());
    }
} 