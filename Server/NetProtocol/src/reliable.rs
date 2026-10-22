//! Reliable delivery for the control messages (spawns, session restarts, object ids, ...) over the
//! unreliable UDP link: a message is wrapped with a sequence number, resent until the other side
//! acknowledges it, and delivered once and in order. Game packets (tunnelled engine traffic) do not
//! use this: they stay unreliable, the engine has its own reliability on top.

use std::time::{Duration, Instant};

/// How long to wait for an acknowledgement before sending a message again.
pub const RESEND_AFTER: Duration = Duration::from_millis(400);
/// A message that is still unacknowledged after this many sends is dropped (the link is dead).
pub const MAX_SENDS: u8 = 20;

struct Pending {
    seq: u16,
    /// The encoded wrapper packet (what goes on the wire).
    bytes: Vec<u8>,
    sent: Instant,
    sends: u8,
}

/// The sending side: numbers messages and keeps them until they are acknowledged.
#[derive(Default)]
pub struct ReliableOut {
    next: u16,
    pending: Vec<Pending>,
}

impl ReliableOut {
    /// Takes the next sequence number; `wrap` builds the wire packet from it. Returns the packet to
    /// send now (it is kept for resending).
    pub fn queue(&mut self, wrap: impl FnOnce(u16) -> Vec<u8>) -> Vec<u8> {
        let seq = self.next;
        self.next = self.next.wrapping_add(1);
        let bytes = wrap(seq);
        self.pending.push(Pending { seq, bytes: bytes.clone(), sent: Instant::now(), sends: 1 });
        bytes
    }

    /// The other side has everything up to and including `seq` (cumulative).
    pub fn ack(&mut self, seq: u16) {
        self.pending.retain(|p| newer(p.seq, seq));
    }

    /// Packets to send again now, and how many messages were given up on.
    pub fn due(&mut self, now: Instant) -> (Vec<Vec<u8>>, usize) {
        let before = self.pending.len();
        self.pending.retain(|p| p.sends < MAX_SENDS || now.duration_since(p.sent) < RESEND_AFTER);
        let dropped = before - self.pending.len();
        let mut resend = Vec::new();
        for p in &mut self.pending {
            if now.duration_since(p.sent) >= RESEND_AFTER {
                p.sent = now;
                p.sends += 1;
                resend.push(p.bytes.clone());
            }
        }
        (resend, dropped)
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

/// `a` is later than `b` in wrapping sequence order.
fn newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// What to do with an incoming reliable message.
#[derive(Debug, PartialEq, Eq)]
pub enum Accept {
    /// The next expected message: process it and acknowledge `ack`.
    Deliver { ack: u16 },
    /// Already seen: only acknowledge `ack` again (the first ack was lost).
    Duplicate { ack: u16 },
    /// A gap (an earlier message is missing): ignore, the sender resends in order.
    Ahead,
}

/// The receiving side: delivers each message once and in order.
#[derive(Default)]
pub struct ReliableIn {
    expected: u16,
}

impl ReliableIn {
    pub fn accept(&mut self, seq: u16) -> Accept {
        if seq == self.expected {
            self.expected = self.expected.wrapping_add(1);
            Accept::Deliver { ack: seq }
        } else if newer(self.expected, seq) {
            Accept::Duplicate { ack: self.expected.wrapping_sub(1) }
        } else {
            Accept::Ahead
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_once_and_in_order() {
        let mut input = ReliableIn::default();
        assert_eq!(input.accept(0), Accept::Deliver { ack: 0 });
        assert_eq!(input.accept(0), Accept::Duplicate { ack: 0 });
        assert_eq!(input.accept(2), Accept::Ahead);
        assert_eq!(input.accept(1), Accept::Deliver { ack: 1 });
        assert_eq!(input.accept(2), Accept::Deliver { ack: 2 });
    }

    #[test]
    fn resends_until_acknowledged() {
        let mut out = ReliableOut::default();
        out.queue(|seq| vec![seq as u8]);
        out.queue(|seq| vec![seq as u8]);
        let later = Instant::now() + RESEND_AFTER * 2;
        let (resend, dropped) = out.due(later);
        assert_eq!((resend.len(), dropped), (2, 0));
        out.ack(0);
        assert_eq!(out.pending(), 1);
        out.ack(1);
        assert_eq!(out.pending(), 0);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut input = ReliableIn { expected: u16::MAX };
        assert_eq!(input.accept(u16::MAX), Accept::Deliver { ack: u16::MAX });
        assert_eq!(input.accept(0), Accept::Deliver { ack: 0 });
    }
}
