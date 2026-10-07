//! Captive-portal DNS: answers every `A` query with the access point's own address so phones
//! open the setup page automatically.

use alloc::vec::Vec;

use super::read_name;

const TYPE_A: u16 = 1;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;

/// Builds the response for a DNS query datagram, or `None` if it is not a plain query.
pub fn respond(query: &[u8], ip: [u8; 4]) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    let is_query = flags & 0x8000 == 0 && (flags >> 11) & 0xF == 0;
    if !is_query || qdcount == 0 {
        return None;
    }
    let (_, name_end) = read_name(query, 12)?;
    let question_end = name_end + 4;
    let question = query.get(12..question_end)?;
    let qtype = u16::from_be_bytes([query[name_end], query[name_end + 1]]);
    let qclass = u16::from_be_bytes([query[name_end + 2], query[name_end + 3]]);
    let answer = (qtype == TYPE_A || qtype == TYPE_ANY) && qclass & 0x7FFF == CLASS_IN;

    let mut out = Vec::with_capacity(question_end + 16);
    out.extend_from_slice(&query[0..2]); // id
    out.extend_from_slice(&0x8180u16.to_be_bytes()); // QR, RD, RA, NOERROR
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&(answer as u16).to_be_bytes()); // ancount
    out.extend_from_slice(&[0, 0, 0, 0]); // nscount, arcount
    out.extend_from_slice(question);
    if answer {
        out.extend_from_slice(&[0xC0, 0x0C]); // pointer to the question name
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&60u32.to_be_bytes()); // TTL
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&ip);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = alloc::vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        super::super::push_name(&mut q, name);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn answers_a_queries_with_the_ap_address() {
        let r = respond(&query("connectivitycheck.gstatic.com", 1), [192, 168, 4, 1]).unwrap();
        assert_eq!(&r[0..2], [0x12, 0x34]);
        assert_eq!(u16::from_be_bytes([r[2], r[3]]), 0x8180);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 1, "one answer");
        assert_eq!(&r[r.len() - 4..], [192, 168, 4, 1]);
    }

    #[test]
    fn aaaa_gets_an_empty_noerror_answer() {
        let r = respond(&query("example.com", 28), [192, 168, 4, 1]).unwrap();
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 0);
        assert_eq!(u16::from_be_bytes([r[2], r[3]]) & 0xF, 0);
    }

    #[test]
    fn ignores_responses_and_garbage() {
        let mut q = query("x.com", 1);
        q[2] |= 0x80; // QR = response
        assert!(respond(&q, [1, 2, 3, 4]).is_none());
        assert!(respond(&[1, 2, 3], [1, 2, 3, 4]).is_none());
        assert!(respond(&[0; 12], [1, 2, 3, 4]).is_none());
        // truncated question
        let q = query("x.com", 1);
        assert!(respond(&q[..q.len() - 3], [1, 2, 3, 4]).is_none());
    }
}
