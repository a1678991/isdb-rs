//! The delivery-system classes that `validate`'s `cross.delivery`
//! compares: the delivery descriptor (ARIB STD-B10 Part 2 Table 6-1), the
//! broadcasting_identifier (§6.2.21, Table 6-36) and the network_id
//! (Annex N, Table N-1, informative).

use isdb_si::values::BroadcastingIdentifier;

/// The delivery system a value of the NIT points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeliveryClass {
    /// terrestrial_delivery_system_descriptor (0xFA): B10 Table 6-1
    /// "Mandatory for digital terrestrial broadcasting and multimedia
    /// broadcasting".
    Terrestrial,
    /// satellite_delivery_system_descriptor (0x43): B10 Table 6-1
    /// "Mandatory in digital satellite broadcasting".
    Satellite,
}

impl DeliveryClass {
    pub(crate) fn name(self) -> &'static str {
        match self {
            DeliveryClass::Terrestrial => "terrestrial",
            DeliveryClass::Satellite => "satellite",
        }
    }
}

/// broadcasting_identifier → delivery class (B10 Table 6-36, split by the
/// Table 6-1 rows of 0xFA and 0x43: `isdb-si`'s
/// [`BroadcastingIdentifier::is_terrestrial`] / `is_satellite`).
/// Undefined codes have no class.
pub(crate) fn identifier_class(id: BroadcastingIdentifier) -> Option<DeliveryClass> {
    if id.is_satellite() {
        Some(DeliveryClass::Satellite)
    } else if id.is_terrestrial() {
        Some(DeliveryClass::Terrestrial)
    } else {
        None
    }
}

/// network_id → delivery class by B10 Annex N Table N-1 (informative):
/// 0x0001–0x000C are the satellite networks (CS, BS, 110° CS, advanced
/// BS/CS), 0x7880–0x7FE8 "Terrestrial digital television broadcasting".
/// Other values have no class here.
pub(crate) fn network_id_class(nid: u16) -> Option<DeliveryClass> {
    match nid {
        0x0001..=0x000C => Some(DeliveryClass::Satellite),
        0x7880..=0x7FE8 => Some(DeliveryClass::Terrestrial),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_si::values::CodedValue;

    #[test]
    fn classes() {
        let id = BroadcastingIdentifier::from_code;
        assert_eq!(
            identifier_class(id(0b00_0010)),
            Some(DeliveryClass::Satellite)
        );
        assert_eq!(
            identifier_class(id(0b00_0011)),
            Some(DeliveryClass::Terrestrial)
        );
        assert_eq!(identifier_class(id(0b00_0110)), None);
        assert_eq!(network_id_class(0x0004), Some(DeliveryClass::Satellite));
        assert_eq!(network_id_class(0x79EE), Some(DeliveryClass::Terrestrial));
        assert_eq!(network_id_class(0x0200), None);
    }
}
