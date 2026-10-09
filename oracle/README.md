# oracle: differential test against gr-isdbt

Scripts that cross-check the Rust ISDB-T PHY against GNU Radio
[gr-isdbt](https://github.com/git-artes/gr-isdbt), an independent
implementation used as an oracle: gr-isdbt decodes the Rust transmitter's
output, the Rust receiver decodes gr-isdbt's output, and the carriers of
the two modulators are compared. A disagreement is settled by reading ARIB
STD-B31, not by copying either side. Results and the deviations found are
in [VERIFICATION.md](../VERIFICATION.md).

| File | Purpose |
|---|---|
| `Dockerfile` | the oracle container: Ubuntu 24.04, GNU Radio 3.10, gr-osmosdr, gr-isdbt built from upstream at a pinned commit |
| `run_matrix.py` | the whole matrix (8 parameter sets), run on the host |
| `grisdbt_tx.py` / `grisdbt_rx.py` | gr-isdbt transmitter / receiver flowgraphs, run inside the container |
| `ts_compare.py` | anchors decoded packets on a unique source packet, then compares them in sequence |
| `iq_compare.py` | carrier-level comparison of two cf32 files, with DBPSK bit attribution for TMCC/AC |
| `rust_tx3/` | stand-alone crate (not a workspace member): `rust_tx3` (three-layer `tx_file`) and `rx_rot` (rotation-numbering check) |

Quick run, from the repository root:

```sh
docker build -t isdb-oracle oracle/
docker run -d --name isdb-oracle --user "$(id -u):$(id -g)" -v "$PWD":/work isdb-oracle
cargo build --release -p isdbt-phy --examples
cargo build --release --manifest-path oracle/rust_tx3/Cargo.toml --target-dir target/oracle
python3 oracle/run_matrix.py               # or one row: python3 oracle/run_matrix.py m3g4p_bench
```

Details, environment variables (`ISDB_RS_ORACLE_CONTAINER`,
`ISDB_RS_ORACLE_OUT`) and the per-step commands are in
[docs/test-data.md](../docs/test-data.md#the-gr-isdbt-oracle) and
[VERIFICATION.md](../VERIFICATION.md#reproduction).

## Licensing

The files in this directory are part of isdb-rs and licensed under the
Apache License 2.0, like the rest of the repository. GNU Radio and gr-isdbt
are licensed under GPL-3.0 and are **not** included or distributed here:
the Dockerfile downloads and builds them, and `grisdbt_tx.py` /
`grisdbt_rx.py` import them at run time. The program that runs when those
two scripts execute (script plus GNU Radio plus gr-isdbt) is therefore a
combined work under GPL-3.0; if you redistribute such a combination, for
example a built container image, GPL-3.0 applies to it. The other scripts
and `rust_tx3/` do not use GPL code. No gr-isdbt code has been copied into
this repository.
