use std::collections::btree_map::{BTreeMap, Entry};

use crate::random;

/// Ports (or ICMP identifiers) in use, keyed by the port number itself.
pub struct PortSet {
    from: u16,
    to: u16,
    ports: BTreeMap<u16, usize>,
}

impl PortSet {
    pub fn new(from: u16, to: u16) -> Option<PortSet> {
        if from > to {
            return None;
        }
        Some(PortSet {
            from,
            to,
            ports: BTreeMap::new(),
        })
    }

    /// Claims a free port in the range, chosen at random: algorithm 1 of RFC 6056,
    /// section 3.3.1, a random start followed by the next free port.
    pub fn get_port(&mut self) -> Option<u16> {
        match random::u32() {
            Ok(start) => self.get_port_from(start),
            Err(err) => {
                error!("No local port: cannot read the random scheme: {err}");
                None
            }
        }
    }

    fn get_port_from(&mut self, start: u32) -> Option<u16> {
        let range = u32::from(self.to) - u32::from(self.from) + 1;
        let mut offset = start % range;
        for _ in 0..range {
            let port = u16::try_from(u32::from(self.from) + offset).ok()?;
            if let Entry::Vacant(entry) = self.ports.entry(port) {
                entry.insert(1);
                return Some(port);
            }
            offset = (offset + 1) % range;
        }
        None
    }

    pub fn claim_port(&mut self, port: u16) -> bool {
        if let Entry::Vacant(entry) = self.ports.entry(port) {
            entry.insert(1);
            true
        } else {
            false
        }
    }

    pub fn claim_reusable(&mut self, port: u16, lingering_count: usize) -> bool {
        match self.ports.entry(port) {
            Entry::Vacant(entry) => {
                entry.insert(1);
                true
            }
            Entry::Occupied(mut entry) if *entry.get() == lingering_count => {
                *entry.get_mut() += 1;
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    pub fn acquire_port(&mut self, port: u16) {
        *self.ports.entry(port).or_insert(0) += 1;
    }

    pub fn release_port(&mut self, port: u16) {
        if let Entry::Occupied(mut entry) = self.ports.entry(port) {
            *entry.get_mut() -= 1;
            if *entry.get() == 0 {
                entry.remove();
            }
        }
    }
}
