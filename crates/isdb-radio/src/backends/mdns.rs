//! A one-shot mDNS lookup (RFC 6762; DNS-SD, RFC 6763) for the `pluto`
//! backend: which hosts on the local link answer for a DNS-SD service
//! (libiio's `_iio._tcp.local`, which iiod announces) or for a host name
//! (`pluto.local`).
//!
//! The query is a legacy unicast one (RFC 6762 §6.7): one packet with a
//! PTR question for the service and an A question for the host, sent from
//! an ephemeral port to 224.0.0.251:5353, so responders (avahi on the
//! Pluto firmware) answer straight to that port with the query's ID and
//! nothing joins the multicast group. A device's address is the source of
//! its answer, the address it has on the link the query went out on (the
//! interface of the IPv4 default route). IPv4 only.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::debug;

/// The mDNS group and port.
const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
/// Record types and class (RFC 1035 §3.2.2, RFC 2782).
const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const CLASS_IN: u16 = 1;
/// When the query is sent again while nothing has answered.
const RESEND_AT: [Duration; 2] = [Duration::from_millis(250), Duration::from_millis(750)];
/// How long to wait for further devices after the first answer.
const SETTLE: Duration = Duration::from_millis(250);

/// A device that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    /// The service instance (`iio on pluto`) or the host name it answered
    /// for.
    pub(crate) name: String,
    /// The address its answer came from.
    pub(crate) addr: Ipv4Addr,
}

/// Ask the local link for `service` (PTR) and `host` (A), waiting at most
/// `wait`; one [`Answer`] per answering address, in the order they came.
pub(crate) fn lookup(service: &str, host: &str, wait: Duration) -> io::Result<Vec<Answer>> {
    let id = query_id();
    let packet = query(id, &[(service, TYPE_PTR), (host, TYPE_A)]);
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_multicast_ttl_v4(255)?;
    let start = Instant::now();
    let mut deadline = start + wait;
    let mut resent = 0;
    socket.send_to(&packet, GROUP)?;
    let mut found: Vec<Answer> = Vec::new();
    let mut buf = [0u8; 9000];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(found);
        }
        let resend = RESEND_AT
            .get(resent)
            .map(|&at| start + at)
            .filter(|_| found.is_empty());
        let until = resend.map_or(deadline, |r| r.min(deadline));
        if until <= now {
            resent += 1;
            socket.send_to(&packet, GROUP)?;
            continue;
        }
        socket.set_read_timeout(Some(until - now))?;
        let (n, from) = match socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        let SocketAddr::V4(from) = from else { continue };
        if from.port() != GROUP.port() {
            continue;
        }
        let Some(name) = answered(&buf[..n], id, service, host) else {
            continue;
        };
        debug!(
            from = %from.ip(),
            name = %name,
            elapsed_ms = start.elapsed().as_millis(),
            "mDNS answer"
        );
        if !found.iter().any(|a| a.addr == *from.ip()) {
            found.push(Answer {
                name,
                addr: *from.ip(),
            });
        }
        deadline = deadline.min(Instant::now() + SETTLE);
    }
}

/// An ID for the query, so answers to someone else's legacy unicast
/// query are not taken for ours.
fn query_id() -> u16 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let [a, b, c, d] = (nanos ^ std::process::id().rotate_left(16)).to_be_bytes();
    u16::from_be_bytes([a ^ c, b ^ d])
}

/// A standard query (RD clear) with `questions` of (name, type), class IN.
fn query(id: u16, questions: &[(&str, u16)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(64);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    let count = u16::try_from(questions.len()).expect("a handful of questions");
    p.extend_from_slice(&count.to_be_bytes());
    p.extend_from_slice(&[0; 6]);
    for &(name, kind) in questions {
        for label in name.trim_end_matches('.').split('.') {
            let len = u8::try_from(label.len())
                .ok()
                .filter(|&l| (1..=63).contains(&l))
                .expect("DNS labels are 1..=63 bytes");
            p.push(len);
            p.extend_from_slice(label.as_bytes());
        }
        p.push(0);
        p.extend_from_slice(&kind.to_be_bytes());
        p.extend_from_slice(&CLASS_IN.to_be_bytes());
    }
    p
}

/// What `packet` answers of our query `id`: the instance of a PTR record
/// of `service`, or `host` for an A record of it. `None` for anything else
/// (a query, another ID, a malformed packet, other records only).
fn answered(packet: &[u8], id: u16, service: &str, host: &str) -> Option<String> {
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_be_bytes(packet.get(at..at + 2)?.try_into().ok()?))
    };
    // a response (QR) to our ID
    if u16_at(0)? != id || u16_at(2)? & 0x8000 == 0 {
        return None;
    }
    let questions = u16_at(4)?;
    let records = [u16_at(6)?, u16_at(8)?, u16_at(10)?]
        .into_iter()
        .map(usize::from)
        .sum::<usize>();
    let mut at = 12;
    for _ in 0..questions {
        at = name(packet, at)?.1 + 4;
    }
    let mut instance = None;
    let mut host_seen = false;
    for _ in 0..records {
        let (owner, next) = name(packet, at)?;
        let kind = u16_at(next)?;
        // class with the cache-flush bit cleared
        let class = u16_at(next + 2)? & 0x7fff;
        let len = usize::from(u16_at(next + 8)?);
        let rdata = next + 10;
        packet.get(rdata..rdata + len)?;
        if class == CLASS_IN {
            match kind {
                TYPE_PTR if same(&owner, service) => {
                    instance.get_or_insert(name(packet, rdata)?.0);
                }
                TYPE_A if len == 4 && same(&owner, host) => host_seen = true,
                _ => {}
            }
        }
        at = rdata + len;
    }
    instance
        .map(|i| {
            let suffix = format!(".{}", service.trim_end_matches('.'));
            match i.strip_suffix(&suffix) {
                Some(short) => short.to_string(),
                None => i,
            }
        })
        .or_else(|| host_seen.then(|| host.trim_end_matches('.').to_string()))
}

/// DNS names compare case-insensitively, with or without the root dot.
fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('.')
        .eq_ignore_ascii_case(b.trim_end_matches('.'))
}

/// The name at `at` (labels joined by `.`, compression pointers followed)
/// and the offset after it in place.
fn name(packet: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    // each pointer must go backwards, so a loop cannot be followed
    let mut limit = at;
    loop {
        let len = *packet.get(at)?;
        match len >> 6 {
            0 if len == 0 => {
                return Some((labels.join("."), end.unwrap_or(at + 1)));
            }
            0 => {
                let label = packet.get(at + 1..at + 1 + usize::from(len))?;
                labels.push(String::from_utf8_lossy(label).into_owned());
                at += 1 + usize::from(len);
            }
            3 => {
                let to = usize::from(u16::from_be_bytes([len, *packet.get(at + 1)?]) & 0x3fff);
                if to >= limit {
                    return None;
                }
                end.get_or_insert(at + 2);
                limit = to;
                at = to;
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE: &str = "_iio._tcp.local";
    const HOST: &str = "pluto.local";

    /// A response header for `id` with `an` answers and `ar` additional
    /// records, after the question section `q` (count `qd`).
    fn response(id: u16, qd: u16, q: &[u8], an: u16, ar: u16) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&0x8400u16.to_be_bytes());
        for n in [qd, an, 0, ar] {
            p.extend_from_slice(&n.to_be_bytes());
        }
        p.extend_from_slice(q);
        p
    }

    fn encoded(name: &str) -> Vec<u8> {
        query(0, &[(name, 0)])[12..]
            .split_at(name.len() + 2)
            .0
            .to_vec()
    }

    fn record(p: &mut Vec<u8>, owner: &[u8], kind: u16, class: u16, rdata: &[u8]) {
        p.extend_from_slice(owner);
        p.extend_from_slice(&kind.to_be_bytes());
        p.extend_from_slice(&class.to_be_bytes());
        p.extend_from_slice(&120u32.to_be_bytes());
        p.extend_from_slice(&u16::try_from(rdata.len()).unwrap().to_be_bytes());
        p.extend_from_slice(rdata);
    }

    /// What answers on this link (sends one query; not in CI):
    /// `cargo test -p isdb-radio mdns::tests::live -- --ignored --nocapture`;
    /// `ISDB_MDNS_HOST=NAME.local` asks for another host name.
    #[test]
    #[ignore = "sends an mDNS query on the local link"]
    fn live() {
        let host = std::env::var("ISDB_MDNS_HOST").unwrap_or_else(|_| HOST.into());
        let found = lookup(SERVICE, &host, Duration::from_secs(2)).unwrap();
        println!("{} answer(s): {found:?}", found.len());
    }

    #[test]
    fn the_query_has_both_questions() {
        let q = query(0xbeef, &[(SERVICE, TYPE_PTR), (HOST, TYPE_A)]);
        let mut want = vec![0xbe, 0xef, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
        want.extend_from_slice(b"\x04_iio\x04_tcp\x05local\x00\x00\x0c\x00\x01");
        want.extend_from_slice(b"\x05pluto\x05local\x00\x00\x01\x00\x01");
        assert_eq!(q, want);
    }

    #[test]
    fn a_ptr_answer_with_compressed_names_gives_the_instance() {
        // question echoed (legacy unicast), answer owner a pointer to it,
        // the instance a label plus a pointer to the service
        let q = [encoded(SERVICE), vec![0, 12, 0, 1]].concat();
        let mut p = response(7, 1, &q, 1, 0);
        record(
            &mut p,
            &[0xc0, 12],
            TYPE_PTR,
            CLASS_IN,
            b"\x0ciio on pluto\xc0\x0c",
        );
        assert_eq!(
            answered(&p, 7, SERVICE, HOST).as_deref(),
            Some("iio on pluto")
        );
        assert_eq!(answered(&p, 8, SERVICE, HOST), None, "another query's ID");
    }

    #[test]
    fn an_a_answer_gives_the_host_and_the_cache_flush_bit_is_ignored() {
        let mut p = response(7, 0, &[], 0, 1);
        record(
            &mut p,
            &encoded("PLUTO.local"),
            TYPE_A,
            0x8001,
            &[192, 0, 2, 30],
        );
        assert_eq!(
            answered(&p, 7, SERVICE, HOST).as_deref(),
            Some("pluto.local")
        );
    }

    #[test]
    fn queries_other_records_and_malformed_packets_answer_nothing() {
        let mut p = response(7, 0, &[], 1, 0);
        record(
            &mut p,
            &encoded("other.local"),
            TYPE_A,
            CLASS_IN,
            &[192, 0, 2, 1],
        );
        assert_eq!(answered(&p, 7, SERVICE, HOST), None, "another host");
        let mut q = p.clone();
        q[2] = 0;
        assert_eq!(answered(&q, 7, SERVICE, HOST), None, "a query");
        for n in 0..p.len() {
            assert_eq!(answered(&p[..n], 7, SERVICE, HOST), None, "cut at {n}");
        }
        // a pointer to itself
        let mut l = response(7, 0, &[], 1, 0);
        record(&mut l, &[0xc0, 12], TYPE_A, CLASS_IN, &[192, 0, 2, 1]);
        assert_eq!(answered(&l, 7, SERVICE, HOST), None, "pointer loop");
    }
}
