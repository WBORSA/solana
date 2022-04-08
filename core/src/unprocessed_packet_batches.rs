use {
    min_max_heap::MinMaxHeap,
    solana_perf::packet::{limited_deserialize, Packet, PacketBatch},
    solana_runtime::bank::Bank,
    solana_sdk::{
        hash::Hash,
        message::{Message, VersionedMessage},
        short_vec::decode_shortu16_len,
        signature::Signature,
        transaction::{Transaction, VersionedTransaction},
    },
    std::{cmp::Ordering, collections::HashMap, mem::size_of, rc::Rc, sync::Arc},
};

#[derive(Debug, Default, PartialEq, Eq)]
struct ImmutableDeserializedPacket {
    original_packet: Packet,
    versioned_transaction: VersionedTransaction,
    message_hash: Hash,
    is_simple_vote: bool,
    fee_per_cu: u64,
}

/// Holds deserialized messages, as well as computed message_hash and other things needed to create
/// SanitizedTransaction
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeserializedPacket {
    immutable_section: Rc<ImmutableDeserializedPacket>,
    pub forwarded: bool,
}

impl DeserializedPacket {
    pub fn new(packet: Packet, bank: &Option<Arc<Bank>>) -> Option<Self> {
        let versioned_transaction: VersionedTransaction =
            match limited_deserialize(&packet.data[0..packet.meta.size]) {
                Ok(tx) => tx,
                Err(_) => return None,
            };

        if let Some(message_bytes) = packet_message(&packet) {
            let message_hash = Message::hash_raw_message(message_bytes);
            let is_simple_vote = packet.meta.is_simple_vote_tx();
            let fee_per_cu = bank
                .as_ref()
                .map(|bank| compute_fee_per_cu(&versioned_transaction.message, &*bank))
                .unwrap_or(0);
            Some(Self {
                immutable_section: Rc::new(ImmutableDeserializedPacket {
                    original_packet: packet,
                    versioned_transaction,
                    message_hash,
                    is_simple_vote,
                    fee_per_cu,
                }),
                forwarded: false,
            })
        } else {
            None
        }
    }

    pub fn original_packet(&self) -> &Packet {
        &self.immutable_section.original_packet
    }

    pub fn is_simple_vote_transaction(&self) -> bool {
        self.immutable_section.is_simple_vote
    }

    pub fn versioned_transaction(&self) -> &VersionedTransaction {
        &self.immutable_section.versioned_transaction
    }

    pub fn sender_stake(&self) -> u64 {
        self.immutable_section.original_packet.meta.sender_stake
    }

    pub fn message_hash(&self) -> Hash {
        self.immutable_section.message_hash
    }

    pub fn fee_per_cu(&self) -> u64 {
        self.immutable_section.fee_per_cu
    }
}

impl PartialOrd for DeserializedPacket {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeserializedPacket {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.fee_per_cu().cmp(&other.fee_per_cu()) {
            Ordering::Equal => self.sender_stake().cmp(&other.sender_stake()),
            ordering => ordering,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PacketPriorityQueueEntry {
    fee_per_cu: u64,
    sender_stake: u64,
    message_hash: Hash,
}

impl PacketPriorityQueueEntry {
    fn from_packet(deserialized_packet: &DeserializedPacket) -> Self {
        Self {
            fee_per_cu: deserialized_packet.fee_per_cu(),
            sender_stake: deserialized_packet.sender_stake(),
            message_hash: deserialized_packet.message_hash(),
        }
    }
}

impl PartialOrd for PacketPriorityQueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PacketPriorityQueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.fee_per_cu.cmp(&other.fee_per_cu) {
            Ordering::Equal => self.sender_stake.cmp(&other.sender_stake),
            ordering => ordering,
        }
    }
}

/// Currently each banking_stage thread has a `UnprocessedPacketBatches` buffer to store
/// PacketBatch's received from sigverify. Banking thread continuously scans the buffer
/// to pick proper packets to add to the block.
#[derive(Default)]
pub struct UnprocessedPacketBatches {
    packet_priority_queue: MinMaxHeap<PacketPriorityQueueEntry>,
    message_hash_to_transaction: HashMap<Hash, DeserializedPacket>,
    batch_limit: usize,
}

impl UnprocessedPacketBatches {
    pub fn from_iter<I: IntoIterator<Item = DeserializedPacket>>(iter: I, capacity: usize) -> Self {
        let mut unprocessed_packet_batches = Self::with_capacity(capacity);
        for deserialized_packet in iter.into_iter() {
            unprocessed_packet_batches.push(deserialized_packet);
        }

        unprocessed_packet_batches
    }

    pub fn with_capacity(capacity: usize) -> Self {
        UnprocessedPacketBatches {
            packet_priority_queue: MinMaxHeap::with_capacity(capacity),
            message_hash_to_transaction: HashMap::with_capacity(capacity),
            batch_limit: capacity,
        }
    }

    pub fn clear(&mut self) {
        self.packet_priority_queue.clear();
        self.message_hash_to_transaction.clear();
    }

    /// Insert new `deserizlized_packet_batch` into inner `MinMaxHeap<DeserializedPacket>`,
    /// weighted first by the fee-per-cu, then the stake of the sender.
    /// If buffer is at the max limit, the lowest weighted packet is dropped
    ///
    /// Returns tuple of number of packets dropped
    pub fn insert_batch(
        &mut self,
        deserialized_packets: impl Iterator<Item = DeserializedPacket>,
    ) -> usize {
        let mut num_dropped_packets = 0;
        for deserialized_packet in deserialized_packets {
            if self.push(deserialized_packet).is_some() {
                num_dropped_packets += 1;
            }
        }
        num_dropped_packets
    }

    pub fn push(&mut self, deserialized_packet: DeserializedPacket) -> Option<DeserializedPacket> {
        if self.len() >= self.batch_limit {
            // Optimized to not allocate by calling `MinMaxHeap::push_pop_min()`
            Some(self.push_pop_min(deserialized_packet))
        } else {
            self.push_internal(deserialized_packet);
            None
        }
    }

    pub fn iter(&mut self) -> impl Iterator<Item = &DeserializedPacket> {
        self.message_hash_to_transaction.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut DeserializedPacket> {
        self.message_hash_to_transaction.iter_mut().map(|(_k, v)| v)
    }

    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut DeserializedPacket) -> bool,
    {
        // TODO: optimize this only when number of packets
        // with oudated blockhash is high
        self.packet_priority_queue.clear();
        self.message_hash_to_transaction
            .retain(|_k, deserialized_packet| {
                let should_retain = f(deserialized_packet);
                if should_retain {
                    let priority_queue_entry =
                        PacketPriorityQueueEntry::from_packet(deserialized_packet);
                    self.packet_priority_queue.push(priority_queue_entry);
                }
                should_retain
            })
    }

    pub fn len(&self) -> usize {
        self.packet_priority_queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packet_priority_queue.is_empty()
    }

    fn push_internal(&mut self, deserialized_packet: DeserializedPacket) {
        let priority_queue_entry = PacketPriorityQueueEntry::from_packet(&deserialized_packet);

        // Push into the priority queue
        self.packet_priority_queue.push(priority_queue_entry);

        // Keep track of the original packet in the tracking hashmap
        self.message_hash_to_transaction
            .insert(deserialized_packet.message_hash(), deserialized_packet);
    }

    /// Returns the popped minimum packet from the priority queue.
    fn push_pop_min(&mut self, deserialized_packet: DeserializedPacket) -> DeserializedPacket {
        let priority_queue_entry = PacketPriorityQueueEntry::from_packet(&deserialized_packet);

        // Push into the priority queue
        let popped_priority_queue_entry = self
            .packet_priority_queue
            .push_pop_min(priority_queue_entry);

        // Remove the popped entry from the tracking hashmap. Unwrap call is safe
        // because the priority queue and hashmap are kept consistent at all times.
        let popped_packet = self
            .message_hash_to_transaction
            .remove(&popped_priority_queue_entry.message_hash)
            .unwrap();

        // Keep track of the original packet in the tracking hashmap
        self.message_hash_to_transaction
            .insert(deserialized_packet.message_hash(), deserialized_packet);

        popped_packet
    }

    pub fn pop_max(&mut self) -> Option<DeserializedPacket> {
        self.packet_priority_queue
            .pop_max()
            .map(|priority_queue_entry| {
                self.message_hash_to_transaction
                    .remove(&priority_queue_entry.message_hash)
                    .unwrap()
            })
    }

    /// Pop the next `n` highest priority transactions from the queue.
    /// Returns `None` if the queue is empty
    pub fn pop_max_n(&mut self, n: usize) -> Option<Vec<DeserializedPacket>> {
        let current_len = self.len();
        if current_len == 0 {
            None
        } else {
            let num_to_pop = std::cmp::min(current_len, n);
            Some(
                std::iter::repeat_with(|| self.pop_max().unwrap())
                    .take(num_to_pop)
                    .collect(),
            )
        }
    }

    pub fn capacity(&self) -> usize {
        self.packet_priority_queue.capacity()
    }
}

pub fn deserialize_packets<'a>(
    packet_batch: &'a PacketBatch,
    packet_indexes: &'a [usize],
    bank: &'a Option<Arc<Bank>>,
) -> impl Iterator<Item = DeserializedPacket> + 'a {
    packet_indexes.iter().filter_map(|packet_index| {
        DeserializedPacket::new(packet_batch.packets[*packet_index].clone(), bank)
    })
}

/// Read the transaction message from packet data
pub fn packet_message(packet: &Packet) -> Option<&[u8]> {
    let (sig_len, sig_size) = decode_shortu16_len(&packet.data).ok()?;
    let msg_start = sig_len
        .checked_mul(size_of::<Signature>())
        .and_then(|v| v.checked_add(sig_size))?;
    let msg_end = packet.meta.size;
    Some(&packet.data[msg_start..msg_end])
}

/// Computes `(addition_fee + base_fee / requested_cu)` for `deserialized_packet`
fn compute_fee_per_cu(_message: &VersionedMessage, _bank: &Bank) -> u64 {
    1
}

pub fn transactions_to_deserialized_packets(
    transactions: &[Transaction],
) -> Vec<DeserializedPacket> {
    transactions
        .iter()
        .map(|transaction| {
            let packet = Packet::from_data(None, transaction).unwrap();
            DeserializedPacket::new(packet, &None).unwrap()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_sdk::{signature::Keypair, system_transaction},
        std::net::IpAddr,
    };

    fn packet_with_sender_stake(sender_stake: u64, ip: Option<IpAddr>) -> DeserializedPacket {
        let tx = system_transaction::transfer(
            &Keypair::new(),
            &solana_sdk::pubkey::new_rand(),
            1,
            Hash::new_unique(),
        );
        let mut packet = Packet::from_data(None, &tx).unwrap();
        packet.meta.sender_stake = sender_stake;
        if let Some(ip) = ip {
            packet.meta.addr = ip;
        }
        DeserializedPacket::new(packet, &None).unwrap()
    }

    #[test]
    fn test_pop_max_n() {
        let num_packets = 10;
        let packets_iter =
            std::iter::repeat_with(|| packet_with_sender_stake(1, None)).take(num_packets);
        let mut unprocessed_packet_batches =
            UnprocessedPacketBatches::from_iter(packets_iter.clone(), num_packets);

        // Test with small step size
        let step_size = 1;
        for _ in 0..step_size {
            assert_eq!(
                unprocessed_packet_batches
                    .pop_max_n(step_size)
                    .unwrap()
                    .len(),
                step_size
            );
        }

        assert!(unprocessed_packet_batches.is_empty());
        assert!(unprocessed_packet_batches.pop_max_n(0).is_none());
        assert!(unprocessed_packet_batches.pop_max_n(1).is_none());

        // Test with step size larger than `num_packets`
        let step_size = num_packets + 1;
        let mut unprocessed_packet_batches =
            UnprocessedPacketBatches::from_iter(packets_iter.clone(), num_packets);
        assert_eq!(
            unprocessed_packet_batches
                .pop_max_n(step_size)
                .unwrap()
                .len(),
            num_packets
        );
        assert!(unprocessed_packet_batches.is_empty());
        assert!(unprocessed_packet_batches.pop_max_n(0).is_none());

        // Test with step size equal to `num_packets`
        let step_size = num_packets;
        let mut unprocessed_packet_batches =
            UnprocessedPacketBatches::from_iter(packets_iter, num_packets);
        assert_eq!(
            unprocessed_packet_batches
                .pop_max_n(step_size)
                .unwrap()
                .len(),
            step_size
        );
        assert!(unprocessed_packet_batches.is_empty());
        assert!(unprocessed_packet_batches.pop_max_n(0).is_none());
    }
}
