use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use time::OffsetDateTime;

const K: u8 = 20;

type Id = u128;

#[derive(Debug, Clone, Copy)]
pub enum ConnState {
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy)]
pub struct PeerMeta {
    pub listening_addr: SocketAddr,
    pub last_seen: OffsetDateTime,
    pub conn_state: ConnState,
}

impl PeerMeta {
    fn new(listening_addr: SocketAddr, last_seen: OffsetDateTime, conn_state: ConnState) -> Self {
        Self {
            listening_addr,
            last_seen,
            conn_state,
        }
    }
}

pub struct RoutingTable {
    // The node's local ID.
    pub local_id: Id,
    pub max_bucket_size: u8,
    // The buckets for broadcast purposes.
    pub buckets: HashMap<u32, HashSet<Id>>,
    // Contains the connected and disconnected peer information.
    pub peer_list: HashMap<Id, PeerMeta>,
}

impl Default for RoutingTable {
    fn default() -> Self {
        Self {
            // TODO: generate a random u128.
            local_id: 0u128,
            max_bucket_size: K,
            buckets: HashMap::new(),
            // pending: HashMap::new(),
            peer_list: HashMap::new(),
        }
    }
}

impl RoutingTable {
    pub fn new(local_id: Id, max_bucket_size: u8) -> Self {
        Self {
            local_id,
            max_bucket_size,
            ..Default::default()
        }
    }

    // Returns true if the record exists already, false if an attempt was made to insert our local
    // ID.
    pub fn insert(&mut self, id: Id, addr: SocketAddr) -> bool {
        // Buckets should only contain connected peers. The other structures should track
        // connection state.

        // Insert can happen in two instances:
        //
        // 1. the peer initiated the connection (should only be inserted if there is space in the
        //    bucket it would be in).
        // 2. the peer was included in a list from another peer (should be inserted as
        //    disconnected unless it is already in the list and is connected).
        //
        // Solution: insert all addresses as disconnected initially, returning whether the relevant
        // bucket would have space. The caller can then use this information to determine whether
        // to initiate a connection in case 1, or accept the connection in case 2.
        //
        // Eviction logic (a little different to the standard kadcast protocol):
        //
        // 1. nodes are evicted when they disconnect
        // 2. nodes are evicted periodically based on network latency

        if id == self.local_id {
            return false;
        }

        // // Calculate the distance by XORing the ids.
        // let distance = id ^ self.local_id;

        // // Don't calculate the log if distance is 0, this should only happen if the ID we got from
        // // the peer is the same as ours.
        // if distance == u128::MIN {
        //     return false;
        // }

        // Insert the peer into the set, if it doesn't exist.
        self.peer_list.entry(id).or_insert_with(|| {
            PeerMeta::new(
                addr,
                // TODO: this isn't correct as nodes we haven't connected to haven't been "seen".
                OffsetDateTime::now_utc(),
                ConnState::Disconnected,
            )
        });

        true
    }

    // Returns if there is space in the particular bucket for that ID and the appropriate bucket
    // index if there is.
    pub fn can_connect(&mut self, id: Id) -> (bool, Option<u32>) {
        // Calculate the distance by XORing the ids.
        let distance = id ^ self.local_id;

        // Don't calculate the log if distance is 0, this should only happen if the ID we got from
        // the peer is the same as ours.
        if distance == u128::MIN {
            return (false, None);
        }

        // Calculate the index of the bucket from the distance.
        // Nightly feature.
        let i = distance.log2();

        let bucket = self.buckets.entry(i).or_insert_with(HashSet::new);

        match bucket.len().cmp(&self.max_bucket_size.into()) {
            Ordering::Less => {
                // Bucket still has space. Signal the value could be inserted into the bucket (once
                // the connection is succesful).
                (true, Some(i))
            }
            Ordering::Equal => {
                // Bucket is full. Signal the value can't currently be inserted into the bucket.
                (false, None)
            }
            Ordering::Greater => {
                // Bucket is over capacity, this should never happen.
                unreachable!()
            }
        }
    }

    pub fn set_connected(&mut self, id: Id) -> bool {
        match self.can_connect(id) {
            (true, Some(i)) => {
                // TODO: if this is true, the id was already in the bucket, this should probably be handled
                // in some way or another, currently we just update the peer metadata.
                if let Some(bucket) = self.buckets.get_mut(&i) {
                    bucket.insert(id);
                }

                if let Some(peer_meta) = self.peer_list.get_mut(&id) {
                    peer_meta.conn_state = ConnState::Connected;
                }

                true
            }

            _ => false,
        }
    }

    pub fn set_last_seen(&mut self, id: Id, last_seen: OffsetDateTime) {
        if let Some(peer_meta) = self.peer_list.get_mut(&id) {
            peer_meta.last_seen = last_seen
        }
    }

    pub fn find_k_closest(&self, id: Id, k: usize) -> Vec<(Id, PeerMeta)> {
        // Find the K closest nodes to the given ID. There is a total order over the keyspace, so a
        // sort won't yield any conflicts.
        //
        // Naive way: just iterate over all the IDs and XOR them? Need a map of ID to the addr, to
        // be sent to the requesting node.
        let mut ids: Vec<_> = self
            .peer_list
            .iter()
            .map(|(&candidate_id, &candidate_meta)| (candidate_id, candidate_meta))
            .collect();
        ids.sort_by_key(|(candidate_id, _)| candidate_id ^ id);
        ids.truncate(k);

        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert() {
        let mut rt = RoutingTable::new(0, 1);

        // Attempt to insert our local id.
        assert!(!rt.insert(rt.local_id, "127.0.0.1:0".parse().unwrap()));

        // ... 0001 -> bucket i = 0
        assert!(rt.insert(1, "127.0.0.1:1".parse().unwrap()));
        // ... 0010 -> bucket i = 1
        assert!(rt.insert(2, "127.0.0.1:2".parse().unwrap()));
        // ... 0011 -> bucket i = 1
        // This should still return true, since no peers have been inserted into the buckets yet
        // and there is still space.
        assert!(rt.insert(3, "127.0.0.1:3".parse().unwrap()));
    }

    #[test]
    fn set_connected() {
        // Set the max bucket size to a low value so we can easily test when it's full.
        let mut rt = RoutingTable::new(0, 1);

        // ... 0001 -> bucket i = 0
        rt.insert(1, "127.0.0.1:1".parse().unwrap());
        assert!(rt.set_connected(1));
        // ... 0010 -> bucket i = 1
        rt.insert(2, "127.0.0.1:2".parse().unwrap());
        assert!(rt.set_connected(2));
        // ... 0011 -> bucket i = 1
        rt.insert(3, "127.0.0.1:3".parse().unwrap());
        assert!(!rt.set_connected(3));
    }

    #[test]
    fn find_k_closest() {
        let mut rt = RoutingTable::new(0, 5);

        // Generate 5 IDs and addressses.
        let peers: Vec<(Id, SocketAddr)> = (1..=5)
            .into_iter()
            .map(|i| (i as u128, format!("127.0.0.1:{}", i).parse().unwrap()))
            .collect();

        for peer in peers {
            assert!(rt.insert(peer.0, peer.1));
            assert!(rt.set_connected(peer.0));
        }

        let k = 3;
        let k_closest = rt.find_k_closest(rt.local_id, k);

        assert_eq!(k_closest.len(), 3);

        // The closest IDs are in the same order as the indexes, they are however offset by 1.
        for (i, (id, peer_meta)) in k_closest.into_iter().enumerate() {
            assert_eq!(id, (i + 1) as u128);
            assert_eq!(peer_meta.listening_addr.port(), (i + 1) as u16);
        }
    }
}
