//! A small DHCP server for the provisioning access point (`192.168.4.0/24`).
//!
//! Hands out addresses `.10`–`.50`, remembers the MAC → address mapping and answers
//! DISCOVER/REQUEST. The reply is always meant to be broadcast to `255.255.255.255:68`.

use alloc::vec::Vec;

const MAGIC: [u8; 4] = [99, 130, 83, 99];
const POOL_START: u8 = 10;
const POOL_SIZE: usize = 41;
const LEASE_SECS: u32 = 3600;

mod msg {
    pub const DISCOVER: u8 = 1;
    pub const OFFER: u8 = 2;
    pub const REQUEST: u8 = 3;
    pub const ACK: u8 = 5;
    pub const NAK: u8 = 6;
    pub const RELEASE: u8 = 7;
}

#[derive(Debug)]
pub struct DhcpServer {
    server_ip: [u8; 4],
    leases: Vec<[u8; 6]>, // index → pool slot
}

impl DhcpServer {
    pub fn new(server_ip: [u8; 4]) -> Self {
        Self {
            server_ip,
            leases: Vec::new(),
        }
    }

    fn lease_for(&mut self, mac: [u8; 6]) -> [u8; 4] {
        let slot = match self.leases.iter().position(|m| *m == mac) {
            Some(i) => i,
            None if self.leases.len() < POOL_SIZE => {
                self.leases.push(mac);
                self.leases.len() - 1
            }
            None => {
                // Pool exhausted: recycle the oldest lease.
                self.leases.remove(0);
                self.leases.push(mac);
                self.leases.len() - 1
            }
        };
        let [a, b, c, _] = self.server_ip;
        [a, b, c, POOL_START + slot as u8]
    }

    /// Processes one DHCP datagram and returns the reply to broadcast, if any.
    pub fn handle(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < 241
            || packet[0] != 1
            || packet[1] != 1
            || packet[2] != 6
            || packet[236..240] != MAGIC
        {
            return None;
        }
        let xid = &packet[4..8];
        let flags = &packet[10..12];
        let ciaddr: [u8; 4] = packet[12..16].try_into().ok()?;
        let mac: [u8; 6] = packet[28..34].try_into().ok()?;

        let mut msg_type = None;
        let mut requested = None;
        let mut server_id = None;
        let mut i = 240;
        while i < packet.len() {
            let code = packet[i];
            if code == 255 {
                break;
            }
            if code == 0 {
                i += 1;
                continue;
            }
            let len = *packet.get(i + 1)? as usize;
            let val = packet.get(i + 2..i + 2 + len)?;
            match (code, len) {
                (53, 1) => msg_type = Some(val[0]),
                (50, 4) => requested = Some([val[0], val[1], val[2], val[3]]),
                (54, 4) => server_id = Some([val[0], val[1], val[2], val[3]]),
                _ => {}
            }
            i += 2 + len;
        }

        // Ignore requests addressed to another DHCP server.
        if server_id.is_some_and(|s| s != self.server_ip) {
            return None;
        }

        let reply_type = match msg_type? {
            msg::DISCOVER => msg::OFFER,
            msg::REQUEST => {
                let ours = self.lease_for(mac);
                let wanted = requested.unwrap_or(ciaddr);
                if wanted == ours { msg::ACK } else { msg::NAK }
            }
            msg::RELEASE => {
                self.leases.retain(|m| *m != mac);
                return None;
            }
            _ => return None,
        };
        let yiaddr = if reply_type == msg::NAK {
            [0; 4]
        } else {
            self.lease_for(mac)
        };

        let mut out = alloc::vec![0u8; 240];
        out[0] = 2; // BOOTREPLY
        out[1] = 1;
        out[2] = 6;
        out[4..8].copy_from_slice(xid);
        out[10..12].copy_from_slice(flags);
        out[16..20].copy_from_slice(&yiaddr);
        out[20..24].copy_from_slice(&self.server_ip); // siaddr
        out[28..34].copy_from_slice(&mac);
        out[236..240].copy_from_slice(&MAGIC);
        let mut opt = |code: u8, data: &[u8]| {
            out.push(code);
            out.push(data.len() as u8);
            out.extend_from_slice(data);
        };
        opt(53, &[reply_type]);
        opt(54, &self.server_ip);
        if reply_type != msg::NAK {
            opt(51, &LEASE_SECS.to_be_bytes());
            opt(58, &(LEASE_SECS / 2).to_be_bytes());
            opt(59, &(LEASE_SECS * 7 / 8).to_be_bytes());
            opt(1, &[255, 255, 255, 0]);
            opt(3, &self.server_ip);
            opt(6, &self.server_ip);
        }
        out.push(255);
        if out.len() < 300 {
            out.resize(300, 0);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AP: [u8; 4] = [192, 168, 4, 1];

    fn client_packet(mac: [u8; 6], msg_type: u8, requested: Option<[u8; 4]>) -> Vec<u8> {
        let mut p = alloc::vec![0u8; 240];
        p[0] = 1;
        p[1] = 1;
        p[2] = 6;
        p[4..8].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        p[28..34].copy_from_slice(&mac);
        p[236..240].copy_from_slice(&MAGIC);
        p.extend_from_slice(&[53, 1, msg_type]);
        if let Some(r) = requested {
            p.extend_from_slice(&[50, 4]);
            p.extend_from_slice(&r);
        }
        p.push(255);
        p
    }

    fn option(reply: &[u8], code: u8) -> Option<Vec<u8>> {
        let mut i = 240;
        while i < reply.len() && reply[i] != 255 {
            let len = reply[i + 1] as usize;
            if reply[i] == code {
                return Some(reply[i + 2..i + 2 + len].to_vec());
            }
            i += 2 + len;
        }
        None
    }

    #[test]
    fn discover_gets_an_offer() {
        let mut s = DhcpServer::new(AP);
        let r = s
            .handle(&client_packet([1, 2, 3, 4, 5, 6], 1, None))
            .unwrap();
        assert_eq!(r[0], 2);
        assert_eq!(&r[4..8], [0xDE, 0xAD, 0xBE, 0xEF], "xid is echoed");
        assert_eq!(&r[16..20], [192, 168, 4, 10]);
        assert_eq!(option(&r, 53), Some(alloc::vec![2]));
        assert_eq!(option(&r, 54), Some(AP.to_vec()));
        assert_eq!(option(&r, 3), Some(AP.to_vec()), "router");
        assert_eq!(
            option(&r, 6),
            Some(AP.to_vec()),
            "DNS points at us for the captive portal"
        );
        assert_eq!(option(&r, 1), Some(alloc::vec![255, 255, 255, 0]));
        assert!(r.len() >= 300);
    }

    #[test]
    fn request_for_the_offered_address_is_acked() {
        let mut s = DhcpServer::new(AP);
        let mac = [1, 2, 3, 4, 5, 6];
        s.handle(&client_packet(mac, 1, None)).unwrap();
        let r = s
            .handle(&client_packet(mac, 3, Some([192, 168, 4, 10])))
            .unwrap();
        assert_eq!(option(&r, 53), Some(alloc::vec![5]));
        assert_eq!(&r[16..20], [192, 168, 4, 10]);
    }

    #[test]
    fn request_for_a_foreign_address_is_nakked() {
        let mut s = DhcpServer::new(AP);
        let r = s
            .handle(&client_packet([1; 6], 3, Some([10, 0, 0, 99])))
            .unwrap();
        assert_eq!(option(&r, 53), Some(alloc::vec![6]));
        assert_eq!(&r[16..20], [0, 0, 0, 0]);
    }

    #[test]
    fn leases_are_stable_and_distinct() {
        let mut s = DhcpServer::new(AP);
        let a = s.handle(&client_packet([1; 6], 1, None)).unwrap();
        let b = s.handle(&client_packet([2; 6], 1, None)).unwrap();
        let a2 = s.handle(&client_packet([1; 6], 1, None)).unwrap();
        assert_ne!(&a[16..20], &b[16..20]);
        assert_eq!(&a[16..20], &a2[16..20]);
    }

    #[test]
    fn pool_wraps_instead_of_failing() {
        let mut s = DhcpServer::new(AP);
        for i in 0..(POOL_SIZE as u8 + 5) {
            let mac = [9, 9, 9, 9, 9, i];
            let r = s.handle(&client_packet(mac, 1, None)).unwrap();
            assert!(r[19] >= POOL_START && (r[19] as usize) < POOL_START as usize + POOL_SIZE);
        }
    }

    #[test]
    fn ignores_other_servers_garbage_and_release() {
        let mut s = DhcpServer::new(AP);
        let mut p = client_packet([1; 6], 3, Some([192, 168, 4, 10]));
        let end = p.len() - 1;
        p.truncate(end);
        p.extend_from_slice(&[54, 4, 10, 0, 0, 1, 255]);
        assert!(s.handle(&p).is_none(), "meant for another server");
        assert!(s.handle(&[0; 20]).is_none());
        assert!(s.handle(&client_packet([1; 6], 7, None)).is_none());
        let mut bad_magic = client_packet([1; 6], 1, None);
        bad_magic[236] = 0;
        assert!(s.handle(&bad_magic).is_none());
    }
}
