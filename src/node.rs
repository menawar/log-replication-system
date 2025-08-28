use crate::network::{NetworkHandler, Client};
use crate::replication::{ReplicationManager, NodeState};
use crate::storage::Storage;
use anyhow::{Context, Result};
use std::net::SocketAddr;

use tokio::time::{Duration, interval};
use tracing::{info, error, debug};

/// A node in the log replication cluster
pub struct Node {
    /// Node identifier
    id: String,
    /// Local address
    #[allow(dead_code)]
    address: SocketAddr,
    /// Replication manager
    replication: ReplicationManager,
    /// Network handler
    network: NetworkHandler,
}

impl Node {
    /// Create a new node
    pub async fn new(
        id: String,
        address: SocketAddr,
        peers: Vec<SocketAddr>,
    ) -> Result<Self> {
        // Create data directory for this node
        let data_dir = format!("data/{}", id);
        std::fs::create_dir_all(&data_dir)
            .context("Failed to create data directory")?;

        // Initialize storage
        let storage = Storage::new(&data_dir)
            .context("Failed to initialize storage")?;

        // Initialize network handler
        let mut network = NetworkHandler::new(address);
        network.start_listening().await
            .context("Failed to start network listener")?;

        // Initialize replication manager
        let replication = ReplicationManager::new(
            id.clone(),
            storage,
            NetworkHandler::new(address), // Separate instance for outgoing connections
            peers,
        );

        info!("Node {} initialized on {}", id, address);

        Ok(Self {
            id,
            address,
            replication,
            network,
        })
    }

    /// Become leader
    pub async fn become_leader(&mut self) -> Result<()> {
        self.replication.become_leader().await
    }

    /// Get current node state
    #[allow(dead_code)]
    pub fn get_state(&self) -> &NodeState {
        self.replication.get_state()
    }

    /// Get current term
    #[allow(dead_code)]
    pub fn get_current_term(&self) -> u64 {
        self.replication.get_current_term()
    }

    /// Get leader ID
    #[allow(dead_code)]
    pub fn get_leader_id(&self) -> Option<&String> {
        self.replication.get_leader_id()
    }

    /// Run the main event loop
    pub async fn run(&mut self) -> Result<()> {
        info!("Starting node {} event loop", self.id);

        // Set up periodic tasks
        let mut heartbeat_timer = interval(Duration::from_millis(50)); // 50ms heartbeat
        let mut election_timer = interval(Duration::from_millis(10)); // Check election every 10ms

        loop {
            tokio::select! {
                // Handle incoming network connections
                result = self.network.accept() => {
                    match result {
                        Ok((mut stream, addr)) => {
                            debug!("Handling connection from {}", addr);
                            
                            // Handle the connection directly (synchronously for simplicity)
                            if let Err(e) = self.handle_connection(&mut stream).await {
                                debug!("Error handling connection from {}: {}", addr, e);
                            }
                        },
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                },
                
                // Send heartbeats (if leader)
                _ = heartbeat_timer.tick() => {
                    if self.replication.get_state() == &NodeState::Leader {
                        if let Err(e) = self.replication.send_heartbeats().await {
                            error!("Failed to send heartbeats: {}", e);
                        }
                    }
                },
                
                // Check for election timeout
                _ = election_timer.tick() => {
                    if self.replication.should_start_election() {
                        info!("Election timeout reached, starting election");
                        if let Err(e) = self.replication.start_election().await {
                            error!("Failed to start election: {}", e);
                        }
                    }
                },
            }
        }
    }

    /// Handle a single connection
    async fn handle_connection(
        &mut self,
        stream: &mut tokio::net::TcpStream,
    ) -> Result<()> {
        // Read the incoming message
        let message = NetworkHandler::read_message(stream).await
            .context("Failed to read message")?;

        debug!("Received message: {:?}", message);

        // Process the message
        let response = self.replication.handle_message(message).await
            .context("Failed to handle message")?;

        // Send response
        NetworkHandler::send_response(stream, response).await
            .context("Failed to send response")?;

        Ok(())
    }
}

/// Client functions for interacting with the cluster
impl Node {
    /// Send an append command to this node
    #[allow(dead_code)]
    pub async fn append_command_local(&mut self, command: String) -> Result<bool> {
        self.replication.append_command(command).await
    }
}

/// Standalone client functions
pub async fn send_append_command(target: SocketAddr, command: String) -> Result<bool> {
    Client::append_command(target, command).await
}

pub async fn query_log(target: SocketAddr) -> Result<Vec<crate::log_entry::LogEntry>> {
    Client::query_log(target).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};


    #[tokio::test]
    async fn test_node_creation() -> Result<()> {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);
        let node = Node::new("test-node".to_string(), addr, vec![]).await;
        assert!(node.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn test_leader_election() -> Result<()> {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);
        let mut node = Node::new("test-leader".to_string(), addr, vec![]).await?;
        
        node.become_leader().await?;
        assert_eq!(node.get_state(), &NodeState::Leader);
        
        Ok(())
    }
} 