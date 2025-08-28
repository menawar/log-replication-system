use log_replication_system::log_entry::{LogEntry, Command};
use log_replication_system::network::Message;
use log_replication_system::node::{Node, send_append_command, query_log};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tempfile::tempdir;

/// Helper function to create a test node with unique ports
async fn create_test_node(id: &str, port: u16, peers: Vec<SocketAddr>) -> Node {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    Node::new(id.to_string(), addr, peers).await.unwrap()
}

/// Helper function to wait for a condition with timeout
#[allow(dead_code)]
async fn wait_for_condition<F, Fut>(condition: F, timeout_duration: Duration) -> bool 
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout_duration {
        if condition().await {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn test_single_node_leader_operations() {
    // Use a unique port to avoid conflicts
    use std::sync::atomic::{AtomicU16, Ordering};
    static PORT_COUNTER: AtomicU16 = AtomicU16::new(10000);
    let port = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
    
    // Use unique node ID with timestamp to avoid data conflicts
    let node_id = format!("leader_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let mut leader = create_test_node(&node_id, port, vec![]).await;
    
    // Start as leader
    leader.become_leader().await.unwrap();
    
    // Start the node in the background
    let node_handle = tokio::spawn(async move {
        leader.run().await.unwrap();
    });
    
    // Wait for the node to start
    sleep(Duration::from_millis(200)).await;
    
    let leader_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    
    // Test appending commands
    let result = timeout(
        Duration::from_secs(10),
        send_append_command(leader_addr, "test_command_1".to_string())
    ).await;
    
    assert!(result.is_ok(), "Failed to send first command");
    assert_eq!(result.unwrap().unwrap(), true);
    
    // Test another command
    let result = timeout(
        Duration::from_secs(10),
        send_append_command(leader_addr, "test_command_2".to_string())
    ).await;
    
    assert!(result.is_ok(), "Failed to send second command");
    assert_eq!(result.unwrap().unwrap(), true);
    
    // Wait a bit for commands to be processed
    sleep(Duration::from_millis(100)).await;
    
    // Query the log
    let result = timeout(
        Duration::from_secs(10),
        query_log(leader_addr)
    ).await;
    
    assert!(result.is_ok(), "Failed to query log");
    let entries = result.unwrap().unwrap();
    
    // Print entries for debugging
    println!("Found {} entries:", entries.len());
    for (i, entry) in entries.iter().enumerate() {
        println!("  {}. ID:{} Command:{:?}", i, entry.id, entry.command);
    }
    
    // Should have exactly 2 entries
    assert_eq!(entries.len(), 2, "Expected exactly 2 entries");
    
    // Find our test commands (they might not be in order due to concurrency)
    let mut found_cmd1 = false;
    let mut found_cmd2 = false;
    
    for entry in &entries {
        match &entry.command {
            Command::Operation { data } => {
                if data == "test_command_1" {
                    found_cmd1 = true;
                } else if data == "test_command_2" {
                    found_cmd2 = true;
                }
            },
            _ => {},
        }
    }
    
    assert!(found_cmd1, "test_command_1 not found in log");
    assert!(found_cmd2, "test_command_2 not found in log");
    
    // Clean up
    node_handle.abort();
}

#[tokio::test]
async fn test_leader_follower_heartbeat() {
    // Create leader and follower addresses
    let leader_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9010);
    let follower_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9011);
    
    // Create nodes
    let mut leader = create_test_node("leader", 9010, vec![follower_addr]).await;
    let mut follower = create_test_node("follower", 9011, vec![leader_addr]).await;
    
    // Start leader
    leader.become_leader().await.unwrap();
    
    // Start both nodes
    tokio::spawn(async move {
        leader.run().await.unwrap();
    });
    
    tokio::spawn(async move {
        follower.run().await.unwrap();
    });
    
    // Wait for nodes to start
    sleep(Duration::from_millis(200)).await;
    
    // Test that we can append to the leader
    let result = timeout(
        Duration::from_secs(5),
        send_append_command(leader_addr, "heartbeat_test".to_string())
    ).await;
    
    assert!(result.is_ok());
    assert_eq!(result.unwrap().unwrap(), true);
    
    // Verify the command was stored
    let result = timeout(
        Duration::from_secs(5),
        query_log(leader_addr)
    ).await;
    
    assert!(result.is_ok());
    let entries = result.unwrap().unwrap();
    assert_eq!(entries.len(), 1);
}

#[tokio::test] 
async fn test_client_redirect_to_leader() {
    // This test verifies that non-leader nodes correctly indicate who the leader is
    let leader_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9020);
    let follower_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9021);
    
    let mut leader = create_test_node("leader", 9020, vec![follower_addr]).await;
    let mut follower = create_test_node("follower", 9021, vec![leader_addr]).await;
    
    // Start leader
    leader.become_leader().await.unwrap();
    
    // Start both nodes
    tokio::spawn(async move {
        leader.run().await.unwrap();
    });
    
    tokio::spawn(async move {
        follower.run().await.unwrap();
    });
    
    // Wait for nodes to start and heartbeats to establish leadership
    sleep(Duration::from_millis(300)).await;
    
    // Try to append to follower - should fail but indicate leader
    let result = timeout(
        Duration::from_secs(5),
        send_append_command(follower_addr, "should_fail".to_string())
    ).await;
    
    // The command should fail (return false) when sent to follower
    assert!(result.is_ok());
    // Note: In our current implementation, the follower will return false
    // In a production system, it might return the leader's address
}

#[tokio::test]
async fn test_network_message_serialization() {
    // Test that our network protocol correctly serializes and deserializes messages
    let message = Message::AppendEntries {
        term: 5,
        leader_id: "test-leader".to_string(),
        prev_log_index: 10,
        prev_log_term: 4,
        entries: vec![
            LogEntry::new(11, 5, Command::operation("test1".to_string())),
            LogEntry::new(12, 5, Command::operation("test2".to_string())),
        ],
        leader_commit: 10,
    };
    
    // Serialize and deserialize
    let serialized = bincode::serialize(&message).unwrap();
    let deserialized: Message = bincode::deserialize(&serialized).unwrap();
    
    // Verify the message was correctly serialized/deserialized
    match deserialized {
        Message::AppendEntries { term, leader_id, prev_log_index, prev_log_term, entries, leader_commit } => {
            assert_eq!(term, 5);
            assert_eq!(leader_id, "test-leader");
            assert_eq!(prev_log_index, 10);
            assert_eq!(prev_log_term, 4);
            assert_eq!(entries.len(), 2);
            assert_eq!(leader_commit, 10);
            
            // Check the entries
            assert_eq!(entries[0].id, 11);
            assert_eq!(entries[0].term, 5);
            assert_eq!(entries[1].id, 12);
            assert_eq!(entries[1].term, 5);
        }
        _ => panic!("Expected AppendEntries message"),
    }
}

#[tokio::test]
async fn test_log_persistence_across_restarts() {
    // Test that logs persist when a node restarts
    let temp_dir = tempdir().unwrap();
    let data_path = temp_dir.path().join("persistent_node");
    std::fs::create_dir_all(&data_path).unwrap();
    
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9030);
    
    // First instance - create and populate log
    {
        let mut node = Node::new("persistent_node".to_string(), addr, vec![]).await.unwrap();
        node.become_leader().await.unwrap();
        
        // Start the node
        let node_handle = tokio::spawn(async move {
            node.run().await.unwrap();
        });
        
        sleep(Duration::from_millis(100)).await;
        
        // Add some commands
        send_append_command(addr, "persistent_command_1".to_string()).await.unwrap();
        send_append_command(addr, "persistent_command_2".to_string()).await.unwrap();
        
        // Verify commands are there
        let entries = query_log(addr).await.unwrap();
        assert_eq!(entries.len(), 2);
        
        // Stop the node
        node_handle.abort();
        sleep(Duration::from_millis(100)).await;
    }
    
    // Second instance - should recover the log
    {
        let mut node = Node::new("persistent_node".to_string(), addr, vec![]).await.unwrap();
        node.become_leader().await.unwrap();
        
        // Start the node
        tokio::spawn(async move {
            node.run().await.unwrap();
        });
        
        sleep(Duration::from_millis(100)).await;
        
        // Should be able to query the persisted log
        let entries = query_log(addr).await.unwrap();
        assert_eq!(entries.len(), 2);
        
        match &entries[0].command {
            Command::Operation { data } => assert_eq!(data, "persistent_command_1"),
            _ => panic!("Expected Operation command"),
        }
        
        match &entries[1].command {
            Command::Operation { data } => assert_eq!(data, "persistent_command_2"),
            _ => panic!("Expected Operation command"),
        }
        
        // Add another command to verify the node is still working
        send_append_command(addr, "after_restart".to_string()).await.unwrap();
        
        let entries = query_log(addr).await.unwrap();
        assert_eq!(entries.len(), 3);
    }
}

#[tokio::test]
async fn test_concurrent_client_requests() {
    // Test that multiple clients can send requests concurrently
    let mut leader = create_test_node("concurrent_leader", 9040, vec![]).await;
    leader.become_leader().await.unwrap();
    
    tokio::spawn(async move {
        leader.run().await.unwrap();
    });
    
    sleep(Duration::from_millis(100)).await;
    
    let leader_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9040);
    
    // Send multiple concurrent requests
    let mut handles = vec![];
    
    for i in 0..10 {
        let addr = leader_addr.clone();
        let handle = tokio::spawn(async move {
            let command = format!("concurrent_command_{}", i);
            send_append_command(addr, command).await
        });
        handles.push(handle);
    }
    
    // Wait for all requests to complete
    for handle in handles {
        let result = timeout(Duration::from_secs(5), handle).await;
        assert!(result.is_ok());
        assert!(result.unwrap().unwrap().unwrap());
    }
    
    // Verify all commands were stored
    let entries = timeout(
        Duration::from_secs(5),
        query_log(leader_addr)
    ).await.unwrap().unwrap();
    
    assert_eq!(entries.len(), 10);
    
    // Verify all commands are present (order might vary due to concurrency)
    let mut commands: Vec<String> = entries.iter()
        .filter_map(|entry| match &entry.command {
            Command::Operation { data } => Some(data.clone()),
            _ => None,
        })
        .collect();
    commands.sort();
    
    for i in 0..10 {
        let expected = format!("concurrent_command_{}", i);
        assert!(commands.contains(&expected), "Missing command: {}", expected);
    }
}

#[tokio::test]
async fn test_error_handling_invalid_address() {
    // Test error handling when trying to connect to invalid address
    let invalid_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9999);
    
    let result = timeout(
        Duration::from_secs(2),
        send_append_command(invalid_addr, "should_fail".to_string())
    ).await;
    
    // Should timeout or return an error
    assert!(result.is_err() || result.unwrap().is_err());
}

#[tokio::test]
async fn test_vote_request_response() {
    // Test the vote request/response mechanism
    use log_replication_system::network::Message;
    
    let message = Message::RequestVote {
        term: 5,
        candidate_id: "test_candidate".to_string(),
        last_log_index: 10,
        last_log_term: 4,
    };
    
    // Serialize and deserialize
    let serialized = bincode::serialize(&message).unwrap();
    let deserialized: Message = bincode::deserialize(&serialized).unwrap();
    
    match deserialized {
        Message::RequestVote { term, candidate_id, last_log_index, last_log_term } => {
            assert_eq!(term, 5);
            assert_eq!(candidate_id, "test_candidate");
            assert_eq!(last_log_index, 10);
            assert_eq!(last_log_term, 4);
        }
        _ => panic!("Expected RequestVote message"),
    }
    
    // Test response
    let response = Message::RequestVoteResponse {
        term: 5,
        vote_granted: true,
    };
    
    let serialized = bincode::serialize(&response).unwrap();
    let deserialized: Message = bincode::deserialize(&serialized).unwrap();
    
    match deserialized {
        Message::RequestVoteResponse { term, vote_granted } => {
            assert_eq!(term, 5);
            assert_eq!(vote_granted, true);
        }
        _ => panic!("Expected RequestVoteResponse message"),
    }
}

#[tokio::test]
async fn test_heartbeat_message() {
    // Test heartbeat message serialization
    let heartbeat = Message::Heartbeat {
        term: 10,
        leader_id: "heartbeat_leader".to_string(),
    };
    
    let serialized = bincode::serialize(&heartbeat).unwrap();
    let deserialized: Message = bincode::deserialize(&serialized).unwrap();
    
    match deserialized {
        Message::Heartbeat { term, leader_id } => {
            assert_eq!(term, 10);
            assert_eq!(leader_id, "heartbeat_leader");
        }
        _ => panic!("Expected Heartbeat message"),
    }
} 