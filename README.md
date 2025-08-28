# Log Replication System

A distributed log replication system built in Rust, implementing a simplified version of the Raft consensus algorithm. This system ensures that a sequence of operations (log entries) is replicated consistently across multiple nodes in a cluster.

## What is a Log Replication System?

A log replication system is a fundamental component of distributed systems that ensures data consistency across multiple nodes. It works by:

1. **Write-Ahead Logging (WAL)**: All operations are first written to a persistent log before being applied
2. **Leader-Follower Architecture**: One node acts as the leader and coordinates replication to followers
3. **Consensus**: Ensures that a majority of nodes agree on the log state before committing entries
4. **Fault Tolerance**: Can continue operating even if some nodes fail
5. **Crash Recovery**: Nodes can recover their state from persistent logs after crashes

## Architecture

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│   Client    │    │   Client    │    │   Client    │
└──────┬──────┘    └──────┬──────┘    └──────┬──────┘
       │                  │                  │
       └──────────────────┼──────────────────┘
                          │
                    ┌─────▼─────┐
                    │  Leader   │
                    │  Node A   │
                    └─────┬─────┘
                          │
          ┌───────────────┼───────────────┐
          │               │               │
    ┌─────▼─────┐   ┌─────▼─────┐   ┌─────▼─────┐
    │ Follower  │   │ Follower  │   │ Follower  │
    │  Node B   │   │  Node C   │   │  Node D   │
    └───────────┘   └───────────┘   └───────────┘
```

### Key Components

- **LogEntry**: Represents a single operation with metadata (term, timestamp, command)
- **Storage**: Persistent storage for log entries with crash recovery
- **NetworkHandler**: TCP-based communication between nodes
- **ReplicationManager**: Core replication logic implementing Raft-like consensus
- **Node**: Main coordinator that ties all components together

## Features

- ✅ **Persistent Storage**: Log entries are stored on disk with crash recovery
- ✅ **Leader Election**: Automatic leader election when nodes start or leaders fail
- ✅ **Log Replication**: Leaders replicate entries to followers with consistency checks
- ✅ **Consensus**: Only commits entries when majority of nodes acknowledge them
- ✅ **Network Communication**: TCP-based messaging between nodes
- ✅ **Client Interface**: CLI for appending commands and querying log state
- ✅ **Heartbeats**: Leaders send periodic heartbeats to maintain authority
- ✅ **Term Management**: Prevents split-brain scenarios with term-based leadership

## Installation & Usage

### Prerequisites

- Rust 1.70+ with Cargo
- Network access between nodes (if running distributed)

### Build

```bash
cargo build --release
```

### Running a Single Node (Leader)

Start a leader node:

```bash
cargo run -- start -i node1 -a 127.0.0.1:8080 --leader
```

### Running a 3-Node Cluster

**Terminal 1 - Start Leader:**
```bash
cargo run -- start -i node1 -a 127.0.0.1:8080 -p "127.0.0.1:8081,127.0.0.1:8082" --leader
```

**Terminal 2 - Start Follower:**
```bash
cargo run -- start -i node2 -a 127.0.0.1:8081 -p "127.0.0.1:8080,127.0.0.1:8082"
```

**Terminal 3 - Start Follower:**
```bash
cargo run -- start -i node3 -a 127.0.0.1:8082 -p "127.0.0.1:8080,127.0.0.1:8081"
```

### Client Operations

**Append a command to the log:**
```bash
cargo run -- append -t 127.0.0.1:8080 -c "user_login:alice"
cargo run -- append -t 127.0.0.1:8080 -c "create_order:order123"
cargo run -- append -t 127.0.0.1:8080 -c "payment_processed:$100"
```

**Query the current log state:**
```bash
cargo run -- query -t 127.0.0.1:8080
```

Example output:
```
📋 Log entries (3 total):
  1. ID:1 Term:1 Committed:true Command:Operation { data: "user_login:alice" }
  2. ID:2 Term:1 Committed:true Command:Operation { data: "create_order:order123" }
  3. ID:3 Term:1 Committed:true Command:Operation { data: "payment_processed:$100" }
```

## Command Reference

### Start a Node
```bash
cargo run -- start [OPTIONS]

Options:
  -i, --id <ID>              Node ID (required)
  -a, --address <ADDRESS>    Address to bind to [default: 127.0.0.1:8080]
  -p, --peers <PEERS>        Peer addresses (comma-separated)
  -l, --leader               Start as leader
```

### Append Command
```bash
cargo run -- append [OPTIONS]

Options:
  -t, --target <TARGET>      Target node address [default: 127.0.0.1:8080]
  -c, --command <COMMAND>    Command to append (required)
```

### Query Log
```bash
cargo run -- query [OPTIONS]

Options:
  -t, --target <TARGET>      Target node address [default: 127.0.0.1:8080]
```

## How It Works

### 1. Leader Election
- When nodes start, they begin as followers
- If no heartbeat is received within the election timeout (150-300ms), a follower becomes a candidate
- Candidates request votes from other nodes
- A candidate becomes leader if it receives majority votes

### 2. Log Replication
- Clients send commands to the leader
- Leader appends the command to its local log
- Leader replicates the entry to all followers via AppendEntries RPC
- Once majority of followers acknowledge, the leader commits the entry
- Committed entries are applied to the state machine

### 3. Consistency Guarantees
- **Log Matching**: Followers reject entries that don't match their log history
- **Leader Completeness**: New leaders must have all committed entries
- **State Machine Safety**: All nodes apply committed entries in the same order

### 4. Fault Tolerance
- System continues operating with majority of nodes available
- Failed nodes can rejoin and catch up by replicating missing entries
- Split-brain prevention through term-based leadership

## Data Storage

Each node stores data in `data/<node-id>/`:
- `log.jsonl`: Append-only log entries (one JSON object per line)
- `metadata.json`: Node metadata (current term, commit index, etc.)

## Use Cases

This log replication system can be used as a building block for:

- **Distributed Databases**: Ensure consistency across database replicas
- **Event Sourcing**: Maintain ordered sequence of domain events
- **Configuration Management**: Replicate configuration changes across services
- **Message Queues**: Durable message ordering with fault tolerance
- **Blockchain**: Consensus on transaction ordering
- **Distributed State Machines**: Any system requiring consistent state updates

## Limitations

This is a simplified implementation for educational purposes. Production systems would need:

- **Snapshots**: Periodic log compaction to prevent unbounded growth
- **Dynamic Membership**: Adding/removing nodes from the cluster
- **Optimizations**: Batching, pipelining, and performance improvements
- **Security**: Authentication, authorization, and encrypted communication
- **Monitoring**: Metrics, health checks, and observability
- **Advanced Recovery**: More sophisticated crash recovery mechanisms

## Testing

Run the test suite:
```bash
cargo test
```

Run specific module tests:
```bash
cargo test storage
cargo test network
cargo test replication
```

## Contributing

1. Fork the repository
2. Create a feature branch
3. Add tests for new functionality
4. Ensure all tests pass
5. Submit a pull request

## License

MIT License - see LICENSE file for details. 