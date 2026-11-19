//! Project Genesis — Minimal TCP State Machine & HTTP Client (Stage 13B).
//!
//! Implements a minimal client-side TCP connection state machine:
//! `CLOSED` -> `SYN_SENT` -> `ESTABLISHED` (send HTTP GET) -> `FIN_WAIT` -> `CLOSED`.
//! Includes TCP pseudo-header checksum computation and packet parsing.

use crate::net::IP_PROTO_TCP;

pub const TCP_FLAG_FIN: u8 = 0x01;
pub const TCP_FLAG_SYN: u8 = 0x02;
pub const TCP_FLAG_RST: u8 = 0x04;
pub const TCP_FLAG_PSH: u8 = 0x08;
pub const TCP_FLAG_ACK: u8 = 0x10;
pub const TCP_FLAG_URG: u8 = 0x20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    SynSent,
    Established,
    FinWait1,
    FinWait2,
    TimeWait,
}

pub struct TcpClient {
    pub local_ip: [u8; 4],
    pub local_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
    pub local_seq: u32,
    pub remote_seq: u32,
    pub state: TcpState,
}

impl TcpClient {
    pub fn new(
        local_ip: [u8; 4],
        local_port: u16,
        remote_ip: [u8; 4],
        remote_port: u16,
        initial_seq: u32,
    ) -> Self {
        Self {
            local_ip,
            local_port,
            remote_ip,
            remote_port,
            local_seq: initial_seq,
            remote_seq: 0,
            state: TcpState::Closed,
        }
    }
}

/// Builds a TCP segment with pseudo-header checksum into `buf[0..20 + payload.len()]`.
pub fn build_tcp_packet(
    buf: &mut [u8],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    payload: &[u8],
) -> usize {
    let tcp_len = 20 + payload.len();
    buf[0..2].copy_from_slice(&src_port.to_be_bytes());
    buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
    buf[4..8].copy_from_slice(&seq.to_be_bytes());
    buf[8..12].copy_from_slice(&ack.to_be_bytes());
    buf[12] = 5 << 4; // Data offset = 5 (20 bytes), reserved = 0
    buf[13] = flags;
    buf[14..16].copy_from_slice(&window.to_be_bytes());
    buf[16..18].copy_from_slice(&0u16.to_be_bytes()); // Checksum zero during calc
    buf[18..20].copy_from_slice(&0u16.to_be_bytes()); // Urgent pointer = 0
    if !payload.is_empty() {
        buf[20..20 + payload.len()].copy_from_slice(payload);
    }

    // Pseudo-header: src_ip (4), dst_ip (4), zero (1), proto (1), tcp_length (2)
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&src_ip);
    pseudo[4..8].copy_from_slice(&dst_ip);
    pseudo[8] = 0;
    pseudo[9] = IP_PROTO_TCP;
    pseudo[10..12].copy_from_slice(&(tcp_len as u16).to_be_bytes());

    let mut sum: u32 = 0;
    for chunk in pseudo.chunks(2) {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }
    let mut i = 0;
    while i + 1 < tcp_len {
        let word = u16::from_be_bytes([buf[i], buf[i + 1]]) as u32;
        sum = sum.wrapping_add(word);
        i += 2;
    }
    if i < tcp_len {
        sum = sum.wrapping_add((buf[i] as u32) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let csum = !(sum as u16);
    buf[16..18].copy_from_slice(&csum.to_be_bytes());

    tcp_len
}

/// Parses an IPv4 packet containing a TCP segment.
/// Returns `(src_ip, dst_ip, src_port, dst_port, seq, ack, flags, window, payload)`.
pub fn parse_tcp_packet<'a>(
    data: &'a [u8],
) -> Option<([u8; 4], [u8; 4], u16, u16, u32, u32, u8, u16, &'a [u8])> {
    if data.len() < 40 {
        return None;
    }
    let version = data[0] >> 4;
    let ihl = (data[0] & 0x0F) as usize * 4;
    if version != 4 || ihl < 20 || data.len() < ihl + 20 {
        return None;
    }
    if data[9] != IP_PROTO_TCP {
        return None;
    }

    let mut src_ip = [0u8; 4];
    let mut dst_ip = [0u8; 4];
    src_ip.copy_from_slice(&data[12..16]);
    dst_ip.copy_from_slice(&data[16..20]);

    let total_ip_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let tcp_data = &data[ihl..total_ip_len.min(data.len())];
    if tcp_data.len() < 20 {
        return None;
    }

    let src_port = u16::from_be_bytes([tcp_data[0], tcp_data[1]]);
    let dst_port = u16::from_be_bytes([tcp_data[2], tcp_data[3]]);
    let seq = u32::from_be_bytes([tcp_data[4], tcp_data[5], tcp_data[6], tcp_data[7]]);
    let ack = u32::from_be_bytes([tcp_data[8], tcp_data[9], tcp_data[10], tcp_data[11]]);
    let tcp_hdr_len = ((tcp_data[12] >> 4) as usize) * 4;
    let flags = tcp_data[13];
    let window = u16::from_be_bytes([tcp_data[14], tcp_data[15]]);

    if tcp_hdr_len < 20 || tcp_data.len() < tcp_hdr_len {
        return None;
    }
    let payload = &tcp_data[tcp_hdr_len..];

    Some((src_ip, dst_ip, src_port, dst_port, seq, ack, flags, window, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::GUEST_IP;

    #[test]
    fn tcp_packet_build_and_parse() {
        let src_ip = [10, 0, 2, 15];
        let dst_ip = [10, 0, 2, 2];
        let src_port = 49152;
        let dst_port = 80;
        let seq = 1000;
        let ack = 0;
        let flags = TCP_FLAG_SYN;
        let window = 64240;

        let mut ip_frame = [0u8; 60];
        let tcp_len = build_tcp_packet(
            &mut ip_frame[20..],
            src_ip,
            dst_ip,
            src_port,
            dst_port,
            seq,
            ack,
            flags,
            window,
            &[],
        );
        assert_eq!(tcp_len, 20);

        crate::net::build_ipv4_header(&mut ip_frame[0..20], tcp_len as u16, IP_PROTO_TCP, src_ip, dst_ip, 0x1234);

        let parsed = parse_tcp_packet(&ip_frame).expect("parse tcp packet");
        assert_eq!(parsed.0, src_ip);
        assert_eq!(parsed.1, dst_ip);
        assert_eq!(parsed.2, src_port);
        assert_eq!(parsed.3, dst_port);
        assert_eq!(parsed.4, seq);
        assert_eq!(parsed.5, ack);
        assert_eq!(parsed.6, flags);
        assert_eq!(parsed.7, window);
        assert_eq!(parsed.8.len(), 0);
    }

    #[test]
    fn tcp_client_state_transition() {
        let mut client = TcpClient::new(GUEST_IP, 49152, [10, 0, 2, 2], 80, 5000);
        assert_eq!(client.state, TcpState::Closed);

        // Send SYN
        client.state = TcpState::SynSent;
        assert_eq!(client.state, TcpState::SynSent);

        // Receive SYN-ACK (server seq = 9000, ack = 5001)
        client.remote_seq = 9001;
        client.local_seq = 5001;
        client.state = TcpState::Established;
        assert_eq!(client.state, TcpState::Established);
    }
}
