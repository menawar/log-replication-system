#!/bin/bash

# Demo script for the Log Replication System

echo "🚀 Log Replication System Demo"
echo "================================="

# Clean up any existing data
echo "🧹 Cleaning up previous data..."
rm -rf data/

# Build the project
echo "🔨 Building the project..."
cargo build --release

if [ $? -ne 0 ]; then
    echo "❌ Build failed!"
    exit 1
fi

echo "✅ Build successful!"
echo ""

echo "📖 This demo will show you:"
echo "1. How to start a leader node"
echo "2. How to append commands to the log"
echo "3. How to query the log state"
echo "4. How the system persists data"
echo ""

# Start the leader node in the background
echo "🌟 Starting leader node on 127.0.0.1:9090..."
RUST_LOG=info ./target/release/log-replication-system start -i leader -a 127.0.0.1:9090 --leader &
LEADER_PID=$!

# Wait for the leader to start
echo "⏳ Waiting for leader to initialize..."
sleep 3

# Check if the leader is still running
if ! kill -0 $LEADER_PID 2>/dev/null; then
    echo "❌ Leader failed to start!"
    exit 1
fi

echo "✅ Leader started successfully!"
echo ""

# Test appending commands
echo "📝 Testing command append operations..."
echo ""

echo "Adding command: 'user_login:alice'"
./target/release/log-replication-system append -t 127.0.0.1:9090 -c "user_login:alice"
echo ""

echo "Adding command: 'create_order:order123'"
./target/release/log-replication-system append -t 127.0.0.1:9090 -c "create_order:order123"
echo ""

echo "Adding command: 'payment_processed:$100'"
./target/release/log-replication-system append -t 127.0.0.1:9090 -c "payment_processed:\$100"
echo ""

# Query the log
echo "🔍 Querying the current log state..."
./target/release/log-replication-system query -t 127.0.0.1:9090
echo ""

# Show persistent storage
echo "💾 Checking persistent storage..."
echo "Data directory structure:"
find data/ -type f -exec echo "📄 {}" \; -exec head -3 {} \; -exec echo "" \;

# Clean up
echo "🧹 Cleaning up..."
kill $LEADER_PID 2>/dev/null
wait $LEADER_PID 2>/dev/null

echo ""
echo "🎉 Demo completed!"
echo ""
echo "💡 Try it yourself:"
echo "   1. Start a leader: cargo run -- start -i node1 -a 127.0.0.1:8080 --leader"
echo "   2. Append commands: cargo run -- append -t 127.0.0.1:8080 -c 'your_command'"
echo "   3. Query the log: cargo run -- query -t 127.0.0.1:8080"
echo ""
echo "📚 For multi-node setup, see the README.md file!" 