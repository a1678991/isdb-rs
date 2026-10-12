# Glossary

Terms as they are used in this project, with the specification that
defines them. "B10", "B24", "B31", "B32" are ARIB STD-B10, -B24, -B31 and
-B32; "H.222.0" is ITU-T H.222.0 | ISO/IEC 13818-1 ([SPECS.md](../SPECS.md)).

## Organisations and documents

**ARIB** — Association of Radio Industries and Businesses, the Japanese
body that publishes the broadcasting standards (STD-Bxx) and technical
reports / operational guidelines (TR-Bxx).

**ISDB** — Integrated Services Digital Broadcasting, the Japanese family of
digital broadcasting systems: ISDB-T (terrestrial, B31), ISDB-S (satellite,
B20) and ISDB-S3 (advanced wide-band satellite, B44). This project
implements ISDB-T.

**IPTV Forum Japan (IPTVFJ)** — the industry forum that publishes the
Hybridcast specifications: STD-0010 (system), STD-0011 (HTML5 browser) and
STD-0013 (operational guideline). Organization identifiers for Hybridcast
applications are assigned by it.

**TR-B14** — ARIB's operational guidelines for digital terrestrial
television (in Japanese, sold by ARIB). Many concrete values used by
broadcasters (PCR placement, PID and component_tag assignments, remote
control keys, copy control defaults) are defined there rather than in the
standards. Not available to this project; its values appear here as
documented *operational choices*.

## Transport stream (H.222.0)

**TS, transport stream** — the MPEG-2 container used by digital
broadcasting: a sequence of 188-byte packets.

**TSP, transport stream packet** — one 188-byte packet: sync byte 0x47, a
header with the PID, a continuity counter and flags, an optional
adaptation field and a payload. With the 16 bytes of RS parity added by the
transmitter it is a 204-byte *transmission TSP*.

**PID** — the 13-bit packet identifier that says which stream a packet
belongs to. Some PIDs are fixed (0x0000 PAT, 0x0010 NIT, … B10 Table 5-1),
the rest are assigned through the PAT and PMT. 0x1FFF is the **null
packet**, which carries nothing and fills a constant-rate stream.

**Continuity counter (CC)** — a 4-bit counter per PID that increments with
every packet carrying payload; a jump reveals lost packets.

**Adaptation field** — optional bytes after the packet header carrying the
PCR, flags such as random_access_indicator, and stuffing.

**PCR, program clock reference** — a sample of the 27 MHz system clock sent
in the adaptation field, at least every 0.1 s; decoders lock their clock to
it. **PTS / DTS** — presentation / decoding time stamps (90 kHz) in PES
headers.

**PES, packetised elementary stream** — the packet format that carries
video, audio and caption data with their time stamps, split across TSPs.

**Section** — the container of a PSI/SI table: header (table_id,
section_length, table_id_extension, version_number, section numbers),
body, CRC_32. Sections are carried in TSPs of their PID, with a
pointer_field marking where a new section starts.

**PSI, program specific information** — the tables H.222.0 defines:
**PAT** (program association table: programs → PMT PIDs), **PMT** (program
map table: a program's elementary streams with their stream_type and
descriptors, and its PCR PID), **CAT** (conditional access table).

**Descriptor** — a tag-length-value structure inside a table that carries
one piece of information (a name, a frequency list, copy control, …).

**T-STD, transport stream system target decoder** — the H.222.0 buffer
model a multiplexer must respect so that a real decoder neither overflows
nor underflows.

**Elementary stream (ES)** — one coded stream (video, audio, captions)
of a program. **stream_type** (in the PMT) identifies its coding, e.g. 0x02
MPEG-2 video, 0x0F AAC in ADTS, 0x1B H.264, 0x06 PES private data.

## Service information (B10)

**SI, service information** — the ARIB/DVB tables that describe networks,
services and events: **NIT** (network information table: delivery
parameters, frequencies, remote-control key, service list), **SDT**
(service description table: service and provider names), **EIT** (event
information table: programme names and times; *present/following* = the
current and next event, *schedule* = the programme guide), **TOT / TDT**
(time offset / time and date table: the current time in JST), **BIT**
(broadcaster information table), and others (BAT, RST, ST, PCAT, NBIT,
LDT).

**network_id, original_network_id, transport_stream_id, service_id** —
the identifiers that name a network, the network a TS originates from, a
TS, and a service. A service_id equals the program_number of its PMT.

**Service, event** — a service is a channel (programme stream); an event
is one programme on it.

**remote_control_key_id** — the one-touch remote-control key (1–12 in
practice) a receiver files a terrestrial TS under (TS information
descriptor, B10 §6.2.42). Receivers number its services as key·10 + index.

**area_code** — the broadcast area of the terrestrial delivery system
descriptor (B10 Annex D).

**Digital copy control descriptor (0xC1)** — signals whether a programme
may be recorded and copied (copy-free, copy-one-generation, copy-never)
plus provider-defined bits; its *copy_control_type* (B10 Annex F) is what a
recorder tested here required to be '01'.

**ARIB 8-unit character coding** — the ISO/IEC 2022-style text encoding of
SI strings (B24 Vol. 1 Part 2 Ch. 7): switchable Kanji, kana and
alphanumeric sets, ARIB additional symbols, size control codes.

## ISDB-T transmission (B31)

**Segment** — the 6 MHz channel is divided into 13 OFDM segments of
6/14 MHz (≈ 429 kHz) each (the 14th share is guard band).

**Hierarchical layers A, B, C** — up to three groups of segments, each with
its own modulation, code rate and time interleaving, so that one channel
can carry a robust and a high-rate service.

**Partial reception, one-seg, full-seg** — with partial reception, layer A
is the single centre segment and can be received alone by a narrow-band
receiver: the *one-seg* service for mobile devices (usually QPSK 2/3). The
other 12 segments (*full-seg*, usually 64QAM 3/4) carry the HD service.

**Mode** — the FFT size: Mode 1 (2k, 108 carriers per segment), Mode 2
(4k, 216), Mode 3 (8k, 432). Japanese broadcasts use Mode 3.

**Guard interval (GI)** — the cyclic prefix in front of each OFDM symbol,
1/4, 1/8, 1/16 or 1/32 of the useful symbol length; it absorbs multipath
echoes. Japanese broadcasts use 1/8.

**OFDM frame** — 204 OFDM symbols (231.336 ms in Mode 3, GI 1/8). All
processing in this project is clocked by it.

**Multiplex frame, re-multiplexing, model receiver** — B31 §3.2: the TS of
all layers is arranged in a fixed slot pattern per OFDM frame, defined by a
model receiver; a receiver of all 13 segments reproduces one TS with that
slot order, with null packets in the slots that belong to no layer.

**TSP per frame** — how many transport packets a layer carries per OFDM
frame (B31 Table 3-3), e.g. 64 for one QPSK 2/3 segment in Mode 3 and 2592
for 12 segments of 64QAM 3/4.

**RS(204,188)** — the outer Reed-Solomon code: 16 parity bytes per packet,
corrects up to 8 byte errors.

**Energy dispersal** — XOR with a pseudo-random sequence to avoid long
runs of identical bits.

**Byte interleaving, convolutional code, puncturing, Viterbi** — the inner
FEC: a 12-path byte interleaver, a rate-1/2 K = 7 convolutional code
punctured to 2/3, 3/4, 5/6 or 7/8, decoded with the Viterbi algorithm.

**Bit interleaving, mapping** — bits are interleaved and mapped onto
carrier symbols: DQPSK (π/4-shift differential), QPSK, 16QAM or 64QAM.

**Time / frequency interleaving** — symbols are spread over several frames
(time interleaving, length I from the TMCC index) and across the carriers
of a layer (frequency interleaving), against fading and burst errors.

**SP, CP** — scattered and continual pilots: carriers with known values
the receiver uses for synchronisation and channel estimation.

**TMCC, transmission and multiplexing configuration control** — carriers
that signal the transmission parameters (mode of each layer, segments,
code rate, interleaving, partial reception, emergency flag), protected by a
difference-set code. The receiver learns all parameters from it.

**AC, auxiliary channel** — carriers for additional information, e.g. the
earthquake early-warning data of B31 §3.16; stuffing '1' otherwise.

**512/63 MHz** — the ISDB-T sample rate (≈ 8.127 MHz) used for baseband
here: FFT size × carrier spacing.

## Signals and measurement

**IQ, baseband, cf32, cs8** — complex samples of the signal around 0 Hz.
`cf32` = interleaved little-endian 32-bit float I/Q (GNU Radio "complex");
`cs8` = interleaved signed 8-bit I/Q (the HackRF's native format).

**Radio backend** — the code in `isdb-radio` that drives one kind of SDR
(today `hackrf`), selected with `--radio NAME[:key=value,...]`; its gains
are set with `--gain NAME=VALUE` and its settings are printed by
`--dry-run`. Each backend is behind its own cargo feature.

**Backoff** — how far the OFDM RMS level sits below integer full scale
(`--backoff-db`, default 10 dB) when samples are quantised for a radio with
integer samples or a cs8 file; OFDM peaks above full scale are clipped and
counted.

**MER, modulation error ratio** — the ratio of signal power to the error of
the received constellation points, in dB; a measure of signal quality per
layer.

**CFO, carrier frequency offset** — the frequency error between
transmitter and receiver, here expressed in carrier spacings.

**LLR, log-likelihood ratio** — the soft bit passed from the demodulator to
the Viterbi decoder: `ln P(b=0)/P(b=1)`, positive meaning 0.

**PER** — packet error rate (packets lost or uncorrectable).

**Conducted link** — transmitter and receiver connected by coax with
attenuators, so nothing is radiated ([transmitting.md](transmitting.md)).

**Oracle** — an independent implementation (here GNU Radio gr-isdbt) run
as a separate program to detect disagreements; it is not a reference, and
the spec decides who is right ([VERIFICATION.md](../VERIFICATION.md)).

## Audio and video (B32, ISO/IEC 13818)

**MPEG-2 video (H.262)** — the video coding of Japanese terrestrial HD
services (1440×1080 or 1920×1080 interlaced).

**AAC, ADTS** — MPEG-2 Advanced Audio Coding, carried in ADTS frames (a
header per frame). B32 Part 2 requires ADTS with a CRC (crc_check), the LC
profile and a constant-rate adts_buffer_fullness.

**H.264 one-seg** — the one-seg service uses H.264 video and HE-AAC audio;
this project can declare such streams in the PMT but not packetise them.

**B-CAS, conditional access, scrambling** — Japanese full-seg services are
scrambled and need a B-CAS (or ACAS) access-control module to view. This
project has no conditional-access support and never descrambles: input
must be unscrambled (transport_scrambling_control '00'), and scrambled input
is refused. The *CA_descriptor* (0x09, in the CAT or PMT) and the *ECM*
(entitlement control message) PID it points to are the conditional-access
signalling of a program; when a program is relayed they are kept by
default (`ca = "keep"`).

## Data broadcasting and Hybridcast (B24 Vol. 2–4, IPTVFJ)

**BML, Broadcast Markup Language** — the XML-based page language of ARIB
data broadcasting (B24 Vol. 2): documents (`text/X-arib-bml`, EUC-JP),
style sheets, scripts and images delivered in a data carousel and shown by
the receiver's BML browser; `startup.bml` in module 0x0000 is the document
launched first. The multiplexer's `bml-carousel` stream kind sends such
files.

**Data carousel, DII, DDB** — the DSM-CC download protocol as ARIB uses it
(B24 Vol. 3 Ch. 6): the files are *modules*, each cut into blocks sent as
DownloadDataBlock sections (DDB, table_id 0x3C) and described by a
DownloadInfoIndication (DII, 0x3B) that lists the modules with their sizes,
versions and descriptors (Type, Name, …). Everything is repeated
cyclically so a receiver can join at any time.

**Module, resource** — a module is one unit of the carousel (moduleId); it
is either one file ("direct mapping") or a `multipart/mixed` entity
holding several *resources*, each named by its `Content-Location` and
addressed as `arib-dc://…/<component_tag>/<moduleId>/<resource>`.

**Hybridcast** — the Japanese broadcast–broadband system (IPTVFJ STD-0010):
HTML5 applications started from the broadcast and run by the receiver's
HTML5 engine next to the programme.

**AIT, Application Information Table** — the table (table_id 0x74, B24
Vol. 4 Ch. 5) that names the applications of a service with their
`organization_id` / `application_id`, control code (AUTOSTART, PRESENT,
KILL), transport (the HTTP URL base) and start path. In Japanese
operation the section-format AIT travels inside the BML data carousel
(STD-0013) or is fetched over HTTP in XML form; the multiplexer's
`[service.hybridcast]` builds it.
