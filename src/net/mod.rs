//! Project Genesis — Network Protocol Stack (Ethernet, ARP, IPv4, ICMP, UDP, DNS, TCP).
//!
//! Provides minimal, memory-safe, zero-allocation network protocol encoding,
//! decoding, and checksum verification for VirtIO-Net communication.

pub mod tcp;
pub use tcp::{
    build_tcp_packet, parse_tcp_packet, TcpClient, TcpState, TCP_FLAG_ACK, TCP_FLAG_FIN,
    TCP_FLAG_PSH, TCP_FLAG_RST, TCP_FLAG_SYN, TCP_FLAG_URG,
};

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;

pub const ARP_HTYPE_ETHERNET: u16 = 0x0001;
pub const ARP_PTYPE_IPV4: u16 = 0x0800;
pub const ARP_OP_REQUEST: u16 = 1;
pub const ARP_OP_REPLY: u16 = 2;

pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP: u8 = 6;
pub const IP_PROTO_UDP: u8 = 17;

pub const ICMP_TYPE_ECHO_REPLY: u8 = 0;
pub const ICMP_TYPE_ECHO_REQUEST: u8 = 8;

pub const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
pub const DNS_SERVER_IP: [u8; 4] = [10, 0, 2, 3];
pub const DNS_PORT: u16 = 53;
pub const BROADCAST_MAC: [u8; 6] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveNic {
    None,
    VirtIoNet,
    E1000e,
}

pub static mut ACTIVE_NIC: ActiveNic = ActiveNic::None;

pub fn set_active_nic(nic: ActiveNic) {
    unsafe {
        *core::ptr::addr_of_mut!(ACTIVE_NIC) = nic;
    }
}

pub fn get_active_nic() -> ActiveNic {
    unsafe { *core::ptr::addr_of!(ACTIVE_NIC) }
}

pub fn get_active_mac(virtio_mac: Option<[u8; 6]>) -> Option<[u8; 6]> {
    match get_active_nic() {
        ActiveNic::E1000e => crate::drivers::e1000e::get_global_e1000e_ref().map(|d| d.mac),
        ActiveNic::VirtIoNet => virtio_mac,
        ActiveNic::None => {
            if let Some(d) = crate::drivers::e1000e::get_global_e1000e_ref() {
                Some(d.mac)
            } else {
                virtio_mac
            }
        }
    }
}

/// Computes the standard Internet Checksum (RFC 1071) over arbitrary byte slices.
pub fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        let word = u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        sum = sum.wrapping_add(word);
        i += 2;
    }
    if i < data.len() {
        let word = (data[i] as u32) << 8;
        sum = sum.wrapping_add(word);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Builds an Ethernet II header into `buf[0..14]`.
pub fn build_ethernet_header(
    buf: &mut [u8],
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    ethertype: u16,
) {
    buf[0..6].copy_from_slice(&dst_mac);
    buf[6..12].copy_from_slice(&src_mac);
    buf[12..14].copy_from_slice(&ethertype.to_be_bytes());
}

/// Builds an ARP packet into `buf[0..28]`.
pub fn build_arp_packet(
    buf: &mut [u8],
    opcode: u16,
    sender_mac: [u8; 6],
    sender_ip: [u8; 4],
    target_mac: [u8; 6],
    target_ip: [u8; 4],
) {
    buf[0..2].copy_from_slice(&ARP_HTYPE_ETHERNET.to_be_bytes());
    buf[2..4].copy_from_slice(&ARP_PTYPE_IPV4.to_be_bytes());
    buf[4] = 6; // HW size
    buf[5] = 4; // Proto size
    buf[6..8].copy_from_slice(&opcode.to_be_bytes());
    buf[8..14].copy_from_slice(&sender_mac);
    buf[14..18].copy_from_slice(&sender_ip);
    buf[18..24].copy_from_slice(&target_mac);
    buf[24..28].copy_from_slice(&target_ip);
}

/// Builds an IPv4 header (20 bytes) into `buf[0..20]` with computed checksum.
pub fn build_ipv4_header(
    buf: &mut [u8],
    payload_len: u16,
    protocol: u8,
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    ident: u16,
) {
    let total_len = 20 + payload_len;
    buf[0] = 0x45; // Version 4, IHL 5
    buf[1] = 0x00; // DSCP / ECN
    buf[2..4].copy_from_slice(&total_len.to_be_bytes());
    buf[4..6].copy_from_slice(&ident.to_be_bytes());
    buf[6..8].copy_from_slice(&0u16.to_be_bytes()); // Flags & Fragment offset
    buf[8] = 64;   // TTL
    buf[9] = protocol;
    buf[10..12].copy_from_slice(&0u16.to_be_bytes()); // Checksum zero during calc
    buf[12..16].copy_from_slice(&src_ip);
    buf[16..20].copy_from_slice(&dst_ip);

    let csum = internet_checksum(&buf[0..20]);
    buf[10..12].copy_from_slice(&csum.to_be_bytes());
}

/// Builds an ICMP Echo Request into `buf[0..8 + payload.len()]`.
pub fn build_icmp_echo(
    buf: &mut [u8],
    icmp_type: u8,
    ident: u16,
    seq: u16,
    payload: &[u8],
) -> usize {
    buf[0] = icmp_type;
    buf[1] = 0; // Code = 0
    buf[2..4].copy_from_slice(&0u16.to_be_bytes()); // Checksum zero during calc
    buf[4..6].copy_from_slice(&ident.to_be_bytes());
    buf[6..8].copy_from_slice(&seq.to_be_bytes());
    buf[8..8 + payload.len()].copy_from_slice(payload);

    let total_len = 8 + payload.len();
    let csum = internet_checksum(&buf[0..total_len]);
    buf[2..4].copy_from_slice(&csum.to_be_bytes());
    total_len
}

/// Parses an IPv4 address string like "10.0.2.15" into `[u8; 4]`.
pub fn parse_ipv4_addr(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut octet_idx = 0;
    let mut cur_val: u16 = 0;
    let mut has_digits = false;

    for &b in s.as_bytes() {
        if b >= b'0' && b <= b'9' {
            cur_val = cur_val * 10 + (b - b'0') as u16;
            if cur_val > 255 {
                return None;
            }
            has_digits = true;
        } else if b == b'.' {
            if !has_digits || octet_idx >= 3 {
                return None;
            }
            octets[octet_idx] = cur_val as u8;
            octet_idx += 1;
            cur_val = 0;
            has_digits = false;
        } else {
            return None;
        }
    }

    if octet_idx == 3 && has_digits {
        octets[3] = cur_val as u8;
        Some(octets)
    } else {
        None
    }
}

/// Parses an ARP packet from `data`. Returns `(opcode, sender_mac, sender_ip, target_ip)`.
pub fn parse_arp(data: &[u8]) -> Option<(u16, [u8; 6], [u8; 4], [u8; 4])> {
    if data.len() < 28 {
        return None;
    }
    let htype = u16::from_be_bytes([data[0], data[1]]);
    let ptype = u16::from_be_bytes([data[2], data[3]]);
    let hlen = data[4];
    let plen = data[5];
    let opcode = u16::from_be_bytes([data[6], data[7]]);

    if htype != ARP_HTYPE_ETHERNET || ptype != ARP_PTYPE_IPV4 || hlen != 6 || plen != 4 {
        return None;
    }

    let mut sender_mac = [0u8; 6];
    let mut sender_ip = [0u8; 4];
    let mut target_ip = [0u8; 4];
    sender_mac.copy_from_slice(&data[8..14]);
    sender_ip.copy_from_slice(&data[14..18]);
    target_ip.copy_from_slice(&data[24..28]);

    Some((opcode, sender_mac, sender_ip, target_ip))
}

/// Parses an IPv4 header and ICMP payload.
/// Returns `(src_ip, dst_ip, icmp_type, icmp_code, ident, seq, icmp_payload)`.
pub fn parse_icmp_reply<'a>(
    data: &'a [u8],
) -> Option<([u8; 4], [u8; 4], u8, u8, u16, u16, &'a [u8])> {
    if data.len() < 28 {
        return None;
    }
    let version = data[0] >> 4;
    let ihl = (data[0] & 0x0F) as usize * 4;
    if version != 4 || ihl < 20 || data.len() < ihl + 8 {
        return None;
    }
    let protocol = data[9];
    if protocol != IP_PROTO_ICMP {
        return None;
    }

    let mut src_ip = [0u8; 4];
    let mut dst_ip = [0u8; 4];
    src_ip.copy_from_slice(&data[12..16]);
    dst_ip.copy_from_slice(&data[16..20]);

    let icmp_data = &data[ihl..];
    let icmp_type = icmp_data[0];
    let icmp_code = icmp_data[1];
    let ident = u16::from_be_bytes([icmp_data[4], icmp_data[5]]);
    let seq = u16::from_be_bytes([icmp_data[6], icmp_data[7]]);
    let payload = &icmp_data[8..];

    Some((src_ip, dst_ip, icmp_type, icmp_code, ident, seq, payload))
}

/// Builds a UDP packet with pseudo-header checksum into `buf[0..8 + payload.len()]`.
pub fn build_udp_packet(
    buf: &mut [u8],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> usize {
    let udp_len = 8 + payload.len();
    buf[0..2].copy_from_slice(&src_port.to_be_bytes());
    buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
    buf[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    buf[6..8].copy_from_slice(&0u16.to_be_bytes()); // Checksum zero during calc
    buf[8..8 + payload.len()].copy_from_slice(payload);

    // Pseudo-header: src_ip (4), dst_ip (4), zero (1), proto (1), udp_length (2)
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&src_ip);
    pseudo[4..8].copy_from_slice(&dst_ip);
    pseudo[8] = 0;
    pseudo[9] = IP_PROTO_UDP;
    pseudo[10..12].copy_from_slice(&(udp_len as u16).to_be_bytes());

    let mut sum: u32 = 0;
    for chunk in pseudo.chunks(2) {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }
    let mut i = 0;
    while i + 1 < udp_len {
        let word = u16::from_be_bytes([buf[i], buf[i + 1]]) as u32;
        sum = sum.wrapping_add(word);
        i += 2;
    }
    if i < udp_len {
        sum = sum.wrapping_add((buf[i] as u32) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let mut csum = !(sum as u16);
    if csum == 0 {
        csum = 0xFFFF;
    }
    buf[6..8].copy_from_slice(&csum.to_be_bytes());

    udp_len
}

/// Parses an IPv4 packet containing a UDP datagram.
/// Returns `(src_ip, dst_ip, src_port, dst_port, payload)`.
pub fn parse_udp_packet<'a>(
    data: &'a [u8],
) -> Option<([u8; 4], [u8; 4], u16, u16, &'a [u8])> {
    if data.len() < 28 {
        return None;
    }
    let version = data[0] >> 4;
    let ihl = (data[0] & 0x0F) as usize * 4;
    if version != 4 || ihl < 20 || data.len() < ihl + 8 {
        return None;
    }
    if data[9] != IP_PROTO_UDP {
        return None;
    }

    let mut src_ip = [0u8; 4];
    let mut dst_ip = [0u8; 4];
    src_ip.copy_from_slice(&data[12..16]);
    dst_ip.copy_from_slice(&data[16..20]);

    let udp_data = &data[ihl..];
    let src_port = u16::from_be_bytes([udp_data[0], udp_data[1]]);
    let dst_port = u16::from_be_bytes([udp_data[2], udp_data[3]]);
    let udp_len = u16::from_be_bytes([udp_data[4], udp_data[5]]) as usize;
    if udp_len < 8 || udp_data.len() < udp_len {
        return None;
    }

    let payload = &udp_data[8..udp_len];
    Some((src_ip, dst_ip, src_port, dst_port, payload))
}

/// Builds a standard DNS A-record query for `hostname` into `buf`.
pub fn build_dns_query(buf: &mut [u8], id: u16, hostname: &str) -> usize {
    buf[0..2].copy_from_slice(&id.to_be_bytes());
    buf[2..4].copy_from_slice(&0x0100u16.to_be_bytes()); // Standard query with RD=1
    buf[4..6].copy_from_slice(&1u16.to_be_bytes());      // 1 question
    buf[6..8].copy_from_slice(&0u16.to_be_bytes());      // 0 answers
    buf[8..10].copy_from_slice(&0u16.to_be_bytes());     // 0 NS
    buf[10..12].copy_from_slice(&0u16.to_be_bytes());    // 0 Additional

    let mut offset = 12;
    for label in hostname.split('.') {
        let label_bytes = label.as_bytes();
        if label_bytes.is_empty() {
            continue;
        }
        buf[offset] = label_bytes.len() as u8;
        offset += 1;
        buf[offset..offset + label_bytes.len()].copy_from_slice(label_bytes);
        offset += label_bytes.len();
    }
    buf[offset] = 0; // End of QNAME
    offset += 1;

    buf[offset..offset + 2].copy_from_slice(&1u16.to_be_bytes()); // QTYPE = 1 (A)
    offset += 2;
    buf[offset..offset + 2].copy_from_slice(&1u16.to_be_bytes()); // QCLASS = 1 (IN)
    offset += 2;

    offset
}

/// Parses a DNS response payload and extracts the first IPv4 A-record address.
pub fn parse_dns_response(data: &[u8], expected_id: u16) -> Option<[u8; 4]> {
    if data.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([data[0], data[1]]);
    let flags = u16::from_be_bytes([data[2], data[3]]);
    let qdcount = u16::from_be_bytes([data[4], data[5]]);
    let ancount = u16::from_be_bytes([data[6], data[7]]);

    if id != expected_id || (flags & 0x8000) == 0 || (flags & 0x000F) != 0 || ancount == 0 {
        return None;
    }

    let mut offset = 12;
    // Skip questions
    for _ in 0..qdcount {
        while offset < data.len() {
            let len = data[offset] as usize;
            if len == 0 {
                offset += 1;
                break;
            }
            if (len & 0xC0) == 0xC0 {
                offset += 2;
                break;
            }
            offset += 1 + len;
        }
        offset += 4; // QTYPE (2) + QCLASS (2)
    }

    // Parse answers
    for _ in 0..ancount {
        if offset >= data.len() {
            return None;
        }
        // Skip NAME (pointer or label sequence)
        if (data[offset] & 0xC0) == 0xC0 {
            offset += 2;
        } else {
            while offset < data.len() {
                let len = data[offset] as usize;
                if len == 0 {
                    offset += 1;
                    break;
                }
                if (len & 0xC0) == 0xC0 {
                    offset += 2;
                    break;
                }
                offset += 1 + len;
            }
        }

        if offset + 10 > data.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let rdlength = u16::from_be_bytes([data[offset + 8], data[offset + 9]]) as usize;
        offset += 10;

        if offset + rdlength > data.len() {
            return None;
        }

        if rtype == 1 && rdlength == 4 {
            let mut ip = [0u8; 4];
            ip.copy_from_slice(&data[offset..offset + 4]);
            return Some(ip);
        }
        offset += rdlength;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipv4_addr_test() {
        assert_eq!(parse_ipv4_addr("10.0.2.15"), Some([10, 0, 2, 15]));
        assert_eq!(parse_ipv4_addr("127.0.0.1"), Some([127, 0, 0, 1]));
        assert_eq!(parse_ipv4_addr("255.255.255.255"), Some([255, 255, 255, 255]));
        assert_eq!(parse_ipv4_addr("256.0.0.1"), None);
        assert_eq!(parse_ipv4_addr("10.0.2"), None);
        assert_eq!(parse_ipv4_addr("10.0.2.15.1"), None);
        assert_eq!(parse_ipv4_addr("google.com"), None);
    }

    #[test]
    fn internet_checksum_calculation() {
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        let csum = internet_checksum(&data);
        assert_eq!(csum, 0x220d);

        let mut full = [0u8; 10];
        full[..8].copy_from_slice(&data);
        full[8..10].copy_from_slice(&csum.to_be_bytes());
        assert_eq!(internet_checksum(&full), 0x0000);
    }

    #[test]
    fn arp_packet_build_and_parse() {
        let sender_mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let sender_ip = [10, 0, 2, 15];
        let target_mac = [0x00; 6];
        let target_ip = [10, 0, 2, 2];

        let mut buf = [0u8; 28];
        build_arp_packet(&mut buf, ARP_OP_REQUEST, sender_mac, sender_ip, target_mac, target_ip);

        let parsed = parse_arp(&buf).expect("parse arp request");
        assert_eq!(parsed.0, ARP_OP_REQUEST);
        assert_eq!(parsed.1, sender_mac);
        assert_eq!(parsed.2, sender_ip);
        assert_eq!(parsed.3, target_ip);
    }

    #[test]
    fn ipv4_and_icmp_echo_request_and_reply() {
        let mut eth_frame = [0u8; 128];
        let my_mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let gw_mac = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];

        build_ethernet_header(&mut eth_frame[0..14], gw_mac, my_mac, ETHERTYPE_IPV4);

        let payload = b"Hello ICMP ping";
        let icmp_len = build_icmp_echo(&mut eth_frame[34..], ICMP_TYPE_ECHO_REQUEST, 0x1234, 1, payload);
        assert_eq!(icmp_len, 8 + payload.len());

        build_ipv4_header(&mut eth_frame[14..34], icmp_len as u16, IP_PROTO_ICMP, GUEST_IP, GATEWAY_IP, 0xABCD);
        assert_eq!(internet_checksum(&eth_frame[14..34]), 0x0000);

        eth_frame[34] = ICMP_TYPE_ECHO_REPLY;
        eth_frame[36..38].copy_from_slice(&0u16.to_be_bytes());
        let new_csum = internet_checksum(&eth_frame[34..34 + icmp_len]);
        eth_frame[36..38].copy_from_slice(&new_csum.to_be_bytes());

        let (src, dst, icmp_type, code, ident, seq, reply_payload) =
            parse_icmp_reply(&eth_frame[14..34 + icmp_len]).expect("parse icmp reply");
        assert_eq!(src, GUEST_IP);
        assert_eq!(dst, GATEWAY_IP);
        assert_eq!(icmp_type, ICMP_TYPE_ECHO_REPLY);
        assert_eq!(code, 0);
        assert_eq!(ident, 0x1234);
        assert_eq!(seq, 1);
        assert_eq!(reply_payload, payload);
    }

    #[test]
    fn udp_packet_build_and_parse() {
        let mut ip_frame = [0u8; 64];
        let payload = b"UDP Genesis Test";
        let udp_len = build_udp_packet(
            &mut ip_frame[20..],
            GUEST_IP,
            DNS_SERVER_IP,
            50000,
            DNS_PORT,
            payload,
        );
        assert_eq!(udp_len, 8 + payload.len());

        build_ipv4_header(&mut ip_frame[0..20], udp_len as u16, IP_PROTO_UDP, GUEST_IP, DNS_SERVER_IP, 0x5678);

        let (src, dst, src_p, dst_p, parsed_payload) =
            parse_udp_packet(&ip_frame).expect("parse udp");
        assert_eq!(src, GUEST_IP);
        assert_eq!(dst, DNS_SERVER_IP);
        assert_eq!(src_p, 50000);
        assert_eq!(dst_p, DNS_PORT);
        assert_eq!(parsed_payload, payload);
    }

    #[test]
    fn dns_query_and_response_parsing() {
        let mut query_buf = [0u8; 128];
        let query_len = build_dns_query(&mut query_buf, 0x4321, "genesis.os");
        assert!(query_len > 12);
        assert_eq!(&query_buf[0..2], &0x4321u16.to_be_bytes());

        // Construct a mock DNS response with 1 question and 1 answer
        let mut resp = [0u8; 64];
        resp[0..query_len].copy_from_slice(&query_buf[0..query_len]);
        // Set QR=1 (response), AA=1, RD=1, RA=1 -> 0x8180
        resp[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
        resp[6..8].copy_from_slice(&1u16.to_be_bytes()); // ANCOUNT = 1

        let mut ans_offset = query_len;
        // Answer name: pointer to offset 12 (0xC00C)
        resp[ans_offset..ans_offset + 2].copy_from_slice(&[0xC0, 0x0C]);
        ans_offset += 2;
        // TYPE A (1), CLASS IN (1)
        resp[ans_offset..ans_offset + 2].copy_from_slice(&1u16.to_be_bytes());
        ans_offset += 2;
        resp[ans_offset..ans_offset + 2].copy_from_slice(&1u16.to_be_bytes());
        ans_offset += 2;
        // TTL (60s)
        resp[ans_offset..ans_offset + 4].copy_from_slice(&60u32.to_be_bytes());
        ans_offset += 4;
        // RDLENGTH (4 bytes)
        resp[ans_offset..ans_offset + 2].copy_from_slice(&4u16.to_be_bytes());
        ans_offset += 2;
        // RDATA (10.0.2.100)
        let resolved_ip = [10, 0, 2, 100];
        resp[ans_offset..ans_offset + 4].copy_from_slice(&resolved_ip);
        ans_offset += 4;

        let parsed_ip = parse_dns_response(&resp[0..ans_offset], 0x4321).expect("parse dns response");
        assert_eq!(parsed_ip, resolved_ip);
    }
}
