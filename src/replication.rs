use crate::log_entry::{LogEntry, Command};
use crate::network::{Message, NetworkHandler};
use crate::storage::Storage;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tracing::{info, warn, debug};

/// Node state in the cluster
#[derive(Debug, Clone, PartialEq)]
pub enum NodeState {
    Follower,
    Candidate,
    Leader,
}

/// Replication manager handles the core replication logic
pub struct ReplicationManager {
    /// Node ID
    node_id: String,
    /// Current state
    state: NodeState,
    /// Current term
    current_term: u64,
    /// ID of the node we voted for in current term
    voted_for: Option<String>,
    /// Storage backend
    storage: Storage,
    /// Network handler
    network: NetworkHandler,
    /// Peer addresses
    peers: Vec<SocketAddr>,
    /// Leader ID (if known)
    leader_id: Option<String>,
    /// Last time we heard from leader (for follower timeout)
    last_heartbeat: Instant,
    /// Election timeout duration
    election_timeout: Duration,
    /// Heartbeat interval (for leaders)
    #[allow(dead_code)]
    heartbeat_interval: Duration,
    /// Next index to send to each peer (for leaders)
    next_index: HashMap<SocketAddr, u64>,
    /// Highest index known to be replicated on each peer (for leaders)
    match_index: HashMap<SocketAddr, u64>,
    /// Votes received in current election (for candidates)
    votes_received: HashSet<String>,
}

impl ReplicationManager {
    /// Create a new replication manager
    pub fn new(
        node_id: String,
        storage: Storage,
        network: NetworkHandler,
        peers: Vec<SocketAddr>,
    ) -> Self {
        Self {
            node_id,
            state: NodeState::Follower,
            current_term: storage.get_current_term(),
            voted_for: None,
            storage,
            network,
            peers,
            leader_id: None,
            last_heartbeat: Instant::now(),
            election_timeout: Duration::from_millis(150 + rand::random::<u64>() % 150), // 150-300ms
            heartbeat_interval: Duration::from_millis(50), // 50ms
            next_index: HashMap::new(),
            match_index: HashMap::new(),
            votes_received: HashSet::new(),
        }
    }

    /// Get current state
    pub fn get_state(&self) -> &NodeState {
        &self.state
    }

    /// Get current term
    #[allow(dead_code)]
    pub fn get_current_term(&self) -> u64 {
        self.current_term
    }

    /// Get leader ID
    #[allow(dead_code)]
    pub fn get_leader_id(&self) -> Option<&String> {
        self.leader_id.as_ref()
    }

    /// Become leader (called when starting as leader)
    pub async fn become_leader(&mut self) -> Result<()> {
        info!("Becoming leader for term {}", self.current_term);
        self.state = NodeState::Leader;
        self.leader_id = Some(self.node_id.clone());
        
        // Initialize leader state
        let last_log_index = self.storage.get_last_log_index();
        for peer in &self.peers {
            self.next_index.insert(*peer, last_log_index + 1);
            self.match_index.insert(*peer, 0);
        }

        // Send initial heartbeat
        self.send_heartbeats().await?;
        
        Ok(())
    }

    /// Append a new command to the log (leader only)
    pub async fn append_command(&mut self, command: String) -> Result<bool> {
        if self.state != NodeState::Leader {
            return Ok(false);
        }

        let entry_id = self.storage.get_last_log_index() + 1;
        let entry = LogEntry::new(entry_id, self.current_term, Command::operation(command));
        
        info!("Leader appending entry {} to log", entry_id);
        self.storage.append(entry)?;

        // Replicate to followers
        self.replicate_to_followers().await?;
        
        // Auto-commit in single-node clusters (no peers to replicate to)
        if self.peers.is_empty() {
            info!("Single-node cluster detected, auto-committing entry {}", entry_id);
            self.storage.commit_up_to(entry_id)?;
        } else {
            // In multi-node clusters, check for majority and commit
            self.update_commit_index()?;
        }
        
        Ok(true)
    }

    /// Handle incoming message
    pub async fn handle_message(&mut self, message: Message) -> Result<Message> {
        match message {
            Message::AppendEntries { 
                term, 
                leader_id, 
                prev_log_index, 
                prev_log_term, 
                entries, 
                leader_commit 
            } => {
                self.handle_append_entries(
                    term, leader_id, prev_log_index, prev_log_term, entries, leader_commit
                ).await
            },
            Message::RequestVote { 
                term, 
                candidate_id, 
                last_log_index, 
                last_log_term 
            } => {
                self.handle_request_vote(term, candidate_id, last_log_index, last_log_term).await
            },
            Message::ClientAppend { command } => {
                if self.state == NodeState::Leader {
                    let success = self.append_command(command).await?;
                    Ok(Message::ClientAppendResponse { 
                        success, 
                        leader_id: self.leader_id.clone() 
                    })
                } else {
                    Ok(Message::ClientAppendResponse { 
                        success: false, 
                        leader_id: self.leader_id.clone() 
                    })
                }
            },
            Message::ClientQuery => {
                Ok(Message::ClientQueryResponse { 
                    entries: self.storage.get_all_entries(),
                    leader_id: self.leader_id.clone().unwrap_or_else(|| "unknown".to_string()),
                })
            },
            Message::Heartbeat { term, leader_id } => {
                self.handle_heartbeat(term, leader_id).await
            },
            _ => {
                warn!("Received unexpected message type");
                anyhow::bail!("Unexpected message type");
            }
        }
    }

    /// Check if we should start an election
    pub fn should_start_election(&self) -> bool {
        self.state == NodeState::Follower && 
        self.last_heartbeat.elapsed() > self.election_timeout
    }

    /// Start leader election
    pub async fn start_election(&mut self) -> Result<()> {
        info!("Starting leader election for term {}", self.current_term + 1);
        
        self.current_term += 1;
        self.state = NodeState::Candidate;
        self.voted_for = Some(self.node_id.clone());
        self.votes_received.clear();
        self.votes_received.insert(self.node_id.clone());
        self.last_heartbeat = Instant::now();
        
        self.storage.set_current_term(self.current_term)?;

        // Request votes from peers
        self.request_votes().await?;
        
        Ok(())
    }

    /// Send heartbeats to all followers (leader only)
    pub async fn send_heartbeats(&mut self) -> Result<()> {
        if self.state != NodeState::Leader {
            return Ok(());
        }

        debug!("Sending heartbeats to {} peers", self.peers.len());
        
        for peer in &self.peers.clone() {
            let prev_log_index = self.next_index.get(peer).unwrap_or(&1).saturating_sub(1);
            let prev_log_term = if prev_log_index > 0 {
                self.storage.get(prev_log_index)
                    .map(|entry| entry.term)
                    .unwrap_or(0)
            } else {
                0
            };

            let message = Message::AppendEntries {
                term: self.current_term,
                leader_id: self.node_id.clone(),
                prev_log_index,
                prev_log_term,
                entries: vec![], // Heartbeat has no entries
                leader_commit: self.storage.get_commit_index(),
            };

            // Send heartbeat (fire and forget for now)
            let peer_addr = *peer;
            if let Err(e) = self.network.send_message(peer_addr, message).await {
                debug!("Failed to send heartbeat to {}: {}", peer_addr, e);
            }
        }
        
        Ok(())
    }

    /// Handle AppendEntries RPC
    async fn handle_append_entries(
        &mut self,
        term: u64,
        leader_id: String,
        prev_log_index: u64,
        prev_log_term: u64,
        entries: Vec<LogEntry>,
        leader_commit: u64,
    ) -> Result<Message> {
        debug!("Received AppendEntries from {} for term {}", leader_id, term);

        // Update term if necessary
        if term > self.current_term {
            self.current_term = term;
            self.voted_for = None;
            self.state = NodeState::Follower;
            self.storage.set_current_term(term)?;
        }

        // Reset election timeout
        self.last_heartbeat = Instant::now();
        self.leader_id = Some(leader_id.clone());

        // Reject if term is stale
        if term < self.current_term {
            return Ok(Message::AppendEntriesResponse {
                term: self.current_term,
                success: false,
                match_index: 0,
            });
        }

        // Check log consistency
        if prev_log_index > 0 {
            if let Some(prev_entry) = self.storage.get(prev_log_index) {
                if prev_entry.term != prev_log_term {
                    debug!("Log consistency check failed at index {}", prev_log_index);
                    return Ok(Message::AppendEntriesResponse {
                        term: self.current_term,
                        success: false,
                        match_index: 0,
                    });
                }
            } else {
                debug!("Missing log entry at index {}", prev_log_index);
                return Ok(Message::AppendEntriesResponse {
                    term: self.current_term,
                    success: false,
                    match_index: 0,
                });
            }
        }

        // Append new entries
        if !entries.is_empty() {
            // Remove conflicting entries
            if let Some(first_new_entry) = entries.first() {
                if let Some(existing_entry) = self.storage.get(first_new_entry.id) {
                    if existing_entry.term != first_new_entry.term {
                        self.storage.truncate_from(first_new_entry.id)?;
                    }
                }
            }

            // Append new entries
            for entry in entries {
                self.storage.append(entry)?;
            }
        }

        // Update commit index
        if leader_commit > self.storage.get_commit_index() {
            let new_commit_index = leader_commit.min(self.storage.get_last_log_index());
            self.storage.commit_up_to(new_commit_index)?;
        }

        Ok(Message::AppendEntriesResponse {
            term: self.current_term,
            success: true,
            match_index: self.storage.get_last_log_index(),
        })
    }

    /// Handle RequestVote RPC
    async fn handle_request_vote(
        &mut self,
        term: u64,
        candidate_id: String,
        last_log_index: u64,
        last_log_term: u64,
    ) -> Result<Message> {
        debug!("Received RequestVote from {} for term {}", candidate_id, term);

        // Update term if necessary
        if term > self.current_term {
            self.current_term = term;
            self.voted_for = None;
            self.state = NodeState::Follower;
            self.storage.set_current_term(term)?;
        }

        let vote_granted = term >= self.current_term &&
            (self.voted_for.is_none() || self.voted_for.as_ref() == Some(&candidate_id)) &&
            self.is_log_up_to_date(last_log_index, last_log_term);

        if vote_granted {
            self.voted_for = Some(candidate_id.clone());
            self.last_heartbeat = Instant::now(); // Reset election timeout
            info!("Granted vote to {} for term {}", candidate_id, term);
        } else {
            debug!("Denied vote to {} for term {}", candidate_id, term);
        }

        Ok(Message::RequestVoteResponse {
            term: self.current_term,
            vote_granted,
        })
    }

    /// Handle heartbeat message
    async fn handle_heartbeat(&mut self, term: u64, leader_id: String) -> Result<Message> {
        if term >= self.current_term {
            self.current_term = term;
            self.state = NodeState::Follower;
            self.leader_id = Some(leader_id);
            self.last_heartbeat = Instant::now();
            self.storage.set_current_term(term)?;
        }

        Ok(Message::AppendEntriesResponse {
            term: self.current_term,
            success: true,
            match_index: self.storage.get_last_log_index(),
        })
    }

    /// Request votes from all peers
    async fn request_votes(&mut self) -> Result<()> {
        let message = Message::RequestVote {
            term: self.current_term,
            candidate_id: self.node_id.clone(),
            last_log_index: self.storage.get_last_log_index(),
            last_log_term: self.storage.get_last_log_term(),
        };

        // For simplicity, we'll send vote requests sequentially
        // In a production system, you'd want to send them in parallel and collect responses
        for peer in &self.peers.clone() {
            let peer_addr = *peer;
            let msg = message.clone();
            
            // Send vote request (fire and forget for now)
            if let Ok(response) = self.network.send_message(peer_addr, msg).await {
                if let Message::RequestVoteResponse { vote_granted, .. } = response {
                    if vote_granted {
                        self.votes_received.insert(format!("peer_{}", peer_addr));
                        debug!("Received vote from {}", peer_addr);
                        
                        // Check if we have majority
                        if self.votes_received.len() > (self.peers.len() + 1) / 2 {
                            info!("Won election with {} votes", self.votes_received.len());
                            self.state = NodeState::Leader;
                            self.leader_id = Some(self.node_id.clone());
                            return Ok(());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Replicate log entries to followers (leader only)
    async fn replicate_to_followers(&mut self) -> Result<()> {
        if self.state != NodeState::Leader {
            return Ok(());
        }

        for peer in &self.peers.clone() {
            let next_index = self.next_index.get(peer).copied().unwrap_or(1);
            let entries = self.storage.get_entries_from(next_index);
            
            if !entries.is_empty() {
                let prev_log_index = next_index.saturating_sub(1);
                let prev_log_term = if prev_log_index > 0 {
                    self.storage.get(prev_log_index)
                        .map(|entry| entry.term)
                        .unwrap_or(0)
                } else {
                    0
                };

                let message = Message::AppendEntries {
                    term: self.current_term,
                    leader_id: self.node_id.clone(),
                    prev_log_index,
                    prev_log_term,
                    entries,
                    leader_commit: self.storage.get_commit_index(),
                };

                // Send replication message
                let peer_addr = *peer;
                if let Ok(response) = self.network.send_message(peer_addr, message).await {
                    if let Message::AppendEntriesResponse { success, match_index, .. } = response {
                        if success {
                            debug!("Successfully replicated to {}, match_index: {}", peer_addr, match_index);
                            // Update next_index and match_index for this peer
                            self.next_index.insert(peer_addr, match_index + 1);
                            self.match_index.insert(peer_addr, match_index);
                        } else {
                            debug!("Failed to replicate to {}", peer_addr);
                            // Decrement next_index and retry
                            let current_next = self.next_index.get(&peer_addr).copied().unwrap_or(1);
                            if current_next > 1 {
                                self.next_index.insert(peer_addr, current_next - 1);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Update commit index based on majority replication
    fn update_commit_index(&mut self) -> Result<()> {
        if self.state != NodeState::Leader {
            return Ok(());
        }

        let current_commit_index = self.storage.get_commit_index();
        let last_log_index = self.storage.get_last_log_index();

        // Find the highest index that has been replicated to majority
        for n in (current_commit_index + 1)..=last_log_index {
            let mut replicated_count = 1; // Count leader itself
            
            // Count how many followers have this entry
            for match_index in self.match_index.values() {
                if *match_index >= n {
                    replicated_count += 1;
                }
            }

            // Check if we have majority (more than half of total nodes)
            let total_nodes = self.peers.len() + 1; // +1 for leader
            let majority = (total_nodes / 2) + 1;

            if replicated_count >= majority {
                // Ensure the entry is from current term before committing
                if let Some(entry) = self.storage.get(n) {
                    if entry.term == self.current_term {
                        info!("Committing entries up to index {} (majority: {}/{})", n, replicated_count, total_nodes);
                        self.storage.commit_up_to(n)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if candidate's log is at least as up-to-date as ours
    fn is_log_up_to_date(&self, last_log_index: u64, last_log_term: u64) -> bool {
        let our_last_term = self.storage.get_last_log_term();
        let our_last_index = self.storage.get_last_log_index();

        last_log_term > our_last_term || 
        (last_log_term == our_last_term && last_log_index >= our_last_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_entry::{LogEntry, Command};
    use crate::storage::Storage;
    use crate::network::NetworkHandler;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use tempfile::tempdir;


    async fn create_test_replication_manager(node_id: &str, peers: Vec<SocketAddr>) -> (ReplicationManager, tempfile::TempDir) {
        let temp_dir = tempdir().unwrap();
        let storage = Storage::new(temp_dir.path()).unwrap();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);
        let network = NetworkHandler::new(addr);
        
        let manager = ReplicationManager::new(
            node_id.to_string(),
            storage,
            network,
            peers,
        );
        
        (manager, temp_dir)
    }

    #[tokio::test]
    async fn test_replication_manager_creation() {
        let peers = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8081),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8082),
        ];
        
        let (manager, _temp_dir) = create_test_replication_manager("test-node", peers.clone()).await;
        
        assert_eq!(manager.node_id, "test-node");
        assert_eq!(manager.state, NodeState::Follower);
        assert_eq!(manager.current_term, 0);
        assert_eq!(manager.peers, peers);
        assert!(manager.voted_for.is_none());
        assert!(manager.leader_id.is_none());
    }

    #[tokio::test]
    async fn test_become_leader() {
        let peers = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8081),
        ];
        
        let (mut manager, _temp_dir) = create_test_replication_manager("leader-node", peers).await;
        
        // Initially should be a follower
        assert_eq!(manager.state, NodeState::Follower);
        assert!(manager.leader_id.is_none());
        
        // Become leader
        let result = manager.become_leader().await;
        assert!(result.is_ok());
        
        // Should now be leader
        assert_eq!(manager.state, NodeState::Leader);
        assert_eq!(manager.leader_id, Some("leader-node".to_string()));
        
        // Should have initialized next_index and match_index for peers
        assert!(manager.next_index.contains_key(&SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8081)));
        assert!(manager.match_index.contains_key(&SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8081)));
    }

    #[tokio::test]
    async fn test_append_command_as_follower() {
        let (mut manager, _temp_dir) = create_test_replication_manager("follower-node", vec![]).await;
        
        // Should be follower initially
        assert_eq!(manager.state, NodeState::Follower);
        
        // Try to append command as follower - should fail
        let result = manager.append_command("test-command".to_string()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false);
    }

    #[tokio::test]
    async fn test_append_command_as_leader() {
        let (mut manager, _temp_dir) = create_test_replication_manager("leader-node", vec![]).await;
        
        // Become leader
        manager.become_leader().await.unwrap();
        assert_eq!(manager.state, NodeState::Leader);
        
        // Append command as leader - should succeed
        let result = manager.append_command("test-command".to_string()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true);
        
        // Verify entry was added to storage
        assert_eq!(manager.storage.get_last_log_index(), 1);
        let entry = manager.storage.get(1).unwrap();
        assert_eq!(entry.id, 1);
        assert_eq!(entry.command, Command::operation("test-command".to_string()));
    }

    #[tokio::test]
    async fn test_handle_append_entries_from_newer_term() {
        let (mut manager, _temp_dir) = create_test_replication_manager("follower-node", vec![]).await;
        
        // Initially term 0, follower
        assert_eq!(manager.current_term, 0);
        assert_eq!(manager.state, NodeState::Follower);
        
        // Receive AppendEntries from newer term
        let response = manager.handle_append_entries(
            5, // newer term
            "leader".to_string(),
            0, // prev_log_index
            0, // prev_log_term
            vec![], // no entries (heartbeat)
            0, // leader_commit
        ).await.unwrap();
        
        // Should update term and remain follower
        assert_eq!(manager.current_term, 5);
        assert_eq!(manager.state, NodeState::Follower);
        assert_eq!(manager.leader_id, Some("leader".to_string()));
        
        // Should respond successfully
        match response {
            Message::AppendEntriesResponse { term, success, .. } => {
                assert_eq!(term, 5);
                assert_eq!(success, true);
            }
            _ => panic!("Expected AppendEntriesResponse"),
        }
    }

    #[tokio::test]
    async fn test_handle_append_entries_from_older_term() {
        let (mut manager, _temp_dir) = create_test_replication_manager("node", vec![]).await;
        
        // Set current term to 5
        manager.current_term = 5;
        manager.storage.set_current_term(5).unwrap();
        
        // Receive AppendEntries from older term
        let response = manager.handle_append_entries(
            3, // older term
            "old-leader".to_string(),
            0,
            0,
            vec![],
            0,
        ).await.unwrap();
        
        // Should reject and maintain current term
        assert_eq!(manager.current_term, 5);
        
        match response {
            Message::AppendEntriesResponse { term, success, .. } => {
                assert_eq!(term, 5);
                assert_eq!(success, false);
            }
            _ => panic!("Expected AppendEntriesResponse"),
        }
    }

    #[tokio::test]
    async fn test_handle_append_entries_with_log_entries() {
        let (mut manager, _temp_dir) = create_test_replication_manager("follower", vec![]).await;
        
        // Create test entries
        let entry1 = LogEntry::new(1, 1, Command::operation("command1".to_string()));
        let entry2 = LogEntry::new(2, 1, Command::operation("command2".to_string()));
        let entries = vec![entry1.clone(), entry2.clone()];
        
        // Handle AppendEntries with log entries
        let response = manager.handle_append_entries(
            1,
            "leader".to_string(),
            0, // prev_log_index
            0, // prev_log_term
            entries,
            0,
        ).await.unwrap();
        
        // Should succeed
        match response {
            Message::AppendEntriesResponse { success, match_index, .. } => {
                assert_eq!(success, true);
                assert_eq!(match_index, 2);
            }
            _ => panic!("Expected AppendEntriesResponse"),
        }
        
        // Verify entries were stored
        assert_eq!(manager.storage.get_last_log_index(), 2);
        assert_eq!(manager.storage.get(1).unwrap().command, entry1.command);
        assert_eq!(manager.storage.get(2).unwrap().command, entry2.command);
    }

    #[tokio::test]
    async fn test_handle_request_vote_grant_vote() {
        let (mut manager, _temp_dir) = create_test_replication_manager("voter", vec![]).await;
        
        // Should grant vote for valid candidate
        let response = manager.handle_request_vote(
            1, // term
            "candidate".to_string(),
            0, // last_log_index
            0, // last_log_term
        ).await.unwrap();
        
        match response {
            Message::RequestVoteResponse { term, vote_granted } => {
                assert_eq!(term, 1);
                assert_eq!(vote_granted, true);
            }
            _ => panic!("Expected RequestVoteResponse"),
        }
        
        // Should have updated term and voted_for
        assert_eq!(manager.current_term, 1);
        assert_eq!(manager.voted_for, Some("candidate".to_string()));
    }

    #[tokio::test]
    async fn test_handle_request_vote_deny_already_voted() {
        let (mut manager, _temp_dir) = create_test_replication_manager("voter", vec![]).await;
        
        // Vote for first candidate
        manager.handle_request_vote(1, "candidate1".to_string(), 0, 0).await.unwrap();
        assert_eq!(manager.voted_for, Some("candidate1".to_string()));
        
        // Try to vote for second candidate in same term
        let response = manager.handle_request_vote(
            1, // same term
            "candidate2".to_string(),
            0,
            0,
        ).await.unwrap();
        
        match response {
            Message::RequestVoteResponse { vote_granted, .. } => {
                assert_eq!(vote_granted, false);
            }
            _ => panic!("Expected RequestVoteResponse"),
        }
        
        // Should still be voted for first candidate
        assert_eq!(manager.voted_for, Some("candidate1".to_string()));
    }

    #[tokio::test]
    async fn test_handle_request_vote_deny_stale_term() {
        let (mut manager, _temp_dir) = create_test_replication_manager("voter", vec![]).await;
        
        // Set current term to 5
        manager.current_term = 5;
        manager.storage.set_current_term(5).unwrap();
        
        // Try to vote for candidate with older term
        let response = manager.handle_request_vote(
            3, // older term
            "candidate".to_string(),
            0,
            0,
        ).await.unwrap();
        
        match response {
            Message::RequestVoteResponse { term, vote_granted } => {
                assert_eq!(term, 5);
                assert_eq!(vote_granted, false);
            }
            _ => panic!("Expected RequestVoteResponse"),
        }
    }

    #[tokio::test]
    async fn test_log_up_to_date_comparison() {
        let (mut manager, _temp_dir) = create_test_replication_manager("node", vec![]).await;
        
        // Add some entries to our log
        let entry1 = LogEntry::new(1, 1, Command::operation("cmd1".to_string()));
        let entry2 = LogEntry::new(2, 2, Command::operation("cmd2".to_string()));
        manager.storage.append(entry1).unwrap();
        manager.storage.append(entry2).unwrap();
        
        // Our log: term=2, index=2
        
        // Candidate with higher term should be more up-to-date
        assert!(manager.is_log_up_to_date(1, 3)); // lower index, higher term
        
        // Candidate with same term but higher index should be more up-to-date
        assert!(manager.is_log_up_to_date(3, 2)); // higher index, same term
        
        // Candidate with same term and same index should be up-to-date
        assert!(manager.is_log_up_to_date(2, 2)); // same index, same term
        
        // Candidate with lower term should not be up-to-date
        assert!(!manager.is_log_up_to_date(5, 1)); // higher index, lower term
        
        // Candidate with same term but lower index should not be up-to-date
        assert!(!manager.is_log_up_to_date(1, 2)); // lower index, same term
    }

    #[tokio::test]
    async fn test_start_election() {
        let peers = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8081),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8082),
        ];
        
        let (mut manager, _temp_dir) = create_test_replication_manager("candidate", peers).await;
        
        // Initially follower at term 0
        assert_eq!(manager.state, NodeState::Follower);
        assert_eq!(manager.current_term, 0);
        assert!(manager.voted_for.is_none());
        
        // Start election
        let result = manager.start_election().await;
        assert!(result.is_ok());
        
        // Should become candidate and increment term
        assert_eq!(manager.state, NodeState::Candidate);
        assert_eq!(manager.current_term, 1);
        assert_eq!(manager.voted_for, Some("candidate".to_string()));
        
        // Should have voted for itself
        assert!(manager.votes_received.contains("candidate"));
    }

    #[tokio::test]
    async fn test_should_start_election_timeout() {
        let (mut manager, _temp_dir) = create_test_replication_manager("node", vec![]).await;
        
        // Initially should not start election (just created)
        assert!(!manager.should_start_election());
        
        // Simulate old heartbeat by setting it to past
        manager.last_heartbeat = std::time::Instant::now() - Duration::from_secs(1);
        
        // Now should start election due to timeout
        assert!(manager.should_start_election());
    }

    #[tokio::test]
    async fn test_should_not_start_election_as_leader() {
        let (mut manager, _temp_dir) = create_test_replication_manager("leader", vec![]).await;
        
        // Become leader
        manager.become_leader().await.unwrap();
        
        // Even with old heartbeat, leader should not start election
        manager.last_heartbeat = std::time::Instant::now() - Duration::from_secs(1);
        assert!(!manager.should_start_election());
    }

    #[tokio::test]
    async fn test_client_message_handling() {
        let (mut manager, _temp_dir) = create_test_replication_manager("leader", vec![]).await;
        manager.become_leader().await.unwrap();
        
        // Test ClientAppend as leader
        let response = manager.handle_message(Message::ClientAppend {
            command: "test-command".to_string(),
        }).await.unwrap();
        
        match response {
            Message::ClientAppendResponse { success, leader_id } => {
                assert_eq!(success, true);
                assert_eq!(leader_id, Some("leader".to_string()));
            }
            _ => panic!("Expected ClientAppendResponse"),
        }
        
        // Test ClientQuery
        let response = manager.handle_message(Message::ClientQuery).await.unwrap();
        
        match response {
            Message::ClientQueryResponse { entries, leader_id } => {
                assert_eq!(entries.len(), 1); // Should have the command we just added
                assert_eq!(leader_id, "leader");
            }
            _ => panic!("Expected ClientQueryResponse"),
        }
    }

    #[tokio::test]
    async fn test_client_append_as_follower() {
        let (mut manager, _temp_dir) = create_test_replication_manager("follower", vec![]).await;
        
        // Set a known leader
        manager.leader_id = Some("actual-leader".to_string());
        
        // Test ClientAppend as follower
        let response = manager.handle_message(Message::ClientAppend {
            command: "test-command".to_string(),
        }).await.unwrap();
        
        match response {
            Message::ClientAppendResponse { success, leader_id } => {
                assert_eq!(success, false);
                assert_eq!(leader_id, Some("actual-leader".to_string()));
            }
            _ => panic!("Expected ClientAppendResponse"),
        }
    }

    #[tokio::test]
    async fn test_heartbeat_handling() {
        let (mut manager, _temp_dir) = create_test_replication_manager("follower", vec![]).await;
        
        // Handle heartbeat from leader
        let response = manager.handle_heartbeat(
            5,
            "heartbeat-leader".to_string(),
        ).await.unwrap();
        
        // Should update state
        assert_eq!(manager.current_term, 5);
        assert_eq!(manager.state, NodeState::Follower);
        assert_eq!(manager.leader_id, Some("heartbeat-leader".to_string()));
        
        // Should respond positively
        match response {
            Message::AppendEntriesResponse { term, success, .. } => {
                assert_eq!(term, 5);
                assert_eq!(success, true);
            }
            _ => panic!("Expected AppendEntriesResponse"),
        }
    }

    #[tokio::test]
    async fn test_log_consistency_check_failure() {
        let (mut manager, _temp_dir) = create_test_replication_manager("follower", vec![]).await;
        
        // Add an entry with term 1
        let entry = LogEntry::new(1, 1, Command::operation("existing".to_string()));
        manager.storage.append(entry).unwrap();
        
        // Try to append entries with conflicting prev_log_term
        let new_entry = LogEntry::new(2, 2, Command::operation("new".to_string()));
        let response = manager.handle_append_entries(
            2,
            "leader".to_string(),
            1, // prev_log_index exists
            2, // but prev_log_term doesn't match (should be 1)
            vec![new_entry],
            0,
        ).await.unwrap();
        
        // Should fail consistency check
        match response {
            Message::AppendEntriesResponse { success, .. } => {
                assert_eq!(success, false);
            }
            _ => panic!("Expected AppendEntriesResponse"),
        }
        
        // Original log should be unchanged
        assert_eq!(manager.storage.get_last_log_index(), 1);
    }

    #[tokio::test]
    async fn test_commit_index_update() {
        let (mut manager, _temp_dir) = create_test_replication_manager("follower", vec![]).await;
        
        // Add some entries
        let entry1 = LogEntry::new(1, 1, Command::operation("cmd1".to_string()));
        let entry2 = LogEntry::new(2, 1, Command::operation("cmd2".to_string()));
        manager.storage.append(entry1).unwrap();
        manager.storage.append(entry2).unwrap();
        
        assert_eq!(manager.storage.get_commit_index(), 0);
        
        // Receive AppendEntries with higher leader_commit
        let response = manager.handle_append_entries(
            1,
            "leader".to_string(),
            2, // prev_log_index
            1, // prev_log_term
            vec![], // no new entries
            2, // leader_commit = 2
        ).await.unwrap();
        
        // Should update commit index
        assert_eq!(manager.storage.get_commit_index(), 2);
        
        // Response should be successful
        match response {
            Message::AppendEntriesResponse { success, .. } => {
                assert_eq!(success, true);
            }
            _ => panic!("Expected AppendEntriesResponse"),
        }
    }
} 