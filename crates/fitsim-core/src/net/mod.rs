//! Tiny packet-level implementations of the network services the device needs:
//! a DHCP server and captive-portal DNS for the provisioning access point, and an mDNS responder
//! for `<hostname>.local`. They work on raw datagrams so the firmware only moves bytes between
//! sockets and these functions.

pub mod dhcp;
pub mod dns;
pub mod mdns;

/// Address of the provisioning access point (also its DHCP/DNS server).
pub const AP_IP: [u8; 4] = [192, 168, 4, 1];
pub const AP_PREFIX_LEN: u8 = 24;

/// Appends a DNS name (`a.b.local`) in wire format.
pub(crate) fn push_name(out: &mut alloc::vec::Vec<u8>, name: &str) {
    for label in name.split('.').filter(|l| !l.is_empty()) {
        out.push(label.len().min(63) as u8);
        out.extend_from_slice(&label.as_bytes()[..label.len().min(63)]);
    }
    out.push(0);
}

/// Reads a (possibly compressed) DNS name at `pos` into lowercase labels. Returns the labels and
/// the offset just after the name in the *original* position.
pub(crate) fn read_name(
    msg: &[u8],
    mut pos: usize,
) -> Option<(alloc::vec::Vec<alloc::string::String>, usize)> {
    let mut labels = alloc::vec::Vec::new();
    let mut end: Option<usize> = None;
    let mut hops = 0;
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            let lo = *msg.get(pos + 1)? as usize;
            end.get_or_insert(pos + 2);
            pos = ((len & 0x3F) << 8) | lo;
            hops += 1;
            if hops > 8 {
                return None;
            }
            continue;
        }
        if len > 63 {
            return None;
        }
        let label = msg.get(pos + 1..pos + 1 + len)?;
        labels.push(alloc::string::String::from_utf8_lossy(label).to_ascii_lowercase());
        pos += 1 + len;
    }
    Some((labels, end.unwrap_or(pos)))
}
