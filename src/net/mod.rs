//! Project Genesis — Network Protocol Stack (Ethernet, ARP, IPv4, ICMP).
//!
//! Provides minimal, memory-safe, zero-allocation network protocol encoding,
//! decoding, and checksum verification for Stage 12B VirtIO-Net communication.

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;

pub const ARP_HTYPE_ETHERNET: u16 = 0x0001;
pub const ARP_PTYPE_IPV4: u16 = 0x0800;
pub const ARP_OP_REQUEST: u16 = 1;
pub const ARP_OP_REPLY: u16 = 2;

pub const IP_PROTO_ICMP: u8 = 1;
pub const ICMP_TYPE_ECHO_REPLY: u8 = 0;
pub const ICMP_TYPE_ECHO_REQUEST: u8 = 8;

pub const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
pub const BROADCAST_MAC: [u8; 6] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

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
        // 20 bytes IPv4 + 8 bytes ICMP
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internet_checksum_calculation() {
        // Standard RFC 1071 test vector
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        let csum = internet_checksum(&data);
        assert_eq!(csum, 0x220d);

        // Verification: checksum of data with checksum appended is 0
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

        // Verify IPv4 header checksum
        assert_eq!(internet_checksum(&eth_frame[14..34]), 0x0000);

        // Simulate reply by changing type to ECHO_REPLY and recomputing ICMP checksum
        eth_frame[34] = ICMP_TYPE_ECHO_REPLY;
        eth_frame[36..38].copy_from_slice(&0u16.to_be_bytes());
        let new_csum = internet_checksum(&eth_frame[34..34 + icmp_len]);
        eth_frame[36..38].copy_from_slice(&new_csum.to_be_bytes());

        // Parse reply
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
}
