///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////
//! A static pool of immutable sealed packets with reference counting.
//!
//! The pool owns the bytes of every packet. Route outboxes and exports hold
//! small [`PacketHandle`]s instead of copies, so the same signed bytes serve
//! several routes. A slot lives while any reference is held (queued, being
//! sent, being exported); a generation in the handle makes a stale handle
//! fail instead of reaching the bytes of a later packet. There is no
//! allocation: a full pool is reported to the caller.
//!
//! The reference rules are explicit: [`alloc`](PacketPool::alloc) returns the
//! first reference; every [`retain`](PacketPool::retain) must be matched by
//! one [`release`](PacketPool::release); the last release frees the slot and
//! invalidates every copy of the handle.

use rohi_telemetry::ProfileStamp;

/// A reference to a packet of a [`PacketPool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketHandle {
    slot: u16,
    generation: u32,
}

struct Slot<const BYTES: usize> {
    generation: u32,
    refs: u16,
    len: usize,
    stamp: Option<ProfileStamp>,
    bytes: [u8; BYTES],
}

/// `SLOTS` packets of up to `BYTES` bytes each.
pub struct PacketPool<const SLOTS: usize, const BYTES: usize> {
    slots: [Slot<BYTES>; SLOTS],
}

impl<const SLOTS: usize, const BYTES: usize> Default for PacketPool<SLOTS, BYTES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SLOTS: usize, const BYTES: usize> PacketPool<SLOTS, BYTES> {
    /// An empty pool.
    pub const fn new() -> Self {
        Self {
            slots: [const {
                Slot {
                    generation: 0,
                    refs: 0,
                    len: 0,
                    stamp: None,
                    bytes: [0; BYTES],
                }
            }; SLOTS],
        }
    }

    /// Takes a free slot for a packet of `stamp` and returns its handle (one
    /// reference) with the buffer to build the packet in. `None` when the
    /// pool is exhausted.
    pub fn alloc(&mut self, stamp: ProfileStamp) -> Option<(PacketHandle, &mut [u8; BYTES])> {
        let index = self.slots.iter().position(|slot| slot.refs == 0)?;
        let slot = &mut self.slots[index];
        slot.refs = 1;
        slot.len = 0;
        slot.stamp = Some(stamp);
        let handle = PacketHandle {
            slot: index as u16,
            generation: slot.generation,
        };
        Some((handle, &mut slot.bytes))
    }

    /// Records the length of the packet built into the buffer of `handle`.
    /// Returns `false` for a stale handle or an impossible length.
    pub fn seal(&mut self, handle: PacketHandle, len: usize) -> bool {
        match self.live_mut(handle) {
            Some(slot) if len <= BYTES => {
                slot.len = len;
                true
            }
            _ => false,
        }
    }

    /// Adds a reference. `false` for a stale handle.
    pub fn retain(&mut self, handle: PacketHandle) -> bool {
        match self.live_mut(handle) {
            Some(slot) => {
                slot.refs = slot.refs.saturating_add(1);
                true
            }
            None => false,
        }
    }

    /// Drops a reference; the last one frees the slot. A stale handle is
    /// ignored (releasing twice cannot free a later packet).
    pub fn release(&mut self, handle: PacketHandle) {
        if let Some(slot) = self.live_mut(handle) {
            slot.refs -= 1;
            if slot.refs == 0 {
                slot.generation = slot.generation.wrapping_add(1);
                slot.stamp = None;
                slot.len = 0;
            }
        }
    }

    /// The stamp and the signed bytes of a live packet.
    pub fn get(&self, handle: PacketHandle) -> Option<(&ProfileStamp, &[u8])> {
        let slot = self.slots.get(usize::from(handle.slot))?;
        if slot.refs == 0 || slot.generation != handle.generation {
            return None;
        }
        Some((slot.stamp.as_ref()?, &slot.bytes[..slot.len]))
    }

    /// Slots in use.
    pub fn in_use(&self) -> usize {
        self.slots.iter().filter(|slot| slot.refs > 0).count()
    }

    fn live_mut(&mut self, handle: PacketHandle) -> Option<&mut Slot<BYTES>> {
        let slot = self.slots.get_mut(usize::from(handle.slot))?;
        (slot.refs > 0 && slot.generation == handle.generation).then_some(slot)
    }
}

#[cfg(test)]
mod tests {
    use rohi_telemetry::PacketProfileId;

    use super::*;

    fn stamp(revision: u32) -> ProfileStamp {
        ProfileStamp {
            id: PacketProfileId::new("p").unwrap(),
            incarnation: 1,
            revision,
        }
    }

    fn packet<const N: usize>(
        pool: &mut PacketPool<N, 8>,
        revision: u32,
        bytes: &[u8],
    ) -> PacketHandle {
        let (handle, buffer) = pool.alloc(stamp(revision)).unwrap();
        buffer[..bytes.len()].copy_from_slice(bytes);
        assert!(pool.seal(handle, bytes.len()));
        handle
    }

    #[test]
    fn several_references_share_one_immutable_packet() {
        let mut pool = PacketPool::<2, 8>::new();
        let handle = packet(&mut pool, 1, b"signed");
        assert!(pool.retain(handle));
        assert_eq!(pool.get(handle).unwrap().1, b"signed");
        pool.release(handle);
        assert_eq!(pool.in_use(), 1, "still referenced");
        assert_eq!(pool.get(handle).unwrap().0.revision, 1);
        pool.release(handle);
        assert_eq!(pool.in_use(), 0);
        assert!(pool.get(handle).is_none());
    }

    #[test]
    fn a_stale_handle_never_reaches_a_later_packet() {
        let mut pool = PacketPool::<1, 8>::new();
        let old = packet(&mut pool, 1, b"old");
        pool.release(old);
        let new = packet(&mut pool, 2, b"new");
        assert_ne!(old, new);
        assert!(pool.get(old).is_none());
        assert!(!pool.retain(old));
        // Releasing the stale handle again must not free the new packet.
        pool.release(old);
        assert_eq!(pool.get(new).unwrap().1, b"new");
        assert_eq!(pool.in_use(), 1);
    }

    #[test]
    fn exhaustion_is_reported_and_a_release_makes_room() {
        let mut pool = PacketPool::<2, 8>::new();
        let a = packet(&mut pool, 1, b"a");
        let _b = packet(&mut pool, 1, b"b");
        assert!(pool.alloc(stamp(1)).is_none());
        pool.release(a);
        assert!(pool.alloc(stamp(1)).is_some());
    }

    #[test]
    fn a_length_beyond_the_buffer_is_refused() {
        let mut pool = PacketPool::<1, 4>::new();
        let (handle, _) = pool.alloc(stamp(1)).unwrap();
        assert!(!pool.seal(handle, 5));
        assert!(pool.seal(handle, 4));
    }
}
