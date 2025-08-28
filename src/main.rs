use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use tracing::{info, error};
use tracing_subscriber;

mod log_entry;
mod storage;
mod node;
mod network;
mod replication;

use node::Node;

#[derive(Parser)]
#[command(name = "log-replication-system")]
#[command(about = "A distributed log replication system")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a node in the cluster
    Start {
        /// Node ID
        #[arg(short, long)]
        id: String,
        /// Address to bind to
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        address: SocketAddr,
        /// Peer addresses (comma-separated)
        #[arg(short, long)]
        peers: Option<String>,
        /// Whether this node should start as leader
        #[arg(short, long)]
        leader: bool,
    },
    /// Send a command to append to the log
    Append {
        /// Target node address
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        target: SocketAddr,
        /// Command to append
        #[arg(short, long)]
        command: String,
    },
    /// Query the current log state
    Query {
        /// Target node address
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        target: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { id, address, peers, leader } => {
            info!("Starting node {} on {}", id, address);
            
            let peer_addresses = peers
                .map(|p| p.split(',').map(|addr| addr.trim().parse().unwrap()).collect())
                .unwrap_or_default();

            let mut node = Node::new(id, address, peer_addresses).await?;
            
            if leader {
                info!("Starting as leader");
                node.become_leader().await?;
            }
            
            node.run().await?;
        },
        Commands::Append { target, command } => {
            info!("Sending append command '{}' to {}", command, target);
            match node::send_append_command(target, command).await {
                Ok(success) => {
                    if success {
                        println!("✅ Command appended successfully!");
                    } else {
                        println!("❌ Failed to append command (not leader or other error)");
                    }
                },
                Err(e) => {
                    error!("Failed to send command: {}", e);
                    println!("❌ Error: {}", e);
                }
            }
        },
        Commands::Query { target } => {
            info!("Querying log state from {}", target);
            match node::query_log(target).await {
                Ok(entries) => {
                    println!("📋 Log entries ({} total):", entries.len());
                    for (i, entry) in entries.iter().enumerate() {
                        println!("  {}. ID:{} Term:{} Committed:{} Command:{:?}", 
                                i + 1, entry.id, entry.term, entry.committed, entry.command);
                    }
                },
                Err(e) => {
                    error!("Failed to query log: {}", e);
                    println!("❌ Error: {}", e);
                }
            }
        },
    }

    Ok(())
}
