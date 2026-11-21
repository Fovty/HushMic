//! Full-frame PCM wire format on a separately negotiated seqpacket socket.
use super::{connect, deadline, error, proxy, Channel, Stamp, FRAME, RESET};
use std::{io, mem::MaybeUninit, os::fd::AsRawFd, slice, time::Instant};

pub const HOP: usize = 480;
const MAGIC: [u8; 4] = *b"DPC1";
const HEADER: usize = 28;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Packet {
    pub stamp: Stamp,
    pub attenuation: f32,
    pub bypass: f32,
}

impl Packet {
    fn encode(self) -> [u8; HEADER] {
        let mut bytes = [0; HEADER];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4..8].copy_from_slice(&self.stamp.kind.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.stamp.epoch.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.stamp.number.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.attenuation.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.bypass.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8; HEADER], words: usize) -> io::Result<Self> {
        if bytes[..4] != MAGIC {
            return Err(error("PCM header magic differs"));
        }
        let packet = Self {
            stamp: Stamp {
                kind: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                epoch: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                number: u64::from_le_bytes(bytes[12..20].try_into().unwrap()),
            },
            attenuation: f32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            bypass: f32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        };
        packet.validate(words)?;
        Ok(packet)
    }

    fn validate(self, words: usize) -> io::Result<()> {
        match self.stamp.kind {
            FRAME
                if words == HOP
                    && (0.0..=1.0).contains(&self.attenuation)
                    && (0.0..=1.0).contains(&self.bypass) =>
            {
                Ok(())
            }
            RESET if words == 0 && self.attenuation == 0.0 && self.bypass == 0.0 => Ok(()),
            _ => Err(error("Invalid PCM packet extent or controls")),
        }
    }
}

fn check_endian() -> io::Result<()> {
    if cfg!(target_endian = "little") {
        Ok(())
    } else {
        Err(io::ErrorKind::Unsupported.into())
    }
}

fn retry(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    )
}

pub fn send(socket: &Channel, packet: Packet, audio: &[f32], until: Instant) -> io::Result<()> {
    send_parts(socket, packet, audio, &[], until)
}

fn send_parts(
    socket: &Channel,
    packet: Packet,
    first: &[f32],
    second: &[f32],
    until: Instant,
) -> io::Result<()> {
    check_endian()?;
    let words = first.len().saturating_add(second.len());
    packet.validate(words)?;
    let header = packet.encode();
    let iov = [
        libc::iovec {
            iov_base: header.as_ptr().cast_mut().cast(),
            iov_len: HEADER,
        },
        libc::iovec {
            iov_base: first.as_ptr().cast_mut().cast(),
            iov_len: size_of_val(first),
        },
        libc::iovec {
            iov_base: second.as_ptr().cast_mut().cast(),
            iov_len: size_of_val(second),
        },
    ];
    // sendmsg reads the iovecs and both byte spans for the duration of the call.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = iov.as_ptr().cast_mut();
    message.msg_iovlen = if !second.is_empty() {
        3
    } else if !first.is_empty() {
        2
    } else {
        1
    };
    let size = (HEADER + words * size_of::<f32>()) as isize;
    loop {
        socket.wait(libc::POLLOUT, until)?;
        let count = unsafe { libc::sendmsg(socket.0.as_raw_fd(), &message, libc::MSG_NOSIGNAL) };
        if count == size {
            return Ok(());
        }
        if count >= 0 {
            return Err(error("Incomplete PCM send"));
        }
        let err = io::Error::last_os_error();
        if !retry(&err) {
            return Err(err);
        }
    }
}

pub fn receive(socket: &Channel, audio: &mut [f32], until: Instant) -> io::Result<Packet> {
    receive_parts(socket, receive_storage(audio), &mut [], until)
}

fn receive_storage(audio: &mut [f32]) -> &mut [MaybeUninit<f32>] {
    // SAFETY: Same layout and exclusive access. Receive only writes bytes and
    // never de-initializes any sample in the originally initialized slice.
    unsafe { slice::from_raw_parts_mut(audio.as_mut_ptr().cast(), audio.len()) }
}

fn receive_parts(
    socket: &Channel,
    first: &mut [MaybeUninit<f32>],
    second: &mut [MaybeUninit<f32>],
    until: Instant,
) -> io::Result<Packet> {
    check_endian()?;
    let mut header = [0u8; HEADER];
    let mut iov = [
        libc::iovec {
            iov_base: header.as_mut_ptr().cast(),
            iov_len: HEADER,
        },
        libc::iovec {
            iov_base: first.as_mut_ptr().cast(),
            iov_len: size_of_val(first),
        },
        libc::iovec {
            iov_base: second.as_mut_ptr().cast(),
            iov_len: size_of_val(second),
        },
    ];
    let count = loop {
        socket.wait(libc::POLLIN, until)?;
        // Header and audio are exclusive, valid writable spans until recvmsg returns.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = iov.as_mut_ptr();
        message.msg_iovlen = if !second.is_empty() {
            3
        } else if !first.is_empty() {
            2
        } else {
            1
        };
        let count = unsafe { libc::recvmsg(socket.0.as_raw_fd(), &mut message, 0) };
        if count > 0 {
            if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
                return Err(error("Truncated PCM packet"));
            }
            break count as usize;
        }
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let err = io::Error::last_os_error();
        if !retry(&err) {
            return Err(err);
        }
    };
    let payload = count
        .checked_sub(HEADER)
        .ok_or_else(|| error("Short PCM header"))?;
    if payload % size_of::<f32>() != 0 {
        return Err(error("Misaligned PCM payload"));
    }
    Packet::decode(&header, payload / size_of::<f32>())
}

pub struct Stream {
    socket: Option<Channel>,
    epoch: u32,
    number: u64,
}

impl Stream {
    pub fn open(model_hash: &str) -> Result<Self, zbus::Error> {
        let bus = connect()?;
        let fd: zbus::zvariant::OwnedFd = proxy(&bus)?.call("OpenPcmSession", &(model_hash,))?;
        Ok(Self {
            socket: Some(Channel(fd.into())),
            epoch: 0,
            number: 0,
        })
    }

    pub fn step(
        &mut self,
        input: &[f32; HOP],
        output: &mut [f32; HOP],
        attenuation: f32,
        bypass: f32,
    ) -> io::Result<()> {
        self.step_parts((input, &[]), (output, &mut []), attenuation, bypass)
    }

    pub fn step_parts(
        &mut self,
        input: (&[f32], &[f32]),
        output: (&mut [f32], &mut [f32]),
        attenuation: f32,
        bypass: f32,
    ) -> io::Result<()> {
        self.step_into(
            input,
            (receive_storage(output.0), receive_storage(output.1)),
            attenuation,
            bypass,
        )
        .map(|_| ())
    }

    /// Receive a complete, validated hop directly into one or two writable spans.
    /// On error, the caller must treat both spans as uninitialized.
    pub fn step_into<'a>(
        &mut self,
        input: (&[f32], &[f32]),
        output: (&'a mut [MaybeUninit<f32>], &'a mut [MaybeUninit<f32>]),
        attenuation: f32,
        bypass: f32,
    ) -> io::Result<(&'a mut [f32], &'a mut [f32])> {
        if output.0.len().saturating_add(output.1.len()) != HOP {
            return Err(error("Invalid PCM output extent"));
        }
        let number = self
            .number
            .checked_add(1)
            .ok_or_else(|| error("PCM frame overflow"))?;
        let request = Packet {
            stamp: Stamp {
                kind: FRAME,
                epoch: self.epoch,
                number: self.number,
            },
            attenuation,
            bypass,
        };
        request.validate(input.0.len().saturating_add(input.1.len()))?;
        self.exchange(
            request,
            input,
            (&mut *output.0, &mut *output.1),
            deadline(20),
        )?;
        self.number = number;
        // SAFETY: A successful exchange requires the exact FRAME header and
        // precisely HOP samples across these two spans, with no truncation.
        // recvmsg wrote every byte. Every f32 bit pattern is valid.
        Ok(unsafe {
            (
                slice::from_raw_parts_mut(output.0.as_mut_ptr().cast(), output.0.len()),
                slice::from_raw_parts_mut(output.1.as_mut_ptr().cast(), output.1.len()),
            )
        })
    }

    pub fn reset(&mut self) -> io::Result<()> {
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| error("PCM epoch overflow"))?;
        let request = Packet {
            stamp: Stamp {
                kind: RESET,
                epoch,
                number: self.number,
            },
            attenuation: 0.0,
            bypass: 0.0,
        };
        self.exchange(request, (&[], &[]), (&mut [], &mut []), deadline(25))?;
        self.epoch = epoch;
        Ok(())
    }

    fn exchange(
        &mut self,
        request: Packet,
        input: (&[f32], &[f32]),
        output: (&mut [MaybeUninit<f32>], &mut [MaybeUninit<f32>]),
        until: Instant,
    ) -> io::Result<()> {
        let socket = self.socket.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        let status = send_parts(socket, request, input.0, input.1, until).and_then(|_| {
            if receive_parts(socket, output.0, output.1, until)? == request {
                Ok(())
            } else {
                Err(error("PCM reply identity differs"))
            }
        });
        if status.is_err() {
            self.socket.take();
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_complete_matching_reply_initializes_both_output_spans() {
        let input = std::array::from_fn::<_, HOP, _>(|sample| sample as f32 / HOP as f32);
        let request = Packet {
            stamp: Stamp {
                kind: FRAME,
                epoch: 3,
                number: 7,
            },
            attenuation: 0.25,
            bypass: 0.5,
        };
        for (reply_epoch, reply_words) in [(3, HOP), (4, HOP), (3, HOP - 1), (3, HOP + 1)] {
            let (client, server) = Channel::pair().unwrap();
            let mut reply = request;
            reply.stamp.epoch = reply_epoch;
            let mut packet = reply.encode().to_vec();
            for sample in input.iter().copied().chain([0.0]).take(reply_words) {
                packet.extend_from_slice(&sample.to_le_bytes());
            }
            // Test peer may deliberately violate the normal sender's extent check.
            let sent = unsafe {
                libc::send(
                    server.0.as_raw_fd(),
                    packet.as_ptr().cast(),
                    packet.len(),
                    0,
                )
            };
            assert_eq!(sent as usize, packet.len());
            let mut stream = Stream {
                socket: Some(client),
                epoch: 3,
                number: 7,
            };
            let mut output = [MaybeUninit::uninit(); HOP];
            let result = stream.step_into(input.split_at(19), output.split_at_mut(137), 0.25, 0.5);
            if (reply_epoch, reply_words) == (3, HOP) {
                let (first, second) = result.unwrap();
                assert_eq!(first, &input[..137]);
                assert_eq!(second, &input[137..]);
                assert_eq!(stream.number, 8);
            } else {
                assert!(result.is_err());
                assert!(stream.socket.is_none());
                assert_eq!(stream.number, 7);
            }
        }
    }
}
