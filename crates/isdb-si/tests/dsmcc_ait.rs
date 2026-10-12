#![allow(missing_docs)]
//! Spec vectors for the data carousel and AIT codecs: sections built field
//! by field from the syntax tables of ARIB STD-B24 Vol. 3 (Tables 6-1,
//! 6-3, 6-21, 6-22, 6-24) and Vol. 4 (Tables 5-1 … 5-7), decoded and
//! re-encoded byte-identically.

use isdb_si::ait::{Ait, AitDescriptor, HttpSelector};
use isdb_si::bxml::{AdditionalAribBxmlInfo, AribBxmlInfo, BxmlEntry};
use isdb_si::dsmcc::{Ddb, Dii, ModuleDescriptor, module_blocks};
use isdb_ts::bits::BitWriter;
use isdb_ts::crc::crc32;

fn fields(rows: &[(&'static str, u32, u64)]) -> Vec<u8> {
    let mut w = BitWriter::new();
    for &(name, bits, v) in rows {
        w.write(name, bits, v).unwrap();
    }
    w.into_bytes()
}

fn with_crc(mut v: Vec<u8>) -> Vec<u8> {
    let c = crc32(&v);
    v.extend_from_slice(&c.to_be_bytes());
    v
}

/// DSM-CC section header (Vol. 3 Table 6-24): private_indicator 0,
/// current_next_indicator 1.
fn dsmcc_section(table_id: u8, ext: u16, version: u8, sn: u8, lsn: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = fields(&[
        ("table_id", 8, table_id as u64),
        ("section_syntax_indicator", 1, 1),
        ("private_indicator", 1, 0),
        ("reserved", 2, 0b11),
        ("dsmcc_section_length", 12, (5 + payload.len() + 4) as u64),
        ("table_id_extension", 16, ext as u64),
        ("reserved", 2, 0b11),
        ("version_number", 5, version as u64),
        ("current_next_indicator", 1, 1),
        ("section_number", 8, sn as u64),
        ("last_section_number", 8, lsn as u64),
    ]);
    v.extend_from_slice(payload);
    with_crc(v)
}

#[test]
fn spec_vol3_table_6_1_dii() {
    // module information: Type "multipart/mixed", Name "0000"
    let mut info = vec![0x01, 15];
    info.extend_from_slice(b"multipart/mixed");
    info.extend_from_slice(&[0x02, 4]);
    info.extend_from_slice(b"0000");
    let private = [0xF0u8, 0x01, 0b1111_1111]; // arib_bxml_privatedata: return_to_entry 1
    let mut body = fields(&[
        ("downloadId", 32, 0x3000_0001),
        ("blockSize", 16, 4066),
        ("windowSize", 8, 0),
        ("ackPeriod", 8, 0),
        ("tCDownloadWindow", 32, 0),
        ("tCDownloadScenario", 32, 1_500_000),
        ("compatibilityDescriptorLength", 16, 2),
        ("descriptorCount", 16, 0),
        ("numberOfModules", 16, 1),
        ("moduleId", 16, 0x0000),
        ("moduleSize", 32, 12_345),
        ("moduleVersion", 8, 7),
        ("moduleInfoLength", 8, info.len() as u64),
    ]);
    body.extend_from_slice(&info);
    body.extend_from_slice(&(private.len() as u16).to_be_bytes());
    body.extend_from_slice(&private);
    // Table 6-3 dsmccMessageHeader
    let mut msg = fields(&[
        ("protocolDiscriminator", 8, 0x11),
        ("dsmccType", 8, 0x03),
        ("messageId", 16, 0x1002),
        ("transaction_id", 32, 0x8000_0009),
        ("reserved", 8, 0xFF),
        ("adaptationLength", 8, 0),
        ("messageLength", 16, body.len() as u64),
    ]);
    msg.extend_from_slice(&body);
    let sec = dsmcc_section(0x3B, 0x0009, 0, 0, 0, &msg);
    let dii = Dii::decode(&sec).unwrap();
    assert_eq!(dii.body.transaction_id, 0x8000_0009);
    assert_eq!(dii.body.download_id, 0x3000_0001);
    assert_eq!(isdb_si::dsmcc::data_event_id(dii.body.download_id), 3);
    assert_eq!(dii.body.block_size, 4066);
    assert_eq!(dii.body.t_c_download_scenario, 1_500_000);
    assert_eq!(dii.body.compatibility, vec![0, 0]);
    assert_eq!(dii.body.modules.len(), 1);
    assert_eq!(dii.body.modules[0].module_size, 12_345);
    assert_eq!(dii.body.modules[0].module_version, 7);
    assert_eq!(
        dii.body.modules[0].info,
        vec![
            ModuleDescriptor::Type {
                media_type: b"multipart/mixed".to_vec()
            },
            ModuleDescriptor::Name {
                name: b"0000".to_vec()
            }
        ]
    );
    assert_eq!(
        dii.body.private,
        vec![ModuleDescriptor::AribBxmlPrivateData {
            return_to_entry: true,
            reserved: 0x7F
        }]
    );
    assert_eq!(dii.encode().unwrap(), sec);
    // the constructor gives the same header values
    let again = dii.body.clone().section();
    assert_eq!(again.header, dii.header);
}

#[test]
fn spec_vol3_table_6_21_ddb() {
    let data: Vec<u8> = (0..100u8).collect();
    let mut body = fields(&[
        ("moduleId", 16, 0x0002),
        ("moduleVersion", 8, 0x21),
        ("reserved", 8, 0xFF),
        ("blockNumber", 16, 3),
    ]);
    body.extend_from_slice(&data);
    let mut msg = fields(&[
        ("protocolDiscriminator", 8, 0x11),
        ("dsmccType", 8, 0x03),
        ("messageId", 16, 0x1003),
        ("downloadId", 32, 0x3000_0001),
        ("reserved", 8, 0xFF),
        ("adaptationLength", 8, 0),
        ("messageLength", 16, body.len() as u64),
    ]);
    msg.extend_from_slice(&body);
    // version_number = moduleVersion low 5 bits (0x21 → 1), section_number = blockNumber low 8
    let sec = dsmcc_section(0x3C, 0x0002, 0x21 & 0x1F, 3, 5, &msg);
    let ddb = Ddb::decode(&sec).unwrap();
    assert_eq!(ddb.body.module_id, 2);
    assert_eq!(ddb.body.module_version, 0x21);
    assert_eq!(ddb.body.block_number, 3);
    assert_eq!(ddb.body.data, data);
    assert_eq!(ddb.encode().unwrap(), sec);
    // module_blocks produces the same header mapping
    let blocks = module_blocks(0x3000_0001, 2, 0x21, 100, &vec![0u8; 550]).unwrap();
    assert_eq!(blocks.len(), 6);
    assert_eq!(blocks[3].header, ddb.header);
    assert_eq!(blocks[5].body.data.len(), 50);
    // a wrong messageId or protocolDiscriminator is refused
    let mut bad = sec.clone();
    bad[8] = 0x12;
    let n = bad.len();
    let c = crc32(&bad[..n - 4]);
    bad[n - 4..].copy_from_slice(&c.to_be_bytes());
    assert!(Ddb::decode(&bad).is_err());
}

#[test]
fn spec_vol4_table_5_1_ait() {
    // common loop: transport_protocol_descriptor (Table 5-4), HTTP selector
    let mut sel = vec![20u8];
    sel.extend_from_slice(b"http://www.xbc.co.jp/");
    sel[0] = 21;
    sel.push(1); // URL_extension_count
    sel.push(12);
    sel.extend_from_slice(b"hybrid/appsA");
    let mut tp = fields(&[
        ("protocol_id", 16, 0x0003),
        ("transport_protocol_label", 8, 0),
    ]);
    tp.extend_from_slice(&sel);
    let mut common = vec![0x02, tp.len() as u8];
    common.extend_from_slice(&tp);
    // application descriptor (Table 5-3)
    let app_desc = fields(&[
        ("application_profiles_length", 8, 5),
        ("application_profile", 16, 0x0002),
        ("version.major", 8, 1),
        ("version.minor", 8, 1),
        ("version.micro", 8, 1),
        ("service_bound_flag", 1, 1),
        ("visibility", 2, 0b11),
        ("reserved_for_future_use", 5, 0x1F),
        ("application_priority", 8, 1),
        ("transport_protocol_label", 8, 0),
    ]);
    let mut descs = vec![0x00, app_desc.len() as u8];
    descs.extend_from_slice(&app_desc);
    // simple application location (Table 5-5, tag 0x15 per B10 Part 1 Table 5-5)
    descs.extend_from_slice(&[0x15, 15]);
    descs.extend_from_slice(b"app1/index.html");
    // boundary and permission (Table 5-6)
    let mut bp = fields(&[
        ("permission_bitmap_count", 8, 1),
        ("permission_bitmap", 16, 0x1FFF),
        ("managed_URL_count", 8, 1),
        ("managed_URL_length", 8, 20),
    ]);
    bp.extend_from_slice(b"http://www.xbc.co.jp");
    descs.extend_from_slice(&[0x30, bp.len() as u8]);
    descs.extend_from_slice(&bp);
    // autostart priority (Table 5-7)
    descs.extend_from_slice(&[0x31, 1, 5]);
    let mut app = fields(&[
        ("organization_id", 16, 19),
        ("application_id", 32, 0x0000_0001),
        ("application_control_code", 8, 0x01),
        ("reserved_for_future_use", 4, 0xF),
        (
            "application_descriptors_loop_length",
            12,
            descs.len() as u64,
        ),
    ]);
    app.extend_from_slice(&descs);
    let mut body = fields(&[
        ("reserved_for_future_use", 4, 0xF),
        ("common_descriptors_length", 12, common.len() as u64),
    ]);
    body.extend_from_slice(&common);
    body.extend_from_slice(&fields(&[
        ("reserved_for_future_use", 4, 0xF),
        ("application_loop_length", 12, app.len() as u64),
    ]));
    body.extend_from_slice(&app);
    let mut sec = fields(&[
        ("table_id", 8, 0x74),
        ("section_syntax_indicator", 1, 1),
        ("reserved_for_future_use", 1, 0),
        ("reserved", 2, 0b11),
        ("section_length", 12, (5 + body.len() + 4) as u64),
        ("application_type", 16, 0x0010),
        ("reserved", 2, 0b11),
        ("version_number", 5, 2),
        ("current_next_indicator", 1, 1),
        ("section_number", 8, 0),
        ("last_section_number", 8, 0),
    ]);
    sec.extend_from_slice(&body);
    let sec = with_crc(sec);
    let ait = Ait::decode(&sec).unwrap();
    assert_eq!(ait.header.table_id_extension, 0x0010);
    assert_eq!(ait.header.version_number, 2);
    let AitDescriptor::TransportProtocol(t) = &ait.body.common_descriptors[0] else {
        panic!()
    };
    let urls = HttpSelector::decode(&t.selector).unwrap();
    assert_eq!(urls.urls[0].base, b"http://www.xbc.co.jp/");
    assert_eq!(urls.urls[0].extensions, vec![b"hybrid/appsA".to_vec()]);
    let a = &ait.body.applications[0];
    assert_eq!(
        (a.organization_id, a.application_id, a.control_code),
        (19, 1, 1)
    );
    assert_eq!(a.descriptors.len(), 4);
    assert_eq!(
        a.descriptors.iter().map(|d| d.tag()).collect::<Vec<_>>(),
        vec![0x00, 0x15, 0x30, 0x31]
    );
    let AitDescriptor::Application(ad) = &a.descriptors[0] else {
        panic!()
    };
    assert_eq!(ad.profiles[0].profile, 0x0002);
    assert_eq!(ait.encode().unwrap(), sec);
}

#[test]
fn spec_vol2_table_9_12_and_9_13() {
    // Table 9-12 rows: entry point, auto start, 1920x1080, no XML, default
    // versions, independent 0, style 0, reserved 1111; Annex C.1 data_event_id
    // 2, event_section 0, reserved 111; ondemand 0, storable 0, reserved 111111
    let b = fields(&[
        ("transmission_format", 2, 0b00),
        ("entry_point_flag", 1, 1),
        ("auto_start_flag", 1, 1),
        ("document_resolution", 4, 0b0001),
        ("use_xml", 1, 0),
        ("default_version_flag", 1, 1),
        ("independent_flag", 1, 0),
        ("style_for_tv_flag", 1, 0),
        ("reserved_future_use", 4, 0xF),
        ("data_event_id", 4, 2),
        ("event_section_flag", 1, 0),
        ("reserved", 3, 0b111),
        ("ondemand_retrieval_flag", 1, 0),
        ("file_storable_flag", 1, 0),
        ("reserved_future_use", 6, 0x3F),
    ]);
    let i = AdditionalAribBxmlInfo::decode(&b).unwrap();
    let BxmlEntry::Entry(e) = &i.entry else {
        panic!()
    };
    assert_eq!(e.document_resolution, 1);
    assert_eq!(i.carousel.unwrap().data_event_id, 2);
    assert_eq!(i.encode().unwrap(), b);
    // Table 9-13 with one carousel of Annex C.2.1 (component_size only)
    let b = fields(&[
        ("transmission_format", 2, 0b00),
        ("reserved_future_use", 1, 1),
        ("auto_start_flag", 1, 1),
        ("document_resolution", 4, 0b0011),
        ("use_xml", 1, 0),
        ("default_version_flag", 1, 0),
        ("independent_flag", 1, 0),
        ("content_id_flag", 1, 0),
        ("associated_contents_flag", 1, 0),
        ("reserved_future_use", 1, 1),
        ("style_for_tv_flag", 1, 0),
        ("update_flag", 1, 0),
        ("ISO_639_language_code", 24, 0x6A_70_6E),
        ("bml_major_version", 16, 3),
        ("bml_minor_version", 16, 0),
        ("num_of_carousels", 8, 1),
        ("component_tag", 8, 0x40),
        ("event_section_flag", 1, 0),
        ("reserved_future_use", 3, 0b111),
        ("component_size_flag", 1, 1),
        ("default_transaction_id_flag", 1, 0),
        ("default_timeout_DII_flag", 1, 0),
        ("default_leak_rate_flag", 1, 0),
        ("component_size", 32, 65_536),
        ("ondemand_retrieval_flag", 1, 0),
        ("file_storable_flag", 1, 0),
        ("reserved_future_use", 6, 0x3F),
    ]);
    let i = AribBxmlInfo::decode(&b).unwrap();
    assert_eq!(i.language, *b"jpn");
    assert_eq!(i.versions.unwrap().bml_major, 3);
    assert_eq!(i.carousels[0].component_size, Some(65_536));
    assert_eq!(i.carousels[0].transaction_id, None);
    assert_eq!(i.encode().unwrap(), b);
}
