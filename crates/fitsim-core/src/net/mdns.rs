//! Minimal mDNS responder: answers `A` queries for `<hostname>.local` so
//! `http://fitness-simulator.local` resolves on the LAN.

use alloc::string::String;
use alloc::vec::Vec;

use super::{push_name, read_name};

pub const MULTICAST_ADDR: [u8; 4] = [224, 0, 0, 251];
pub const PORT: u16 = 5353;
const TTL: u32 = 120;

#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub payload: Vec<u8>,
    /// The query had the QU bit set: answer the sender directly instead of the multicast group.
    pub unicast: bool,
}

fn answer(hostname: &str, ip: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    out.extend_from_slice(&[0, 0]); // id is 0 in mDNS responses
    out.extend_from_slice(&0x8400u16.to_be_bytes()); // response, authoritative
    out.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0]); // qd=0, an=1, ns=0, ar=0
    let mut fqdn = String::from(hostname);
    fqdn.push_str(".local");
    push_name(&mut out, &fqdn);
    out.extend_from_slice(&1u16.to_be_bytes()); // A
    out.extend_from_slice(&0x8001u16.to_be_bytes()); // IN + cache-flush
    out.extend_from_slice(&TTL.to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&ip);
    out
}

/// Unsolicited announcement sent after getting an address.
pub fn announcement(hostname: &str, ip: [u8; 4]) -> Vec<u8> {
    answer(hostname, ip)
}

/// Builds an answer if `query` asks for `<hostname>.local` (type A or ANY).
pub fn respond(query: &[u8], hostname: &str, ip: [u8; 4]) -> Option<Reply> {
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None; // too short, or a response from someone else
    }
    let qdcount = u16::from_be_bytes([query[4], query[5]]) as usize;
    let mut pos = 12;
    for _ in 0..qdcount.min(8) {
        let (labels, end) = read_name(query, pos)?;
        let qtype = u16::from_be_bytes([*query.get(end)?, *query.get(end + 1)?]);
        let qclass = u16::from_be_bytes([*query.get(end + 2)?, *query.get(end + 3)?]);
        pos = end + 4;
        let matches = labels.len() == 2 && labels[0] == hostname && labels[1] == "local";
        if matches && (qtype == 1 || qtype == 255) && qclass & 0x7FFF == 1 {
            return Some(Reply {
                payload: answer(hostname, ip),
                unicast: qclass & 0x8000 != 0,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16, qclass: u16) -> Vec<u8> {
        let mut q = alloc::vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        push_name(&mut q, name);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&qclass.to_be_bytes());
        q
    }

    #[test]
    fn answers_our_hostname_case_insensitively() {
        let ip = [192, 168, 1, 77];
        let r = respond(
            &query("Fitness-Simulator.local", 1, 1),
            "fitness-simulator",
            ip,
        )
        .unwrap();
        assert!(!r.unicast);
        let p = &r.payload;
        assert_eq!(u16::from_be_bytes([p[2], p[3]]), 0x8400);
        assert_eq!(u16::from_be_bytes([p[6], p[7]]), 1);
        assert_eq!(&p[p.len() - 4..], ip);
        // class has the cache-flush bit set
        let class_off = p.len() - 4 - 2 - 4 - 2;
        assert_eq!(u16::from_be_bytes([p[class_off], p[class_off + 1]]), 0x8001);
    }

    #[test]
    fn honours_the_unicast_bit() {
        let r = respond(
            &query("fitness-simulator.local", 1, 0x8001),
            "fitness-simulator",
            [1, 2, 3, 4],
        )
        .unwrap();
        assert!(r.unicast);
    }

    #[test]
    fn ignores_other_names_types_and_responses() {
        let host = "fitness-simulator";
        assert!(respond(&query("other.local", 1, 1), host, [1, 2, 3, 4]).is_none());
        assert!(
            respond(&query("fitness-simulator.local", 28, 1), host, [1, 2, 3, 4]).is_none(),
            "no AAAA"
        );
        assert!(
            respond(
                &query("fitness-simulator.example", 1, 1),
                host,
                [1, 2, 3, 4]
            )
            .is_none()
        );
        let mut resp = query("fitness-simulator.local", 1, 1);
        resp[2] = 0x84;
        assert!(respond(&resp, host, [1, 2, 3, 4]).is_none());
        assert!(respond(&[1, 2], host, [1, 2, 3, 4]).is_none());
    }

    #[test]
    fn announcement_is_a_valid_response() {
        let a = announcement("fitness-simulator", [10, 0, 0, 5]);
        assert_eq!(u16::from_be_bytes([a[2], a[3]]), 0x8400);
        assert_eq!(&a[a.len() - 4..], [10, 0, 0, 5]);
    }
}
