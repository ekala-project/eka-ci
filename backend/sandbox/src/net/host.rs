use std::io;
use std::net::Ipv4Addr;

pub(crate) fn ipv4_addrs() -> io::Result<Vec<Ipv4Addr>> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` fills `head` on success; freed below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::new();
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list returned by getifaddrs.
        let ifa = unsafe { &*cur };
        if let Some(addr) = ipv4_of(ifa.ifa_addr).filter(|a| !a.is_loopback()) {
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
        cur = ifa.ifa_next;
    }
    // SAFETY: `head` came from a successful getifaddrs call.
    unsafe { libc::freeifaddrs(head) };
    Ok(out)
}

fn ipv4_of(sa: *const libc::sockaddr) -> Option<Ipv4Addr> {
    // SAFETY: a non-null ifa_addr points to a valid sockaddr whose
    // family says how large it is; AF_INET means sockaddr_in.
    unsafe {
        if sa.is_null() || i32::from((*sa).sa_family) != libc::AF_INET {
            return None;
        }
        let sin = &*sa.cast::<libc::sockaddr_in>();
        Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn skips_loopback() {
        let addrs = super::ipv4_addrs().unwrap();
        assert!(addrs.iter().all(|a| !a.is_loopback()), "{addrs:?}");
    }
}
