use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::SocketAddr,
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use ziggurat::protocol::payload::{ProtocolVersion, VarStr};

/// The elapsed time before a connection should be regarded as inactive.
pub const LAST_SEEN_CUTOFF: u64 = 10 * 60;

/// A node encountered in the network or obtained from one of the peers.
#[derive(Debug, Default, Clone)]
pub struct KnownNode {
    // The address is omitted, as it's a key in the owning HashMap.
    /// The last time the node was successfully connected to.
    pub last_connected: Option<Instant>,
    /// The time it took to complete a connection.
    pub handshake_time: Option<Duration>,
    /// The node's protocol version.
    pub protocol_version: Option<ProtocolVersion>,
    /// The node's user agent.
    pub user_agent: Option<VarStr>,
    /// The number of services supported by the node.
    pub services: Option<u64>,
    /// The number of subsequent connection errors.
    pub connection_failures: u8,
}

/// A connection found in the network.
#[derive(Debug, Eq, Copy, Clone)]
pub struct KnownConnection {
    /// One of the two sides of a connection.
    pub a: SocketAddr,
    /// The other side of a connection.
    pub b: SocketAddr,
    /// The timestamp of the last time the connection was seen.
    pub last_seen: Instant,
}

impl PartialEq for KnownConnection {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (self.a, self.b);
        let (c, d) = (other.a, other.b);

        a == d && b == c || a == c && b == d
    }
}

impl Hash for KnownConnection {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (a, b) = (self.a, self.b);

        // This ensures the hash is the same for (a, b) as it is for (b, a).
        match a.cmp(&b) {
            Ordering::Greater => {
                b.hash(state);
                a.hash(state);
            }
            _ => {
                a.hash(state);
                b.hash(state);
            }
        }
    }
}

impl KnownConnection {
    pub fn new(a: SocketAddr, b: SocketAddr) -> Self {
        Self {
            a,
            b,
            last_seen: Instant::now(),
        }
    }
}

/// The list of nodes and connections the crawler is aware of.
#[derive(Default)]
pub struct KnownNetwork {
    pub nodes: RwLock<HashMap<SocketAddr, KnownNode>>,
    pub connections: RwLock<HashSet<KnownConnection>>,
}

impl KnownNetwork {
    /// Extends the list of known nodes.
    pub fn add_addrs(&self, source: SocketAddr, listening_addrs: &[SocketAddr]) {
        {
            let connections = &mut self.connections.write();
            for addr in listening_addrs {
                connections.insert(KnownConnection::new(source, *addr));
            }
        }

        self.update_nodes();
    }

    /// Returns a snapshot of the known connections.
    pub fn connections(&self) -> HashSet<KnownConnection> {
        self.connections.read().clone()
    }

    /// Returns a snapshot of the known nodes.
    pub fn nodes(&self) -> HashMap<SocketAddr, KnownNode> {
        self.nodes.read().clone()
    }

    /// Returns the number of known connections.
    pub fn num_connections(&self) -> usize {
        self.connections.read().len()
    }

    /// Returns the number of known nodes.
    pub fn num_nodes(&self) -> usize {
        self.nodes.read().len()
    }

    /// Prunes the list of known connections by removing connections last seen long ago.
    pub fn remove_old_connections(&self) {
        let mut old_conns: HashSet<KnownConnection> = HashSet::new();
        for conn in self.connections() {
            if conn.last_seen.elapsed().as_secs() > LAST_SEEN_CUTOFF {
                old_conns.insert(conn);
            }
        }

        if !old_conns.is_empty() {
            let mut conns = self.connections.write();
            for conn in old_conns {
                conns.remove(&conn);
            }
        }
    }

    /// Updates the list of known nodes based on the known connections.
    pub fn update_nodes(&self) {
        let mut prospect_nodes: HashSet<SocketAddr> = HashSet::new();
        for connection in self.connections() {
            prospect_nodes.insert(connection.a);
            prospect_nodes.insert(connection.b);
        }

        let mut nodes = self.nodes.write();
        for addr in prospect_nodes {
            nodes.entry(addr).or_default();
        }
    }
}
