//! Local transport shared by the host service and the optional HushMic client.
pub mod pcm;

use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    time::{Duration, Instant},
};

pub const SERVICE: &str = "io.github.fovty.HushMic.XDNA2";
pub const OBJECT: &str = "/io/github/fovty/HushMic/XDNA2";
pub const INTERFACE: &str = "io.github.fovty.HushMic.XDNA2.V1";
pub const WORDS: usize = 962;
pub const FRAME: u32 = 1;
pub const RESET: u32 = 2;
pub const READY: u32 = 3;
pub const CPU: u8 = 0;
pub const AUTO: u8 = 1;
pub const PREFER_NPU: u8 = 2;

const HEADER: usize = 20;
const MAGIC: [u8; 4] = *b"DPX1";
const MAX_PACKET: usize = HEADER + WORDS * 4;

pub fn file_hash(path: &Path) -> io::Result<String> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0; 32768];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub kind: u32,
    pub epoch: u32,
    pub number: u64,
}

pub fn deadline(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}

pub struct Channel(pub OwnedFd);

fn error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl Channel {
    pub fn pair() -> io::Result<(Self, Self)> {
        let mut descriptors = [-1; 2];
        // socketpair initializes both descriptors only on success.
        let status = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
                descriptors.as_mut_ptr(),
            )
        };
        if status < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe {
            (
                Self(OwnedFd::from_raw_fd(descriptors[0])),
                Self(OwnedFd::from_raw_fd(descriptors[1])),
            )
        })
    }

    fn wait(&self, events: i16, until: Instant) -> io::Result<()> {
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let millis = left.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
            let mut pollfd = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pollfd, 1, millis) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if n == 0 || Instant::now() >= until {
                return Err(io::ErrorKind::TimedOut.into());
            }
            if pollfd.revents & events != 0 {
                return Ok(());
            }
            return Err(io::ErrorKind::BrokenPipe.into());
        }
    }

    pub fn send(&self, stamp: Stamp, data: &[f32], until: Instant) -> io::Result<()> {
        let mut packet = [0; MAX_PACKET];
        let words = if stamp.kind == FRAME { WORDS } else { 0 };
        if data.len() != words || data.iter().any(|v| !v.is_finite()) {
            return Err(error("invalid spectral frame"));
        }
        packet[..4].copy_from_slice(&MAGIC);
        packet[4..8].copy_from_slice(&stamp.kind.to_le_bytes());
        packet[8..12].copy_from_slice(&stamp.epoch.to_le_bytes());
        packet[12..HEADER].copy_from_slice(&stamp.number.to_le_bytes());
        for (dst, sample) in packet[HEADER..].chunks_exact_mut(4).zip(data) {
            dst.copy_from_slice(&sample.to_le_bytes());
        }
        let len = HEADER + words * 4;
        loop {
            self.wait(libc::POLLOUT, until)?;
            let n = unsafe {
                libc::send(
                    self.0.as_raw_fd(),
                    packet.as_ptr().cast(),
                    len,
                    libc::MSG_NOSIGNAL,
                )
            };
            if n == len as isize {
                return Ok(());
            }
            if n >= 0 {
                return Err(error("short spectral packet write"));
            }
            let err = io::Error::last_os_error();
            if !matches!(
                err.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                return Err(err);
            }
        }
    }

    pub fn recv(&self, data: &mut [f32; WORDS], until: Instant) -> io::Result<Stamp> {
        let mut packet = [0; MAX_PACKET];
        let len = loop {
            self.wait(libc::POLLIN, until)?;
            let n = unsafe {
                libc::recv(
                    self.0.as_raw_fd(),
                    packet.as_mut_ptr().cast(),
                    packet.len(),
                    libc::MSG_TRUNC,
                )
            };
            if n > 0 {
                break n as usize;
            }
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let err = io::Error::last_os_error();
            if !matches!(
                err.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                return Err(err);
            }
        };
        if len < HEADER || len > MAX_PACKET || packet[..4] != MAGIC {
            return Err(error("invalid spectral packet header"));
        }
        let kind = u32::from_le_bytes(packet[4..8].try_into().unwrap());
        let epoch = u32::from_le_bytes(packet[8..12].try_into().unwrap());
        let number = u64::from_le_bytes(packet[12..HEADER].try_into().unwrap());
        let wanted = match kind {
            FRAME => MAX_PACKET,
            RESET | READY => HEADER,
            _ => return Err(error("unknown spectral packet")),
        };
        if len != wanted {
            return Err(error("invalid spectral packet length"));
        }
        if kind == FRAME {
            for (dst, src) in data.iter_mut().zip(packet[HEADER..].chunks_exact(4)) {
                *dst = f32::from_le_bytes(src.try_into().unwrap());
                if !dst.is_finite() {
                    return Err(error("nonfinite spectral value"));
                }
            }
        }
        Ok(Stamp {
            kind,
            epoch,
            number,
        })
    }
}

fn proxy(conn: &zbus::blocking::Connection) -> zbus::Result<zbus::blocking::Proxy<'_>> {
    zbus::blocking::Proxy::new(conn, SERVICE, OBJECT, INTERFACE)
}

fn connect() -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_millis(1500))
        .build()
}

pub fn policy() -> zbus::Result<(u8, bool, bool)> {
    proxy(&connect()?)?.call("GetPolicy", &())
}

pub fn set_policy(value: u8) -> zbus::Result<()> {
    proxy(&connect()?)?.call("SetPolicy", &(value,))
}

pub fn session_busy(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name, _, _)
        if name.as_str() == "org.freedesktop.DBus.Error.LimitsExceeded")
}

pub struct Stream {
    socket: Channel,
    epoch: u32,
    number: u64,
}

impl Stream {
    pub fn open(model_hash: &str) -> Result<Self, zbus::Error> {
        let bus = connect()?;
        let fd: zbus::zvariant::OwnedFd = proxy(&bus)?.call("OpenSession", &(model_hash,))?;
        Ok(Self {
            socket: Channel(fd.into()),
            epoch: 0,
            number: 0,
        })
    }

    pub fn step(&mut self, input: &[f32; WORDS], output: &mut [f32; WORDS]) -> io::Result<()> {
        let stamp = Stamp {
            kind: FRAME,
            epoch: self.epoch,
            number: self.number,
        };
        let until = deadline(20);
        self.socket.send(stamp, input, until)?;
        if self.socket.recv(output, until)? != stamp {
            return Err(error("stale spectral frame"));
        }
        self.number = self
            .number
            .checked_add(1)
            .ok_or_else(|| error("frame number overflow"))?;
        Ok(())
    }

    pub fn reset(&mut self) -> io::Result<()> {
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| error("stream epoch overflow"))?;
        let stamp = Stamp {
            kind: RESET,
            epoch,
            number: self.number,
        };
        let until = deadline(25);
        self.socket.send(stamp, &[], until)?;
        if self.socket.recv(&mut [0.0; WORDS], until)? != stamp {
            return Err(error("stale reset reply"));
        }
        self.epoch = epoch;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_and_timeout() {
        let (a, b) = Channel::pair().unwrap();
        let stamp = Stamp {
            kind: FRAME,
            epoch: 7,
            number: 512,
        };
        let input = [0.125; WORDS];
        let mut output = [0.0; WORDS];
        a.send(stamp, &input, deadline(100)).unwrap();
        assert_eq!(b.recv(&mut output, deadline(100)).unwrap(), stamp);
        assert_eq!(input, output);
        assert!(a.send(stamp, &[f32::NAN; WORDS], deadline(100)).is_err());
        assert_eq!(
            b.recv(&mut output, deadline(2)).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
