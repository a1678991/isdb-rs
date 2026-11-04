"""The chain's register map, `registers.toml` (next to this module): the one
source of `SlotsRx`'s registers for the HDL, the host and their tests
(issue #30 item 5; the file's header has the schema).

`load()` parses it into `RegisterMap` (registers with their elements' addresses
and resets, the FEATURES flags, the CTRL bits, the command protocol, the host's
sequences); `render_docstring()` and `render_readme()` write the two Markdown
tables generated from it (`top.py`'s docstring, fpga/README.md "Register
map"), which `tools/regmap.py` puts between the `BEGIN` / `END` markers and
tests/test_registers.py checks are current. Pure Python (`tomllib`), no
Amaranth: a timed host model (issue #30 item 4) may read the map the same way.
"""

import dataclasses
import tomllib
from pathlib import Path

PATH = Path(__file__).with_name("registers.toml")
BEGIN = "<!-- regmap:begin: generated from isdb_fpga/chain/registers.toml by tools/regmap.py -->"
END = "<!-- regmap:end -->"
ACCESS = ("ro", "rw", "staged", "strobe")
HOST = ("", "r", "w", "rw")
CHANGES = ("const", "host", "live")


@dataclasses.dataclass(frozen=True)
class Field:
    name: str
    lsb: int
    width: int
    meaning: str
    feature: bool
    built: bool
    source: str

    @property
    def mask(self):
        return ((1 << self.width) - 1) << self.lsb

    def bits(self):
        return str(self.lsb) if self.width == 1 else f"{self.lsb + self.width - 1}-{self.lsb}"


@dataclasses.dataclass(frozen=True)
class Register:
    name: str
    address: int
    count: int
    access: str
    width: int
    signed: bool
    resets: tuple  # per element
    read: str
    write: str
    takes_effect: str
    feature: str
    write_feature: str
    ignored_write: object
    built: bool
    answer_after: int
    holds_previous: bool
    changes: str
    snapshot: str
    host: str
    host_note: str
    elements: tuple
    fields: tuple
    raw: dict

    @property
    def addresses(self):
        return [self.address + k for k in range(self.count)]

    @property
    def end(self):
        return self.address + self.count

    @property
    def mask(self):
        return (1 << self.width) - 1

    @property
    def writable(self):
        """A write changes something on this image."""
        return self.access != "ro"

    def field(self, name):
        return next(f for f in self.fields if f.name == name)


@dataclasses.dataclass(frozen=True)
class RegisterMap:
    window: tuple
    registers: tuple
    protocol: dict
    ctrl: dict  # name -> bit
    ctrl_meaning: dict
    frontend: dict  # name -> dict
    id_word: dict
    host: dict  # sequence name -> list of steps
    raw: dict

    def __getitem__(self, name):
        for r in self.registers:
            if r.name == name:
                return r
        raise KeyError(name)

    def at(self, addr):
        """The register and element index at `addr`, or None (unmapped)."""
        for r in self.registers:
            if r.address <= addr < r.end:
                return r, addr - r.address
        return None

    @property
    def features(self):
        """FEATURES' flags: name -> Field (lsb the bit, built)."""
        return {f.name: f for f in self["FEATURES"].fields if f.feature}

    @property
    def feature_bits(self):
        """The flags this image sets."""
        return sum(1 << f.lsb for f in self.features.values() if f.built)

    def unmapped(self):
        """The window's addresses no register has."""
        lo, hi = self.window
        return [a for a in range(lo, hi) if self.at(a) is None]

    def reset_at(self, addr):
        """The value an address reads after configuration (0 unmapped)."""
        hit = self.at(addr)
        if hit is None:
            return 0
        r, k = hit
        return r.resets[k]

    def gpio_in_after(self, r):
        """Symbol clocks from the READ's gpio_out write to its value on gpio_in."""
        p = self.protocol
        return p["decode_after"] + r.answer_after + p["readback_after_answer"]


def _fields(raw):
    out = []
    for f in raw.get("field", ()):
        out.append(Field(f["name"], f["lsb"], f.get("width", 1), f.get("meaning", ""), f.get("feature", False),
                         f.get("built", True), f.get("source", "")))
    return tuple(out)


def _register(raw):
    count = raw.get("count", 1)
    reset = raw.get("reset", 0)
    resets = tuple(reset) if isinstance(reset, list) else (reset,) * count
    width = raw.get("width", 32)
    return Register(
        name=raw["name"], address=raw["address"], count=count, access=raw["access"], width=width,
        signed=raw.get("signed", False), resets=resets, read=raw.get("read", ""), write=raw.get("write", ""),
        takes_effect=raw.get("takes_effect", ""), feature=raw.get("feature", ""),
        write_feature=raw.get("write_feature", ""), ignored_write=raw.get("ignored_write"),
        built=raw.get("built", True), answer_after=raw.get("answer_after", 0),
        holds_previous=raw.get("holds_previous", False), changes=raw.get("changes", ""),
        snapshot=raw.get("snapshot", ""), host=raw.get("host", ""), host_note=raw.get("host_note", ""),
        elements=tuple(raw.get("elements", ())), fields=_fields(raw), raw=raw)


def load(path=PATH):
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    return RegisterMap(
        window=tuple(raw["window"]),
        registers=tuple(_register(r) for r in raw["register"]),
        protocol=raw["protocol"],
        ctrl={c["name"]: c["bit"] for c in raw["ctrl"]},
        ctrl_meaning={c["name"]: c["meaning"] for c in raw["ctrl"]},
        frontend={f["name"]: f for f in raw["frontend"]},
        id_word=raw["id_word"],
        host=raw.get("host", {}),
        raw=raw,
    )


def problems(m):
    """What is wrong with the map itself (empty when well formed)."""
    out = []
    lo, hi = m.window
    seen = {}
    names = set()
    for r in m.registers:
        if r.name in names:
            out.append(f"{r.name}: named twice")
        names.add(r.name)
        if r.access not in ACCESS:
            out.append(f"{r.name}: access {r.access!r} not one of {ACCESS}")
        if r.host not in HOST:
            out.append(f"{r.name}: host {r.host!r} not one of {HOST}")
        if r.changes not in CHANGES:
            out.append(f"{r.name}: changes {r.changes!r} not one of {CHANGES}")
        if len(r.resets) != r.count:
            out.append(f"{r.name}: {len(r.resets)} resets for {r.count} registers")
        if not r.read:
            out.append(f"{r.name}: no read semantics")
        if r.writable and not r.write:
            out.append(f"{r.name}: writable without write semantics")
        if r.access == "staged" and not r.takes_effect:
            out.append(f"{r.name}: staged without takes_effect")
        if (r.write_feature or r.ignored_write is not None) and r.access != "ro":
            out.append(f"{r.name}: write_feature / ignored_write on a writable register")
        if not r.built and r.access != "ro":
            out.append(f"{r.name}: not built but writable")
        if r.elements and len(r.elements) != r.count:
            out.append(f"{r.name}: {len(r.elements)} element names for {r.count} registers")
        if r.count > 1 and r.changes == "live" and not r.snapshot:
            out.append(f"{r.name}: a live multi-word value without a snapshot rule")
        for a in r.addresses:
            if not lo <= a < hi:
                out.append(f"{r.name}: {a:#x} outside the window")
            if a in seen:
                out.append(f"{r.name}: {a:#x} is also {seen[a]}'s")
            seen[a] = r.name
        for v in r.resets:
            if not 0 <= v <= r.mask:
                out.append(f"{r.name}: reset {v:#x} wider than {r.width} bits")
        used = 0
        for f in r.fields:
            if f.lsb + f.width > r.width:
                out.append(f"{r.name}.{f.name}: bits {f.bits()} beyond the width {r.width}")
            if used & f.mask:
                out.append(f"{r.name}.{f.name}: overlaps another field")
            used |= f.mask
        for f in r.fields:
            if f.source and f.source not in names | {x.name for x in m.registers}:
                out.append(f"{r.name}.{f.name}: unknown source register {f.source}")
        for feat in (r.feature, r.write_feature):
            if feat and feat not in m.features:
                out.append(f"{r.name}: unknown FEATURES flag {feat}")
    for c in m.raw["ctrl"]:
        if "restart_within" in c and str(c["restart_within"]) not in c["meaning"]:
            out.append(f"CTRL {c['name']}: restart_within {c['restart_within']} not in its meaning")
    for f in m.features.values():
        if f.width != 1:
            out.append(f"FEATURES.{f.name}: a flag of {f.width} bits")
    return out


# ---- the generated tables ---------------------------------------------------

def _hex(v):
    s = f"{v:X}"
    if len(s) > 4:
        s = s[:-4].rjust(4, "0") + "_" + s[-4:]
    return "0x" + s


def _reset(r):
    if not r.built:
        return "0"
    vals = set(r.resets)
    if len(vals) == 1:
        v = r.resets[0]
        return str(v) if v < 0x100 else _hex(v)
    odd = [f"[{k}] {_hex(v)}" for k, v in enumerate(r.resets) if v]
    return ", ".join(odd) + ", else 0"


def _addr(r):
    if r.count == 1:
        return f"0x{r.address:03X}"
    return f"0x{r.address:03X} .. 0x{r.end - 1:03X}"


def _bits(r):
    def one(f):
        s = f"{f.bits()} {f.name}"
        if f.meaning or not f.built:
            s += " (" + ("not built, reads 0: " if not f.built else "") + f.meaning + ")"
        return s
    return ", ".join(one(f) for f in r.fields)


def _gate(r):
    out = []
    if r.feature:
        out.append(f"FEATURES {r.feature}")
    if r.write_feature:
        out.append(f"writable with FEATURES {r.write_feature}")
    return "; ".join(out)


def _read(r):
    s = r.read
    if r.answer_after:
        s += (f"; a READ is answered {r.answer_after} symbol clocks after it is decoded"
              + (", gpio_in keeping the previous READ's value until then" if r.holds_previous else ""))
    return s


def _write(r):
    if r.access == "ro":
        return f"ignored: {r.write}" if r.write else "ignored"
    s = r.write
    if r.takes_effect:
        s += f" (in use: {r.takes_effect})"
    return s


def _cell(s):
    return s.replace("|", "\\|")


def render_docstring(m):
    """`top.py`'s docstring table: one row per register, then CTRL."""
    rows = ["| Address | Register |", "|---|---|"]
    for r in m.registers:
        head = f"{r.name} ({r.access}"
        if r.width != 32:
            head += f", {r.width} bits"
        head += f", reset {_reset(r)}"
        if _gate(r):
            head += f"; {_gate(r)}"
        head += ")"
        parts = [f"read, {_read(r)}"]
        if r.fields:
            parts.append(f"bits {_bits(r)}")
        if r.elements:
            parts.append("elements " + ", ".join(r.elements))
        parts.append(f"written, {_write(r)}")
        if r.snapshot:
            parts.append(f"consistency: {r.snapshot}")
        rows.append(f"| {_addr(r)} | {_cell(head + ': ' + '; '.join(parts))} |")
    ctrl = "; ".join(f"bit {b} {n}: {m.ctrl_meaning[n]}" for n, b in sorted(m.ctrl.items(), key=lambda x: x[1]))
    rows.append(f"| CTRL | the front end's bits: {_cell(ctrl)} |")
    rows.append(f"| other | {_cell(m.protocol['unmapped'])}; {_cell(m.protocol['outside'])} |")
    return "\n".join(rows)


def render_readme(m):
    """fpga/README.md's table: one row per register, with the host's use."""
    rows = ["| Address | Register | Access | Reset | Bits | Read | Write | Host |", "|---|---|---|---|---|---|---|---|"]
    host = {"": "—", "r": "reads", "w": "writes", "rw": "reads, writes"}
    for r in m.registers:
        name = r.name + (f" ({_gate(r)})" if _gate(r) else "")
        access = r.access + ("" if r.width == 32 else f", {r.width} bits") + (", signed" if r.signed else "")
        bits = _bits(r)
        if r.elements:
            bits = (bits + "; " if bits else "") + ", ".join(r.elements)
        read = _read(r) + (f" (consistency: {r.snapshot})" if r.snapshot else "")
        h = host[r.host] + (f": {r.host_note}" if r.host_note else "")
        rows.append("| " + " | ".join(_cell(c) for c in (_addr(r), name, access, _reset(r), bits or "—", read,
                                                         _write(r), h)) + " |")
    return "\n".join(rows)


def replace_section(text, table):
    """`text` with the lines between `BEGIN` and `END` replaced by `table`."""
    try:
        i = text.index(BEGIN) + len(BEGIN)
        j = text.index(END, i)
    except ValueError as e:
        raise ValueError(f"no {BEGIN!r} ... {END!r} section") from e
    return text[:i] + "\n" + table + "\n" + text[j:]


def section(text):
    """The table between the markers."""
    i = text.index(BEGIN) + len(BEGIN)
    return text[i:text.index(END, i)].strip("\n")
