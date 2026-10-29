//! Limits for listeners that anyone on the network can reach: a bounded
//! number of connections still in their handshake, and a short ban for
//! addresses whose handshakes keep failing.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Shutdown, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Failed handshakes from one address before it is banned.
pub const BAN_AFTER: u32 = 5;
/// How long a banned address is refused.
pub const BAN_FOR: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Failures {
    count: u32,
    until: Option<Instant>,
}

/// Failed attempts per source address.
#[derive(Default)]
pub struct Bans {
    map: Mutex<HashMap<IpAddr, Failures>>,
}

impl Bans {
    pub fn banned(&self, ip: IpAddr) -> bool {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(&ip).and_then(|f| f.until) {
            Some(until) if Instant::now() < until => true,
            Some(_) => {
                m.remove(&ip);
                false
            }
            None => false,
        }
    }

    /// Count a failure; returns true if the address is banned now.
    pub fn fail(&self, ip: IpAddr) -> bool {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 4096 {
            // Many spoofed sources: forget the stale ones.
            let now = Instant::now();
            m.retain(|_, f| f.until.is_some_and(|u| u > now));
        }
        let f = m.entry(ip).or_default();
        f.count += 1;
        if f.count >= BAN_AFTER {
            f.count = 0;
            f.until = Some(Instant::now() + BAN_FOR);
            return true;
        }
        false
    }

    pub fn succeed(&self, ip: IpAddr) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(&ip);
    }
}

/// Connections in their handshake. When all slots are taken, the oldest
/// connection is closed to make room: a genuine handshake takes milliseconds,
/// so a flood of idle connections pushes out itself, not the paired Mac.
pub struct Handshakes {
    max: usize,
    next: Mutex<u64>,
    open: Arc<Mutex<VecDeque<(u64, TcpStream)>>>,
}

/// A slot in [`Handshakes`]; released on drop.
pub struct Slot {
    id: u64,
    open: Arc<Mutex<VecDeque<(u64, TcpStream)>>>,
}

impl Handshakes {
    pub fn new(max: usize) -> Handshakes {
        Handshakes { max, next: Mutex::new(0), open: Arc::new(Mutex::new(VecDeque::new())) }
    }

    pub fn admit(&self, stream: &TcpStream) -> std::io::Result<Slot> {
        let clone = stream.try_clone()?;
        let id = {
            let mut n = self.next.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            *n
        };
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        while open.len() >= self.max {
            if let Some((_, oldest)) = open.pop_front() {
                let _ = oldest.shutdown(Shutdown::Both);
            }
        }
        open.push_back((id, clone));
        Ok(Slot { id, open: self.open.clone() })
    }

    pub fn len(&self) -> usize {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn ban_after_repeated_failures() {
        let b = Bans::default();
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        let other: IpAddr = "192.168.1.10".parse().unwrap();
        for _ in 0..BAN_AFTER - 1 {
            assert!(!b.fail(ip));
        }
        assert!(!b.banned(ip));
        assert!(b.fail(ip));
        assert!(b.banned(ip));
        assert!(!b.banned(other));
        b.succeed(ip);
        assert!(!b.banned(ip));
    }

    #[test]
    fn full_slots_close_the_oldest() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = Handshakes::new(2);
        let mut clients = Vec::new();
        let mut slots = Vec::new();
        for _ in 0..3 {
            clients.push(TcpStream::connect(addr).unwrap());
            let (s, _) = l.accept().unwrap();
            slots.push((h.admit(&s).unwrap(), s));
        }
        assert_eq!(h.len(), 2);
        // The first connection was shut down: its client sees EOF.
        let mut buf = [0u8; 1];
        assert_eq!(clients[0].read(&mut buf).unwrap(), 0);
        slots.clear();
        assert!(h.is_empty());
    }
}
