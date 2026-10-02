use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::{Ipv4Cidr, Route};

const NLMSG_HDR_LEN: usize = 16;
const RTMSG_LEN: usize = 12;
const RTNH_F_ONLINK: u32 = 4;

pub(crate) struct Netlink {
    fd: OwnedFd,
    seq: u32,
}

impl Netlink {
    pub(crate) fn open() -> io::Result<Self> {
        // SAFETY: plain socket(2) call; the result is checked below.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a freshly created descriptor we own.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self { fd, seq: 0 })
    }

    pub(crate) fn replace(&mut self, route: Route, ifindex: u32, gw: Ipv4Addr) -> io::Result<()> {
        self.seq += 1;
        let msg = route_message(route, ifindex, gw, self.seq);
        self.send(&msg)?;
        self.recv_ack()
    }

    fn send(&self, msg: &[u8]) -> io::Result<()> {
        // SAFETY: zeroed sockaddr_nl is valid (pid 0 = the kernel).
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // SAFETY: `msg` and `addr` are valid for the given lengths.
        let rc = unsafe {
            libc::sendto(
                self.fd.as_raw_fd(),
                msg.as_ptr().cast(),
                msg.len(),
                0,
                std::ptr::addr_of!(addr).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn recv_ack(&self) -> io::Result<()> {
        let mut buf = [0u8; 4096];
        // SAFETY: `buf` is valid for writes of its full length.
        let n = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        parse_ack(&buf[..n as usize])
    }
}

fn route_message(route: Route, ifindex: u32, gw: Ipv4Addr, seq: u32) -> Vec<u8> {
    let (cidr, kind, scope, flags) = match route {
        Route::Unreachable(c) => (c, libc::RTN_UNREACHABLE, libc::RT_SCOPE_NOWHERE, 0),
        Route::ViaGateway(c) => (c, libc::RTN_UNICAST, libc::RT_SCOPE_UNIVERSE, RTNH_F_ONLINK),
    };
    let mut body = rtmsg(cidr, kind, scope, flags);
    if cidr.prefix() > 0 {
        push_attr(&mut body, libc::RTA_DST, &cidr.addr().octets());
    }
    if let Route::ViaGateway(_) = route {
        push_attr(&mut body, libc::RTA_GATEWAY, &gw.octets());
        push_attr(&mut body, libc::RTA_OIF, &ifindex.to_ne_bytes());
    }
    let nl_flags = libc::NLM_F_REQUEST | libc::NLM_F_ACK | libc::NLM_F_CREATE | libc::NLM_F_REPLACE;
    let mut msg = Vec::with_capacity(NLMSG_HDR_LEN + body.len());
    msg.extend(((NLMSG_HDR_LEN + body.len()) as u32).to_ne_bytes());
    msg.extend(libc::RTM_NEWROUTE.to_ne_bytes());
    msg.extend((nl_flags as u16).to_ne_bytes());
    msg.extend(seq.to_ne_bytes());
    msg.extend(0u32.to_ne_bytes());
    msg.extend(body);
    msg
}

fn rtmsg(cidr: Ipv4Cidr, kind: u8, scope: u8, flags: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(RTMSG_LEN);
    out.extend([
        libc::AF_INET as u8,
        cidr.prefix(),
        0, // src_len
        0, // tos
        libc::RT_TABLE_MAIN,
        libc::RTPROT_STATIC,
        scope,
        kind,
    ]);
    out.extend(flags.to_ne_bytes());
    out
}

fn push_attr(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
    let len = 4 + data.len();
    out.extend((len as u16).to_ne_bytes());
    out.extend(kind.to_ne_bytes());
    out.extend(data);
    out.resize(out.len() + (4 - len % 4) % 4, 0);
}

fn parse_ack(reply: &[u8]) -> io::Result<()> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed netlink reply");
    let kind = reply.get(4..6).ok_or_else(bad)?;
    if i32::from(u16::from_ne_bytes([kind[0], kind[1]])) != libc::NLMSG_ERROR {
        return Err(bad());
    }
    let errno = reply.get(16..20).ok_or_else(bad)?;
    match i32::from_ne_bytes([errno[0], errno[1], errno[2], errno[3]]) {
        0 => Ok(()),
        e => Err(io::Error::from_raw_os_error(-e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_unreachable_route() {
        let route = Route::Unreachable("10.0.0.0/8".parse().unwrap());
        let msg = route_message(route, 7, Ipv4Addr::new(192, 0, 2, 1), 1);
        assert_eq!(msg.len(), 16 + 12 + 8);
        assert_eq!(u32::from_ne_bytes(msg[0..4].try_into().unwrap()), 36);
        assert_eq!(&msg[16..24], &[2, 8, 0, 0, 254, 4, 255, 7]);
        assert_eq!(&msg[32..36], &[10, 0, 0, 0]);
    }

    #[test]
    fn default_route_has_gateway_and_no_dst() {
        let route = Route::ViaGateway("0.0.0.0/0".parse().unwrap());
        let msg = route_message(route, 7, Ipv4Addr::new(192, 0, 2, 1), 1);
        assert_eq!(msg.len(), 16 + 12 + 8 + 8);
        assert_eq!(u32::from_ne_bytes(msg[24..28].try_into().unwrap()), 4);
        assert_eq!(&msg[32..36], &[192, 0, 2, 1]);
    }

    #[test]
    fn parses_ack_and_error() {
        let mut ack = vec![0u8; 36];
        ack[4..6].copy_from_slice(&(libc::NLMSG_ERROR as u16).to_ne_bytes());
        assert!(parse_ack(&ack).is_ok());
        ack[16..20].copy_from_slice(&(-libc::EPERM).to_ne_bytes());
        let err = parse_ack(&ack).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EPERM));
        assert!(parse_ack(&[0; 4]).is_err());
    }
}
