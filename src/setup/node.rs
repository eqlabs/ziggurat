//! High level APIs and types for node setup and teardown.

use tracing::error;

use crate::{
    protocol::payload::{
        block::{Block, Headers},
        Hash, Inv,
    },
    setup::config::{NodeConfig, NodeKind, NodeMetaData, ZcashdConfigFile, ZebraConfigFile},
    tools::{
        message_filter::{Filter, MessageFilter},
        synthetic_node::SyntheticNode,
    },
    wait_until,
};

use std::process::{Child, Command};

use std::{fs, io, net::SocketAddr, process::Stdio, time::Duration};

/// Actions to prepare node state on start.
pub enum Action {
    /// Performs no action
    None,
    /// Waits for the node to connect to a local listener, connection is then terminated.
    /// This is useful for indicating that the node has started and is available for
    /// other connections.
    WaitForConnection,
    /// Seeds the node with `n` blocks from the testnet chain, by connecting from a local socket
    /// and sending the appropriate data. After this, the connection is terminated.
    ///
    /// **Warning**: this currently only works for zcashd type nodes, for zebra the behaviour
    /// is equivalent to WaitForConnection.
    SeedWithTestnetBlocks(
        /// The number of initial testnet blocks to seed. Note that this is capped by the number of blocks available
        /// from [Block::initial_testnet_blocks].
        usize,
    ),
}

/// Represents an instance of a node, its configuration and setup/teardown intricacies.
pub struct Node {
    /// Configuration definable in tests and written to the node's configuration file on start.
    config: NodeConfig,
    /// Type, path to binary, various commands for starting, stopping, cleanup, network
    /// configuration.
    meta: NodeMetaData,
    /// Process of the running node.
    process: Option<Child>,
}

impl Node {
    /// Creates a new [`Node`] instance.
    ///
    /// Once created, it can be configured with calls to [`initial_peers`], [`max_peers`] and [`log_to_stdout`].
    ///
    /// [`Node`]: struct@Node
    /// [`NodeMetaData`]: struct@crate::setup::config::NodeMetaData
    /// [`initial_peers`]: method@Node::initial_peers
    /// [`max_peers`]: method@Node::max_peers
    /// [`log_to_stdout`]: method@Node::log_to_stdout
    pub fn new() -> io::Result<Self> {
        // Config (to be written to node configuration file).
        let config = NodeConfig::new();
        let meta = NodeMetaData::new()?;

        Ok(Self {
            config,
            meta,
            process: None,
        })
    }

    /// Returns the (external) address of the node.
    pub fn addr(&self) -> SocketAddr {
        self.config.local_addr
    }

    /// Sets the initial peers (ports only) for the node.
    ///
    /// The ip used to construct the addresses can be optionally set in the configuration file and
    /// otherwise defaults to localhost.
    pub fn initial_peers(&mut self, peers: Vec<SocketAddr>) -> &mut Self {
        self.config.initial_peers = peers.iter().map(|addr| format!("{}", addr)).collect();

        self
    }

    /// Sets the maximum connection value for the node.
    pub fn max_peers(&mut self, max_peers: usize) -> &mut Self {
        self.config.max_peers = max_peers;
        self
    }

    /// Sets whether to log the node's output to Ziggurat's output stream.
    pub fn log_to_stdout(&mut self, log_to_stdout: bool) -> &mut Self {
        self.config.log_to_stdout = log_to_stdout;
        self
    }

    /// Sets the initial action to undertake once the node has started. See [`Action`] for more
    /// information on what the actions pertain.
    pub fn initial_action(&mut self, action: Action) -> &mut Self {
        self.config.initial_action = action;
        self
    }

    /// Starts the node instance.
    ///
    /// This function will write the appropriate configuration file and run the start command
    /// provided in `config.toml`.
    pub async fn start(&mut self) -> io::Result<()> {
        // cleanup any previous runs (node.stop won't always be reached e.g. test panics, or SIGINT)
        self.cleanup()?;

        // Setup the listener if there is some initial action required
        let synthetic_node = match self.config.initial_action {
            Action::None => None,
            Action::WaitForConnection | Action::SeedWithTestnetBlocks(_) => {
                // Start a synthetic node to perform the initial actions.
                let synthetic_node = SyntheticNode::builder()
                    .with_full_handshake()
                    .with_message_filter(
                        MessageFilter::with_all_auto_reply()
                            .with_getheaders_filter(Filter::Disabled)
                            .with_getdata_filter(Filter::Disabled),
                    )
                    .build()
                    .await?;

                self.config
                    .initial_peers
                    .insert(synthetic_node.listening_addr().to_string());

                Some(synthetic_node)
            }
        };

        // Generate config files for Zebra or Zcashd node.
        self.generate_config_file()?;

        let (stdout, stderr) = match self.config.log_to_stdout {
            true => (Stdio::inherit(), Stdio::inherit()),
            false => (Stdio::null(), Stdio::null()),
        };

        let process = Command::new(&self.meta.start_command)
            .current_dir(&self.meta.path)
            .args(&self.meta.start_args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("node failed to start");

        self.process = Some(process);

        if let Some(synthetic_node) = synthetic_node {
            self.perform_initial_action(synthetic_node).await?;
        }

        Ok(())
    }

    async fn perform_initial_action(&self, mut synthetic_node: SyntheticNode) -> io::Result<()> {
        const TIMEOUT: Duration = Duration::from_secs(10);

        match self.config.initial_action {
            Action::None => {}
            Action::WaitForConnection => {
                // The synthetic node will accept the connection and handshake by itself.
                wait_until!(TIMEOUT, synthetic_node.num_connected() == 1);
            }
            Action::SeedWithTestnetBlocks(_) if self.meta.kind == NodeKind::Zebra => {
                unimplemented!("zebra doesn't support block seeding");
            }
            Action::SeedWithTestnetBlocks(block_count) => {
                use crate::protocol::message::Message;

                let genesis_block = Block::testnet_genesis();
                // initial blocks, skipping genesis as it doesn't get sent
                let blocks = Block::initial_testnet_blocks()
                    .into_iter()
                    .take(block_count)
                    .skip(1)
                    .collect::<Vec<_>>();

                // respond to GetHeaders(Block[0])
                let source = match synthetic_node.recv_message_timeout(TIMEOUT).await? {
                    (source, Message::GetHeaders(locations)) => {
                        // The request should be from the genesis hash onwards,
                        // i.e. locator_hash = [genesis.hash], stop_hash = [0]
                        assert_eq!(
                            locations.block_locator_hashes,
                            vec![genesis_block.double_sha256().unwrap()]
                        );
                        assert_eq!(locations.hash_stop, Hash::zeroed());

                        // Reply with headers for the initial block headers
                        let headers = blocks.iter().map(|block| block.header.clone()).collect();
                        synthetic_node
                            .send_direct_message(source, Message::Headers(Headers::new(headers)))
                            .await?;

                        source
                    }

                    (_, msg) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Expected GetHeaders but got: {:?}", msg),
                        ));
                    }
                };

                // respond to GetData(inv) for the initial blocks
                match synthetic_node.recv_message_timeout(TIMEOUT).await? {
                    (source, Message::GetData(inv)) => {
                        // The request must be for the initial blocks
                        let inv_hashes = blocks.iter().map(|block| block.inv_hash()).collect();
                        let expected = Inv::new(inv_hashes);
                        assert_eq!(inv, expected);

                        // Send the blocks
                        for block in blocks {
                            synthetic_node
                                .send_direct_message(source, Message::Block(Box::new(block)))
                                .await?;
                        }
                    }

                    (_, msg) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Expected GetData but got: {:?}", msg),
                        ))
                    }
                }

                // Check that the node has received and processed all previous messages.
                synthetic_node.ping_pong_timeout(source, TIMEOUT).await?;
            }
        }

        // Setup is complete, we no longer require this synthetic node.
        synthetic_node.shut_down();

        Ok(())
    }

    /// Stops the node instance.
    ///
    /// The stop command will only be run if provided in the `config.toml` file as it may not be
    /// necessary to shutdown a node (killing the process is sometimes sufficient).
    pub fn stop(&mut self) -> io::Result<()> {
        if let Some(mut child) = self.process.take() {
            // Stop node process, and check for crash
            // (needs to happen before cleanup)
            let crashed = match child.try_wait()? {
                None => {
                    child.kill()?;
                    None
                }
                Some(exit_code) if exit_code.success() => {
                    Some("but exited successfully somehow".to_string())
                }
                Some(exit_code) => Some(format!("crashed with {}", exit_code)),
            };

            self.cleanup()?;

            if let Some(crash_msg) = crashed {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Node exited early, {}", crash_msg),
                ));
            }
        }

        Ok(())
    }

    fn generate_config_file(&self) -> io::Result<()> {
        let path = self.meta.kind.config_filepath();
        let content = match self.meta.kind {
            NodeKind::Zebra => ZebraConfigFile::generate(&self.config)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            NodeKind::Zcashd => ZcashdConfigFile::generate(&self.config),
        };

        fs::write(path, content)
    }

    fn cleanup(&self) -> io::Result<()> {
        self.cleanup_config_file()?;
        self.cleanup_cache()
    }

    fn cleanup_config_file(&self) -> io::Result<()> {
        let path = self.meta.kind.config_filepath();
        match fs::remove_file(path) {
            // File may not exist, so we surpress the error.
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn cleanup_cache(&self) -> io::Result<()> {
        // No cache for zebra as it is configured in ephemeral mode
        if let (NodeKind::Zcashd, Some(path)) = (self.meta.kind, home::home_dir()) {
            // Default cache location is ~/.zcash, testnet3 is the relevant directory for our
            // tests, unfortunately a custom dir can't be specified at this time.
            let path = path.join(".zcash/testnet3");

            if let Err(e) = fs::remove_dir_all(path) {
                // Directory may not exist, so we let that error through
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
        }

        Ok(())
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // We should not panic in Drop
        if let Err(err) = self.stop() {
            error!("Failed to stop node: {}", err);
        }
    }
}
