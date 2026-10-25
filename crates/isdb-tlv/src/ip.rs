//! What TLV packets carry: IPv4 / IPv6 packets, header-compressed IP
//! packets (ARIB STD-B32 v3.11 Part 3 §3.7 (1), HCfB) and the UDP payload
//! inside, which in broadcast is an MMTP packet (§3.9) or other data.
//!
//! HCfB header: context id (12 bits), sequence number (4), header type of
//! context identification (8), compressed header. Header types: 20h
//! partial IPv4 + partial UDP header, 21h IPv4 header identifier, 60h
//! partial IPv6 + partial UDP header, 61h no compressed header. The
//! partial IPv4 header omits total length, header checksum and options;
//! the partial IPv6 header omits payload length; the partial UDP header
//! omits length and checksum (Table No. 10 notes). So a 20h header is
//! IPv4's 20 bytes less 4 (length, checksum) plus UDP's 4 address bytes =
//! 16 + 4; a 60h header is IPv6's 40 bytes less 2 plus 4 = 42 bytes.
//! Context types 21h and 61h carry no address header: the payload follows
//! the 3-byte HCfB header directly (the addresses are those of the
//! context's last 20h / 60h packet).

/// A UDP flow: source and destination address and port.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Flow {
    /// Source address (4 or 16 bytes).
    pub src: Vec<u8>,
    /// Destination address.
    pub dst: Vec<u8>,
    /// Source port.
    pub sport: u16,
    /// Destination port.
    pub dport: u16,
}

impl std::fmt::Display for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let a = |v: &[u8]| -> String {
            if v.len() == 4 {
                format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])
            } else {
                std::net::Ipv6Addr::from(<[u8; 16]>::try_from(v).unwrap_or([0; 16])).to_string()
            }
        };
        write!(
            f,
            "{}:{} → {}:{}",
            a(&self.src),
            self.sport,
            a(&self.dst),
            self.dport
        )
    }
}

/// A UDP datagram recovered from a TLV packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    /// HCfB context id (compressed packets only).
    pub context: Option<u16>,
    /// The flow, when the packet carries its addresses.
    pub flow: Option<Flow>,
    /// The UDP payload.
    pub payload: Vec<u8>,
}

/// Why a TLV packet's IP content could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IpError {
    /// Shorter than its headers.
    #[error("IP: truncated")]
    Truncated,
    /// Not UDP (protocol / next header given).
    #[error("IP: protocol {0} is not UDP")]
    NotUdp(u8),
    /// Unknown HCfB header type.
    #[error("compressed IP: header type {0:#04x}")]
    HeaderType(u8),
    /// Wrong IP version nibble.
    #[error("IP: version {0}")]
    Version(u8),
}

fn udp(b: &[u8]) -> Result<(u16, u16, &[u8]), IpError> {
    if b.len() < 8 {
        return Err(IpError::Truncated);
    }
    Ok((
        u16::from_be_bytes([b[0], b[1]]),
        u16::from_be_bytes([b[2], b[3]]),
        &b[8..],
    ))
}

/// An uncompressed IPv4 packet (TLV type 01h) carrying UDP.
pub fn ipv4(b: &[u8]) -> Result<Datagram, IpError> {
    if b.len() < 20 {
        return Err(IpError::Truncated);
    }
    if b[0] >> 4 != 4 {
        return Err(IpError::Version(b[0] >> 4));
    }
    let ihl = usize::from(b[0] & 0x0F) * 4;
    if b[9] != 17 {
        return Err(IpError::NotUdp(b[9]));
    }
    let (sport, dport, payload) = udp(b.get(ihl..).ok_or(IpError::Truncated)?)?;
    Ok(Datagram {
        context: None,
        flow: Some(Flow {
            src: b[12..16].to_vec(),
            dst: b[16..20].to_vec(),
            sport,
            dport,
        }),
        payload: payload.to_vec(),
    })
}

/// An uncompressed IPv6 packet (TLV type 02h) carrying UDP (no extension
/// headers).
pub fn ipv6(b: &[u8]) -> Result<Datagram, IpError> {
    if b.len() < 40 {
        return Err(IpError::Truncated);
    }
    if b[0] >> 4 != 6 {
        return Err(IpError::Version(b[0] >> 4));
    }
    if b[6] != 17 {
        return Err(IpError::NotUdp(b[6]));
    }
    let (sport, dport, payload) = udp(&b[40..])?;
    Ok(Datagram {
        context: None,
        flow: Some(Flow {
            src: b[8..24].to_vec(),
            dst: b[24..40].to_vec(),
            sport,
            dport,
        }),
        payload: payload.to_vec(),
    })
}

/// A header-compressed IP packet (TLV type 03h, HCfB).
pub fn compressed(b: &[u8]) -> Result<Datagram, IpError> {
    if b.len() < 3 {
        return Err(IpError::Truncated);
    }
    let context = u16::from_be_bytes([b[0], b[1]]) >> 4;
    let ht = b[2];
    let rest = &b[3..];
    let (flow, payload) = match ht {
        0x20 => {
            // partial IPv4 (20 − total length − checksum = 16 bytes) +
            // ports (4)
            if rest.len() < 20 {
                return Err(IpError::Truncated);
            }
            // the 16 bytes are: ver/ihl, tos, id(2), flags/frag(2), ttl,
            // proto, src(4), dst(4)
            if rest[7] != 17 {
                return Err(IpError::NotUdp(rest[7]));
            }
            (
                Some(Flow {
                    src: rest[8..12].to_vec(),
                    dst: rest[12..16].to_vec(),
                    sport: u16::from_be_bytes([rest[16], rest[17]]),
                    dport: u16::from_be_bytes([rest[18], rest[19]]),
                }),
                &rest[20..],
            )
        }
        0x60 => {
            // partial IPv6 (40 − payload length = 38 bytes) + ports (4):
            // ver/tc/flow(4), next header, hop limit, src(16), dst(16)
            if rest.len() < 42 {
                return Err(IpError::Truncated);
            }
            if rest[4] != 17 {
                return Err(IpError::NotUdp(rest[4]));
            }
            (
                Some(Flow {
                    src: rest[6..22].to_vec(),
                    dst: rest[22..38].to_vec(),
                    sport: u16::from_be_bytes([rest[38], rest[39]]),
                    dport: u16::from_be_bytes([rest[40], rest[41]]),
                }),
                &rest[42..],
            )
        }
        0x21 => {
            // IPv4 header identifier: the identification field (2 bytes)
            if rest.len() < 2 {
                return Err(IpError::Truncated);
            }
            (None, &rest[2..])
        }
        0x61 => (None, rest),
        t => return Err(IpError::HeaderType(t)),
    };
    Ok(Datagram {
        context: Some(context),
        flow,
        payload: payload.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_ipv6_header_type_60() {
        let mut b = vec![0x00, 0x13, 0x60]; // context 1, sequence 3
        b.extend([0x60, 0, 0, 0, 17, 64]);
        b.extend([0x20u8; 16]);
        b.extend([0xFFu8; 16]);
        b.extend([0x30, 0x39, 0x30, 0x3A]); // ports 12345 → 12346
        b.extend([1, 2, 3]);
        let d = compressed(&b).unwrap();
        assert_eq!(d.context, Some(1));
        let f = d.flow.unwrap();
        assert_eq!((f.sport, f.dport), (12345, 12346));
        assert_eq!(d.payload, vec![1, 2, 3]);
    }

    #[test]
    fn compressed_type_61_has_no_header() {
        let d = compressed(&[0x00, 0x24, 0x61, 9, 9]).unwrap();
        assert_eq!(d.context, Some(2));
        assert_eq!(d.flow, None);
        assert_eq!(d.payload, vec![9, 9]);
    }
}
