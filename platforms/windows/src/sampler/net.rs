//! Network adapters: the list from `GetIfTable2`, counters per adapter from `GetIfEntry2`,
//! addresses from `GetAdaptersAddresses`.

use std::net::{Ipv4Addr, Ipv6Addr};

use tm_core::model::{NetInfo, RawNet};
use windows::Win32::NetworkManagement::IpHelper::*;
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

use crate::util::{from_pwstr, from_wide};

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_PPP: u32 = 23;
const IF_TYPE_LOOPBACK: u32 = 24;
const IF_TYPE_PROP_VIRTUAL: u32 = 53;
const IF_TYPE_WIFI: u32 = 71;
const IF_TYPE_TUNNEL: u32 = 131;
const IF_TYPE_WWAN_PP: u32 = 243;
const IF_TYPE_WWAN_PP2: u32 = 244;

const FLAG_HARDWARE: u8 = 1 << 0;
const FLAG_FILTER: u8 = 1 << 1;
const FLAG_NOT_MEDIA_CONNECTED: u8 = 1 << 4;

#[derive(Default)]
pub struct Nets {
    /// (LUID, interface index) of the adapters shown.
    known: Vec<(u64, u32)>,
}

fn kind_label(t: u32) -> &'static str {
    match t {
        IF_TYPE_WIFI => "Wi-Fi",
        IF_TYPE_ETHERNET => "Ethernet",
        IF_TYPE_WWAN_PP | IF_TYPE_WWAN_PP2 => "Mobile broadband",
        IF_TYPE_PPP => "Dial-up",
        IF_TYPE_PROP_VIRTUAL => "Virtual",
        _ => "Network",
    }
}

/// Adapters people think of as connections: up, not a loopback, tunnel or packet filter layered
/// on a real adapter.
fn shown(row: &MIB_IF_ROW2) -> bool {
    let f = row.InterfaceAndOperStatusFlags._bitfield;
    row.OperStatus == IfOperStatusUp
        && f & FLAG_FILTER == 0
        && f & FLAG_NOT_MEDIA_CONNECTED == 0
        && row.Type != IF_TYPE_LOOPBACK
        && row.Type != IF_TYPE_TUNNEL
        && (f & FLAG_HARDWARE != 0 || matches!(row.Type, IF_TYPE_ETHERNET | IF_TYPE_WIFI | IF_TYPE_PROP_VIRTUAL | IF_TYPE_PPP | IF_TYPE_WWAN_PP | IF_TYPE_WWAN_PP2))
}

fn link(row: &MIB_IF_ROW2) -> Option<u64> {
    let s = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
    // u64::MAX means unknown.
    (s > 0 && s != u64::MAX).then_some(s)
}

struct Addresses {
    luid: u64,
    ipv4: Vec<String>,
    ipv6: Vec<String>,
    dns_suffix: Option<String>,
}

impl Addresses {
    /// Has an address other than a self-assigned or link-local one, so traffic can reach it.
    fn reachable(&self) -> bool {
        self.ipv4.iter().any(|a| !a.starts_with("169.254.")) || self.ipv6.iter().any(|a| !a.to_ascii_lowercase().starts_with("fe80:"))
    }
}

/// A virtual adapter is worth showing when it has an address: VPNs, Hyper-V and WSL switches. The
/// ones without (WAN Miniports for IP, IPv6 and Network Monitor) are Windows plumbing that is
/// “up” but never carries traffic of its own.
fn worth_showing(row: &MIB_IF_ROW2, a: Option<&Addresses>) -> bool {
    row.InterfaceAndOperStatusFlags._bitfield & FLAG_HARDWARE != 0 || a.is_some_and(Addresses::reachable)
}

fn addresses() -> Vec<Addresses> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..4 {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let r = unsafe { GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
        match r {
            0 => break,
            111 => continue, // ERROR_BUFFER_OVERFLOW: size now holds what's needed
            _ => return Vec::new(),
        }
    }
    let mut out = Vec::new();
    let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !p.is_null() {
        let a = unsafe { &*p };
        let mut entry = Addresses { luid: unsafe { a.Luid.Value }, ipv4: Vec::new(), ipv6: Vec::new(), dns_suffix: None };
        let suffix = unsafe { from_pwstr(a.DnsSuffix.0) };
        if !suffix.is_empty() {
            entry.dns_suffix = Some(suffix);
        }
        let mut u = a.FirstUnicastAddress;
        while !u.is_null() {
            let ua = unsafe { &*u };
            let sa = ua.Address.lpSockaddr as *const u8;
            if !sa.is_null() {
                let family = unsafe { *(sa as *const u16) };
                match family {
                    2 if ua.Address.iSockaddrLength >= 8 => {
                        let b = unsafe { std::slice::from_raw_parts(sa.add(4), 4) };
                        entry.ipv4.push(Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string());
                    }
                    23 if ua.Address.iSockaddrLength >= 24 => {
                        let mut b = [0u8; 16];
                        b.copy_from_slice(unsafe { std::slice::from_raw_parts(sa.add(8), 16) });
                        entry.ipv6.push(Ipv6Addr::from(b).to_string());
                    }
                    _ => {}
                }
            }
            u = ua.Next;
        }
        out.push(entry);
        p = a.Next;
    }
    out
}

impl Nets {
    pub fn describe(&mut self) -> Vec<NetInfo> {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) }.0 != 0 || table.is_null() {
            return Vec::new();
        }
        let rows = unsafe { std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize) };
        let addrs = addresses();
        let mut out = Vec::new();
        self.known.clear();
        for row in rows.iter().filter(|r| shown(r)) {
            let luid = unsafe { row.InterfaceLuid.Value };
            let a = addrs.iter().find(|a| a.luid == luid);
            if !worth_showing(row, a) {
                continue;
            }
            let mac = (row.PhysicalAddressLength == 6).then(|| {
                row.PhysicalAddress[..6].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join("-")
            });
            self.known.push((luid, row.InterfaceIndex));
            out.push(NetInfo {
                id: luid,
                name: from_wide(&row.Alias),
                description: from_wide(&row.Description),
                kind: kind_label(row.Type).to_string(),
                mac,
                link_bps: link(row),
                ipv4: a.map(|a| a.ipv4.clone()).unwrap_or_default(),
                ipv6: a.map(|a| a.ipv6.clone()).unwrap_or_default(),
                dns_suffix: a.and_then(|a| a.dns_suffix.clone()),
                connected: true,
            });
        }
        unsafe { FreeMibTable(table as *const _) };
        out
    }

    pub fn sample(&mut self) -> Vec<RawNet> {
        let mut out = Vec::with_capacity(self.known.len());
        for &(luid, index) in &self.known {
            let mut row = MIB_IF_ROW2::default();
            row.InterfaceLuid.Value = luid;
            row.InterfaceIndex = index;
            if unsafe { GetIfEntry2(&mut row) }.0 != 0 || row.OperStatus != IfOperStatusUp {
                continue;
            }
            out.push(RawNet { id: luid, in_octets: row.InOctets, out_octets: row.OutOctets, link_bps: link(&row) });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_connected_adapters() {
        let mut n = Nets::default();
        let info = n.describe();
        let raw = n.sample();
        assert_eq!(info.len(), raw.len());
        for i in &info {
            assert!(!i.name.is_empty());
        }
    }

    #[test]
    fn plumbing_without_addresses_is_left_out() {
        let none = Addresses { luid: 1, ipv4: vec![], ipv6: vec![], dns_suffix: None };
        let link_local = Addresses { luid: 1, ipv4: vec!["169.254.3.4".into()], ipv6: vec!["fe80::1".into()], dns_suffix: None };
        let vpn = Addresses { luid: 1, ipv4: vec!["100.64.0.7".into()], ipv6: vec![], dns_suffix: None };
        assert!(!none.reachable());
        assert!(!link_local.reachable());
        assert!(vpn.reachable());
    }
}
