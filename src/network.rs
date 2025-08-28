use crate::log_entry::LogEntry;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, debug};

/// Messages exchanged between nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    /// Append entries RPC (for replication)
    AppendEntries {
        term: u64,
        leader_id: String,
        prev_log_index: u64,
        prev_log_term: u64,
        entries: Vec<LogEntry>,
        leader_commit: u64,
    },
    /// Response to append entries RPC
    AppendEntriesResponse {
        term: u64,
        success: bool,
        match_index: u64,
    },
    /// Request vote RPC (for leader election)
    RequestVote {
        term: u64,
        candidate_id: String,
        last_log_index: u64,
        last_log_term: u64,
    },
    /// Response to request vote RPC
    RequestVoteResponse {
        term: u64,
        vote_granted: bool,
    },
    /// Client request to append a command
    ClientAppend {
        command: String,
    },
    /// Response to client append request
    ClientAppendResponse {
        success: bool,
        leader_id: Option<String>,
    },
    /// Client request to query the log
    ClientQuery,
    /// Response to client query
    ClientQueryResponse {
        entries: Vec<LogEntry>,
        leader_id: String,
    },
    /// Heartbeat message
    Heartbeat {
        term: u64,
        leader_id: String,
    },
}

/// Network handler for a node
pub struct NetworkHandler {
    /// Local address this node listens on
    local_addr: SocketAddr,
    /// TCP listener
    listener: Option<TcpListener>,
}

impl NetworkHandler {
    /// Create a new network handler
    pub fn new(local_addr: SocketAddr) -> Self {
        Self {
            local_addr,
            listener: None,
        }
    }

    /// Start listening for incoming connections
    pub async fn start_listening(&mut self) -> Result<()> {
        let listener = TcpListener::bind(self.local_addr).await
            .context("Failed to bind TCP listener")?;
        
        info!("Listening on {}", self.local_addr);
        self.listener = Some(listener);
        Ok(())
    }

    /// Accept incoming connections and return the stream
    pub async fn accept(&mut self) -> Result<(TcpStream, SocketAddr)> {
        if let Some(listener) = &self.listener {
            let (stream, addr) = listener.accept().await
                .context("Failed to accept connection")?;
            debug!("Accepted connection from {}", addr);
            Ok((stream, addr))
        } else {
            anyhow::bail!("Network handler not started");
        }
    }

    /// Send a message to a peer
    pub async fn send_message(&self, peer_addr: SocketAddr, message: Message) -> Result<Message> {
        debug!("Sending message to {}: {:?}", peer_addr, message);
        
        let mut stream = TcpStream::connect(peer_addr).await
            .context("Failed to connect to peer")?;

        // Serialize and send message
        let message_bytes = bincode::serialize(&message)
            .context("Failed to serialize message")?;
        
        let message_len = message_bytes.len() as u32;
        stream.write_u32(message_len).await
            .context("Failed to write message length")?;
        
        stream.write_all(&message_bytes).await
            .context("Failed to write message")?;

        // Read response
        let response_len = stream.read_u32().await
            .context("Failed to read response length")?;
        
        let mut response_bytes = vec![0u8; response_len as usize];
        stream.read_exact(&mut response_bytes).await
            .context("Failed to read response")?;

        let response: Message = bincode::deserialize(&response_bytes)
            .context("Failed to deserialize response")?;

        debug!("Received response from {}: {:?}", peer_addr, response);
        Ok(response)
    }

    /// Read a message from a stream
    pub async fn read_message(stream: &mut TcpStream) -> Result<Message> {
        let message_len = stream.read_u32().await
            .context("Failed to read message length")?;
        
        let mut message_bytes = vec![0u8; message_len as usize];
        stream.read_exact(&mut message_bytes).await
            .context("Failed to read message")?;

        let message: Message = bincode::deserialize(&message_bytes)
            .context("Failed to deserialize message")?;

        Ok(message)
    }

    /// Send a response message to a stream
    pub async fn send_response(stream: &mut TcpStream, response: Message) -> Result<()> {
        let response_bytes = bincode::serialize(&response)
            .context("Failed to serialize response")?;
        
        let response_len = response_bytes.len() as u32;
        stream.write_u32(response_len).await
            .context("Failed to write response length")?;
        
        stream.write_all(&response_bytes).await
            .context("Failed to write response")?;

        Ok(())
    }
}

/// Client for sending requests to the cluster
pub struct Client;

impl Client {
    /// Send an append command to a node
    pub async fn append_command(target: SocketAddr, command: String) -> Result<bool> {
        let message = Message::ClientAppend { command };
        
        let mut stream = TcpStream::connect(target).await
            .context("Failed to connect to target node")?;

        // Send request
        let message_bytes = bincode::serialize(&message)
            .context("Failed to serialize message")?;
        
        let message_len = message_bytes.len() as u32;
        stream.write_u32(message_len).await
            .context("Failed to write message length")?;
        
        stream.write_all(&message_bytes).await
            .context("Failed to write message")?;

        // Read response
        let response_len = stream.read_u32().await
            .context("Failed to read response length")?;
        
        let mut response_bytes = vec![0u8; response_len as usize];
        stream.read_exact(&mut response_bytes).await
            .context("Failed to read response")?;

        let response: Message = bincode::deserialize(&response_bytes)
            .context("Failed to deserialize response")?;

        match response {
            Message::ClientAppendResponse { success, leader_id } => {
                if !success {
                    if let Some(leader) = leader_id {
                        info!("Request failed, leader is: {}", leader);
                    } else {
                        info!("Request failed, no known leader");
                    }
                }
                Ok(success)
            },
            _ => anyhow::bail!("Unexpected response type"),
        }
    }

    /// Query the log from a node
    pub async fn query_log(target: SocketAddr) -> Result<Vec<LogEntry>> {
        let message = Message::ClientQuery;
        
        let mut stream = TcpStream::connect(target).await
            .context("Failed to connect to target node")?;

        // Send request
        let message_bytes = bincode::serialize(&message)
            .context("Failed to serialize message")?;
        
        let message_len = message_bytes.len() as u32;
        stream.write_u32(message_len).await
            .context("Failed to write message length")?;
        
        stream.write_all(&message_bytes).await
            .context("Failed to write message")?;

        // Read response
        let response_len = stream.read_u32().await
            .context("Failed to read response length")?;
        
        let mut response_bytes = vec![0u8; response_len as usize];
        stream.read_exact(&mut response_bytes).await
            .context("Failed to read response")?;

        let response: Message = bincode::deserialize(&response_bytes)
            .context("Failed to deserialize response")?;

        match response {
            Message::ClientQueryResponse { entries, leader_id } => {
                info!("Query successful, leader is: {}", leader_id);
                Ok(entries)
            },
            _ => anyhow::bail!("Unexpected response type"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[tokio::test]
    async fn test_message_serialization() {
        let message = Message::Heartbeat {
            term: 1,
            leader_id: "node1".to_string(),
        };

        let serialized = bincode::serialize(&message).unwrap();
        let deserialized: Message = bincode::deserialize(&serialized).unwrap();

        match deserialized {
            Message::Heartbeat { term, leader_id } => {
                assert_eq!(term, 1);
                assert_eq!(leader_id, "node1");
            },
            _ => panic!("Wrong message type"),
        }
    }
} 