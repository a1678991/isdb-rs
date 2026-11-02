"""GF(2^16) and the B44 BCH code (t = 12) in Python, as `isdb_fec::bch`
defines them: the field from x^16 + x^12 + x^3 + x + 1 (primitive element
alpha = x), the syndromes S_i = r(alpha^i) of the word read as r(x) = sum
word[j] x^(n-1-j), Berlekamp-Massey and the Chien search over the n
positions present, with `decode` a line-by-line port of
`isdb_fec::bch::decode` (the HDL's reference in the unit tests; the
chain's vectors carry no BCH correction). Also the constant-multiplier
matrices the HDL elaborates its XOR networks from.
"""

POLY = (1 << 16) | (1 << 12) | (1 << 3) | (1 << 1) | 1
Q1 = (1 << 16) - 1
T = 12
PARITY = 192

_EXP = [0] * (2 * Q1)
_LOG = [0] * (Q1 + 1)
_v = 1
for _i in range(Q1):
    _EXP[_i] = _v
    _LOG[_v] = _i
    _v <<= 1
    if _v & (1 << 16):
        _v ^= POLY
for _i in range(Q1, 2 * Q1):
    _EXP[_i] = _EXP[_i - Q1]


def mul(a, b):
    if a == 0 or b == 0:
        return 0
    return _EXP[_LOG[a] + _LOG[b]]


def inv(a):
    return _EXP[(Q1 - _LOG[a]) % Q1]


def alpha(e):
    return _EXP[e % Q1]


def matrix(c):
    """The 16 x 16 GF(2) matrix of multiplication by the constant c: column
    k = c * x^k (as integers: entry k is the image of bit k)."""
    return [mul(c, 1 << k) for k in range(16)]


def square_matrix():
    return [mul(1 << k, 1 << k) for k in range(16)]


def syndromes(word):
    n = len(word)
    s = [0] * (2 * T + 1)
    for j, b in enumerate(word):
        if b & 1:
            p = n - 1 - j
            for i in range(1, 2 * T + 1):
                s[i] ^= alpha(p * i)
    return s


def decode(word):
    """`isdb_fec::bch::decode`: returns (result, word after), result the
    number corrected or None (uncorrectable); the word is modified as the
    Rust function modifies it (the Chien roots flipped even when it then
    reports a failure)."""
    w = list(word)
    n = len(w)
    s = syndromes(w)
    if not any(s[1:]):
        return 0, w
    c = [0] * (2 * T + 2)
    bp = [0] * (2 * T + 2)
    c[0] = bp[0] = 1
    ll, m, b = 0, 1, 1
    for k in range(2 * T):
        d = s[k + 1]
        for i in range(1, ll + 1):
            d ^= mul(c[i], s[k + 1 - i])
        if d == 0:
            m += 1
            continue
        coef = mul(d, inv(b))
        t = list(c)
        for i in range(len(c) - m):
            c[i + m] ^= mul(coef, bp[i])
        if 2 * ll <= k:
            ll = k + 1 - ll
            bp = t
            b = d
            m = 1
        else:
            m += 1
    if ll > T:
        return None, w
    fixed = 0
    for j in range(n):
        p = n - 1 - j
        xinv = (Q1 - p % Q1) % Q1
        v = 0
        for i in range(ll + 1):
            if c[i]:
                v ^= mul(c[i], alpha(xinv * i))
        if v == 0:
            w[j] ^= 1
            fixed += 1
    if fixed != ll:
        return None, w
    return fixed, w


TABLE_3_4_1 = [
    [0, 1, 3, 12, 16], [0, 2, 3, 4, 8, 9, 11, 12, 16], [0, 2, 3, 7, 9, 10, 11, 13, 16],
    [0, 1, 3, 6, 7, 11, 12, 13, 16], [0, 1, 2, 3, 5, 7, 8, 9, 11, 13, 16],
    [0, 1, 6, 7, 9, 10, 12, 13, 16], [0, 1, 2, 6, 9, 10, 11, 15, 16],
    [0, 1, 3, 6, 8, 9, 12, 15, 16], [0, 1, 4, 6, 8, 10, 11, 12, 13, 15, 16],
    [0, 1, 2, 4, 6, 8, 9, 10, 11, 15, 16], [0, 6, 8, 9, 10, 13, 14, 15, 16],
    [0, 1, 2, 3, 5, 6, 7, 10, 11, 15, 16],
]


def generator():
    g = [1]
    for f in TABLE_3_4_1:
        fp = [0] * 17
        for e in f:
            fp[e] = 1
        out = [0] * (len(g) + 16)
        for i, a in enumerate(g):
            if a:
                for j, bb in enumerate(fp):
                    out[i + j] ^= bb
        g = out
    return g


def parity(msg):
    """`isdb_fec::bch::parity`."""
    g = generator()
    r = [0] * PARITY
    for bit in msg:
        fb = (bit & 1) ^ r[0]
        r = r[1:] + [0]
        if fb:
            for i in range(PARITY):
                r[i] ^= g[PARITY - 1 - i]
    return r


def encode(msg):
    return list(msg) + parity(msg)
