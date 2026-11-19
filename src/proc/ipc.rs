//! Project Genesis — Kernel-Mediated Inter-Process Communication (IPC).
//!
//! Provides mailbox message queuing between EL0 user-space processes:
//! - `SYS_IPC_SEND (200)`: Target PID, buffer pointer, length
//! - `SYS_IPC_RECV (201)`: Sender PID filter (0 for any), buffer pointer, max capacity

use crate::mm::heap::SpinLock;

pub const MAX_IPC_MSG_LEN: usize = 64;
pub const MAX_IPC_QUEUE: usize = 8;

pub const SYS_IPC_SEND: u64 = 200;
pub const SYS_IPC_RECV: u64 = 201;

#[derive(Clone, Copy)]
pub struct IpcMessage {
    pub sender_pid: u64,
    pub target_pid: u64,
    pub len: usize,
    pub payload: [u8; MAX_IPC_MSG_LEN],
}

impl IpcMessage {
    pub const fn empty() -> Self {
        Self {
            sender_pid: 0,
            target_pid: 0,
            len: 0,
            payload: [0; MAX_IPC_MSG_LEN],
        }
    }
}

pub struct IpcMailbox {
    messages: [IpcMessage; MAX_IPC_QUEUE],
    count: usize,
    pub verified_ping: bool,
}

impl IpcMailbox {
    pub const fn new() -> Self {
        Self {
            messages: [IpcMessage::empty(); MAX_IPC_QUEUE],
            count: 0,
            verified_ping: false,
        }
    }

    pub fn send(&mut self, sender_pid: u64, target_pid: u64, data: &[u8]) -> Result<(), i64> {
        if data.len() > MAX_IPC_MSG_LEN {
            return Err(-22); // -EINVAL
        }
        if self.count >= MAX_IPC_QUEUE {
            return Err(-11); // -EAGAIN / queue full
        }
        if data.starts_with(b"PING_FROM_PID1") {
            self.verified_ping = true;
        }
        let mut msg = IpcMessage::empty();
        msg.sender_pid = sender_pid;
        msg.target_pid = target_pid;
        msg.len = data.len();
        msg.payload[..data.len()].copy_from_slice(data);
        self.messages[self.count] = msg;
        self.count += 1;
        Ok(())
    }

    pub fn recv(
        &mut self,
        receiver_pid: u64,
        sender_filter: u64,
        buf: &mut [u8],
    ) -> Result<(u64, usize), i64> {
        for i in 0..self.count {
            let msg = &self.messages[i];
            if msg.target_pid == receiver_pid && (sender_filter == 0 || msg.sender_pid == sender_filter) {
                let copy_len = msg.len.min(buf.len());
                buf[..copy_len].copy_from_slice(&msg.payload[..copy_len]);
                let sender_pid = msg.sender_pid;
                // Shift remaining messages down
                for j in i..(self.count - 1) {
                    self.messages[j] = self.messages[j + 1];
                }
                self.count -= 1;
                return Ok((sender_pid, copy_len));
            }
        }
        Err(-11) // -EAGAIN / no message available
    }

    pub fn reset(&mut self) {
        self.count = 0;
        self.verified_ping = false;
    }
}

pub static IPC_MAILBOX: SpinLock<IpcMailbox> = SpinLock::new(IpcMailbox::new());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_send_recv_and_filter() {
        let mut mb = IpcMailbox::new();
        assert_eq!(mb.send(1, 2, b"HELLO PID2"), Ok(()));
        assert_eq!(mb.send(3, 2, b"HELLO FROM 3"), Ok(()));

        let mut buf = [0u8; 32];
        // Recv filtering for sender 1
        let (sender, len) = mb.recv(2, 1, &mut buf).expect("recv from 1");
        assert_eq!(sender, 1);
        assert_eq!(&buf[..len], b"HELLO PID2");

        // Recv remaining message from sender 3
        let (sender, len) = mb.recv(2, 0, &mut buf).expect("recv any");
        assert_eq!(sender, 3);
        assert_eq!(&buf[..len], b"HELLO FROM 3");

        // Now queue empty
        assert_eq!(mb.recv(2, 0, &mut buf), Err(-11));
    }

    #[test]
    fn ipc_verified_ping_flag() {
        let mut mb = IpcMailbox::new();
        assert!(!mb.verified_ping);
        assert_eq!(mb.send(1, 2, b"PING_FROM_PID1\n"), Ok(()));
        assert!(mb.verified_ping);
    }
}
