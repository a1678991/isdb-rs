[English](README.md)

# isdb-rs

> この文書は [README.md](README.md) の日本語訳です。リンク先のドキュメントは英語版のみで、内容に相違がある場合は英語版が優先します。

規格書に基づいてゼロから書かれた、日本の地上デジタルテレビ放送（ISDB-T）の Rust 実装です。MPEG-2 トランスポートストリームと ARIB の番組配列情報（SI）のコーデック、サービス多重化器、トランスポートストリーム解析器、ARIB STD-B31 の変調器と復調器を備え、SDR による入出力（HackRF One と ADALM-Pluto 系）に対応します。送信と受信の両方向が同じライブラリの上で動作します（サービス → TS → OFDM ベースバンド → 無線機、および無線機 → OFDM → TS → 解析）。そのため各層を、その逆変換、規格書自体に記載された数値、そして独立した受信機と突き合わせて検証できます。有線接続の試験環境（ベンチ）での受信機試験、プロトコルの学習、放送技術の実験を目的としています。ISDB-T は送受信の全体を実装しています。ISDB-S は衛星 IF の録音をフレーム同期と TMCC まで復調できます（`isdb iq tmcc`）が、TS パケットまではまだです。ISDB-S3 についてはキャプチャ（`isdb capture`）と録音の解析（`isdb iq survey`）があります。

> [!WARNING]
> **法令および電波安全に関する注意**
>
> - **送信には免許が必要です。** 放送用（UHF テレビ）の周波数で電波を発射するには無線局の免許が必要です。日本では電波法、その他の国・地域ではそれぞれの法令に基づきます。免許を受けずに送信することは違法であり、付近のすべての人のテレビ受信に妨害を与えるおそれがあります。
> - **有線接続またはシールド環境でのみ使用してください。** `isdb tx --radio` は、電波を一切空間に放射しない試験環境での使用を想定しています。すなわち、無線機から減衰器（アッテネータ）を経由して受信機へ直接つなぐ同軸ケーブルによる接続、またはシールドボックスです。アンテナは接続しません。電波を発射するのは `--rf-enable` を指定した場合のみです。`--rf-enable` も `--dry-run` も指定しない `--radio` は拒否され、`--dry-run` は RF 設定を表示してデバイスを開く前に終了します。中心周波数は、`--freq-unrestricted` を指定しない限り UHF テレビ帯の 470〜710 MHz（13〜52 ch）に制限されます。[docs/transmitting.md](docs/transmitting.md) を参照してください。
> - **スクランブル解除は行いません。** B-CAS／限定受信方式（CAS）には対応しておらず、本ツールがスクランブルを解除することはありません。入力はスクランブルされていないもの（transport_scrambling_control '00'）でなければならず、スクランブルされた入力は拒否します。中継する番組は、既定で元のコピー制御の信号と CA_descriptor／ECM を保持します。
> - **コンテンツの権利。** 送信または中継するコンテンツ（録画物を含む）について必要な権利を有していることは、利用者の責任です。放送事業者の番組の再送信は、同軸ケーブルを用いた私的なものであっても、お住まいの地域では制限されている場合があります。
> - 本ソフトウェアは Apache License 2.0 の下で**無保証**で提供されます（[LICENSE](LICENSE) を参照）。
> - **提携関係はありません。** isdb-rs は独立したプロジェクトです。ARIB、ITU、ISO/IEC、Great Scott Gadgets、Analog Devices、OpenSourceSDRLab、Sony、MyGica およびいかなる放送事業者とも提携関係になく、これらから承認や後援を受けてもいません。これらの名称、および HackRF、ADALM-Pluto、PlutoSky や BDZ などの製品名は、実装した規格と試験に用いた機器を識別する目的でのみ使用しています。これらはそれぞれの権利者の商標である場合があります（HackRF は Great Scott Gadgets の、ADALM-Pluto は Analog Devices の商標です）。

## 機能

- **TS と PSI**（`isdb-ts`、ITU-T H.222.0）：パケット、アダプテーションフィールド、PCR、PES、セクションの組み立てとパケット化、PAT/CAT/PMT、連続性カウンタの追跡、188／192／204 バイトパケットに対応した再同期機能付きのリーダ。
- **ARIB SI**（`isdb-si`、ARIB STD-B10）：NIT、BAT、SDT、EIT、TDT、TOT、RST、ST、PCAT、BIT、NBIT、LDT と B10 の記述子を、型付きの*ロスレス*なコーデックとして実装（予約ビットもすべて保持するため `encode(decode(x)) == x` が成り立ちます）。MJD/BCD 時刻、および独立した規格適合性チェッカ。
- **ARIB 文字符号**（`isdb-text`、ARIB STD-B24）：SI 文字列の 8 単位符号。Unicode への復号（表現できない符号はすべて報告）と、逆方向の符号化。
- **多重化器**（`isdbt-mux`）：TOML によるサービス記述（ネットワーク、サービス、イベント、SI テーブル、実験用の上書き設定）と MPEG-2 映像および AAC ADTS ファイルから、OFDM フレームに合わせて送出される固定レートの多重を生成します。H.222.0 の T-STD バッファモデルを備えます。**TS 中継**：スクランブルされていない録画 TS（たとえば自分で作成したもの）の 1 番組を、独自の PSI/SI を持つ地上デジタル放送のサービスとして再送信します。ストリームの役割と PID は録画から検出され、元のコピー制御と限定受信の信号は既定で保持されます。**データ放送とハイブリッドキャスト**：ディレクトリ内のファイルを BML データカルーセル（DSM-CC の DII／DDB、ARIB STD-B24 第 2・3 編）として PMT／EIT の信号とともに送出し、ハイブリッドキャスト受信機が起動する HTML5 アプリケーションの URL を示す AIT（B24 第 4 編、IPTVFJ STD-0010／0013）を、生成したランチャーとともにカルーセル内で、または独立した PID で伝送します。
- **解析器**（`isdb-analyze`）：内容の表示（inspect）、検証（validate。各指摘には根拠となる条項を明記）、2 つのキャプチャのテーブル／フィールド単位の差分、および TS の正当性を保ったままの編集（PID のヌル化、セクションの削除・置換・挿入、型付きのフィールド変更）。
- **ISDB-T 物理層**（`isdbt-core`、`isdbt-fec`、`isdbt-ofdm`、`isdbt-phy`、ARIB STD-B31）：モード 1〜3、すべてのガードインターバル、部分受信を含む最大 3 階層の階層伝送、DQPSK／QPSK／16QAM／64QAM、すべての符号化率と時間インターリーブ長、TMCC と AC。受信側は同期、CFO（キャリア周波数オフセット）とタイミングドリフトの補正、伝送路推定、TMCC 復号、軟判定ビタビ復号と RS 復号を行い、13 セグメントの TS を再生します。
- **入出力**（`isdb-radio`、`isdb-hackrf`、`isdb-iio`、`isdb-cli`）：IQ ファイル（GNU Radio 向けの cf32、`hackrf_transfer` 向けの cs8）と、デバイス非依存の IQ シンク／ソーストレイトを介した無線機とのストリーミング（デバイスごとに 1 つのバックエンド。HackRF One は `--radio hackrf`、ADALM-Pluto とその互換機（USB または Ethernet 接続の PlutoSky R2 など）は `--radio pluto`）。`tx`、`rx`、`loopback`、`mux`、`ts`、`capture`、`iq`、`symbols`、`completions` の各コマンドを持つ単一の `isdb` バイナリ。
- **衛星 IF の録音**（`isdb-iq`、`isdb capture`、`isdb iq survey`）：無線機が対応する任意のサンプルレートでの受信専用キャプチャ。一括で連続した録音（Pluto では libiio 経由で 16 ビット最大 1650 万サンプル、Tezuka の maia-sdr レコーダーで 16 ビット 3355 万サンプル、8 ビット 6710 万サンプル＝40 Msps で 1.7 秒）と、欠落箇所をすべて記録するストリーミング録音（Gigabit Ethernet で欠落なしは約 40 MB/s まで、例えば 8 ビット 20 Msps）、SigMF メタデータ、BS／110 度 CS のチャンネル名による IF 周波数指定、録音からシンボルレート・搬送波オフセット・方式（ISDB-S、ISDB-S3）を求める解析。
- **衛星のシンボル・フロントエンド**（`isdb-frontend`、`fpga/`、`isdb symbols`、`isdb iq symbols`）：IQ から 5 または 6 ビットのシンボルへの整合フィルタ、タイミング再生、AGC。CPU 上のビット精度の固定小数点モデルと、PlutoSky R2 の FPGA 向けの同じ処理（Amaranth HDL）の両方があり、FPGA 側はリンクを流れるデータ量を約半分（80 MB/s から約 43 MB/s）にして ISDB-S／ISDB-S3 の連続受信を可能にします。HDL はシミュレーションでモデルとビット単位で一致しますが、まだビットストリームには組み込んでいません。それまでは CPU が 1 回の連続キャプチャに対して処理します。
- **ISDB-S のフレーム同期と TMCC**（`isdbs-core`、`isdbs-phy`、`isdb iq tmcc`）：フレーム内の既知シンボルによるフレーム・スーパーフレーム同期、バースト信号を用いた搬送波再生、TMCC（内符号、エネルギー拡散、RS(64,48)）の復号により、変調・符号化方式の組み合わせ、スロット、各スロットの TS_ID を得ます。ケーブル配信の BS 9 トランスポンダと 110 度 CS 12 トランスポンダのすべてのスーパーフレームで復号できました。

## クレート

| クレート | 規格 | 範囲 |
|---|---|---|
| [`isdb-ts`](crates/isdb-ts/README.md) | H.222.0 | ビット I/O、CRC_32、TS パケット／アダプテーションフィールド／PCR、PES、セクションの組み立てとパケット化、PAT/PMT/CAT、TS リーダ |
| [`isdb-text`](crates/isdb-text/README.md) | B24 Vol. 1 Part 2 Ch. 7 | ARIB 8 単位符号文字列の復号／符号化 |
| [`isdb-si`](crates/isdb-si/README.md) | B10 | SI テーブル、記述子、MJD/BCD 時刻。ロスレスな型付きコーデックと適合性チェック |
| [`isdb-analyze`](crates/isdb-analyze/README.md) | H.222.0, B10, B32 Part 2 | TS の内容表示、条項を引用した検証、2 つのキャプチャの差分、編集 |
| [`isdbt-mux`](crates/isdbt-mux/README.md) | H.222.0, B10, B31, B32 | サービス多重化器：PSI/SI の生成、MPEG-2 映像／AAC のパケット化、録画番組の中継、OFDM フレームに合わせた出力 |
| [`isdbt-core`](crates/isdbt-core/README.md) | B31 の各表 | モード、ガードインターバル、階層パラメータ、そこから導かれるレートと遅延 |
| [`isdb-fec`](crates/isdb-fec/README.md) | B31 §3.3、§3.5、§3.8；BO.1408-1 §2、§4、§9 | 方式間で共有する通信路符号化の部品：短縮 RS(255,239) 符号、拘束長 7 の畳み込み符号とビタビ復号、PRBS 生成器 |
| [`isdbt-fec`](crates/isdbt-fec/README.md) | B31 §3.2–3.8 | TS 再多重、RS(204,188)、エネルギー拡散、バイトインターリーブ、畳み込み符号／ビタビ復号 |
| [`isdbt-ofdm`](crates/isdbt-ofdm/README.md) | B31 §3.9–3.16 | マッピング、インターリーブ、SP/CP/TMCC/AC を含む OFDM フレーム、IFFT/GI。受信側の同期と等化 |
| [`isdbt-phy`](crates/isdbt-phy/README.md) | B31 | 送信／受信パイプライン（TS パケット ⇄ 複素ベースバンド）、IQ ファイル形式 |
| [`isdb-radio`](crates/isdb-radio/README.md) | — | デバイス非依存の SDR 層：IQ シンク／ソーストレイト、`--radio` によるデバイス選択、バックエンドのレジストリ（HackRF は `hackrf`、ADALM-Pluto は `pluto` フィーチャで有効化）、整数サンプルの量子化器 |
| [`isdb-hackrf`](crates/isdb-hackrf/README.md) | — | システムの libhackrf に対する最小限の安全なラッパー（FFI）。`isdb-radio` の HackRF バックエンドが使用 |
| [`isdb-iio`](crates/isdb-iio/README.md) | — | システムの libiio 0.x に対する最小限の安全なラッパー（FFI）。`isdb-radio` の Pluto バックエンドが使用 |
| [`isdb-iq`](crates/isdb-iq/README.md) | —（ISDB-S／ISDB-S3 のシンボルレートを引用） | 方式に依存しない IQ 録音：サンプル形式、SigMF メタデータ、単一キャリア信号の解析（シンボルレート、搬送波オフセット、帯域、方式） |
| [`isdb-frontend`](crates/isdb-frontend/README.md) | —（レートは `isdb-iq` から） | ISDB-S／ISDB-S3 の単一キャリア・フロントエンド（IQ → シンボル）：FPGA 処理のビット精度の固定小数点モデル、浮動小数点の参照実装、`Symbols` 境界の形式、FPGA の制御レジスタ。HDL は [`fpga/`](fpga/README.md) |
| [`isdbs-core`](crates/isdbs-core/README.md) | BO.1408-1；総務省令第 87 号 別表第四十五号〜第五十一号 | ISDB-S のフレーム構造、変調・符号化方式の組み合わせ、スロット割当て、TMCC 情報のコーデックと信号、バースト |
| [`isdbs-phy`](crates/isdbs-phy/README.md) | BO.1408-1 | フロントエンド以降の ISDB-S 受信処理：フレーム同期、搬送波再生、TMCC 復号、位相補正済みの主信号 |
| [`isdb-cli`](crates/isdb-cli/README.md) | — | `isdb` バイナリ |

各クレートの関係、フレーム／レイテンシの約束事、データの流れについては [docs/architecture.md](docs/architecture.md) を参照してください。

## クイックスタート

Linux 上の Rust 1.88 以上（edition 2024）で動作確認しています。無線機への対応は `isdb-cli` の 2 つの cargo フィーチャ（`isdb-radio` のバックエンドへ転送されます）で、どちらも既定で有効です。`hackrf` はシステムの `libhackrf`（2022.09.1 以降）に、`pluto`（ADALM-Pluto 系、PlutoSky R2 など。USB または Ethernet）はシステムの `libiio`（0.x、0.21 以降）に動的リンクするため、無線機を一切接続しない場合でも、既定のビルドには両方のライブラリのインストールが必要です。

| システム | パッケージ |
|---|---|
| Arch Linux | `hackrf libiio` |
| Debian / Ubuntu | `libhackrf-dev libiio-dev` |
| Fedora | `hackrf-devel libiio-devel` |

```sh
git clone https://github.com/a1678991/isdb-rs && cd isdb-rs
cargo build --release -p isdb-cli          # → target/release/isdb
export PATH="$PWD/target/release:$PATH"
```

どちらのライブラリもない場合は `cargo build --release -p isdb-cli --no-default-features` でビルドしてください（片方だけなら `--features hackrf` または `--features pluto` を追加）。ファイルを扱うコマンド（`tx --iq`、`rx --iq`、`loopback`、`mux`、`ts`）はすべて動作し、組み込まれていないバックエンドを `--radio` で指定するとフィーチャ名を示すメッセージとともに失敗します。

以下はすべて、無線機もキャプチャデータもなしで動作します。

ソフトウェアループバック。決定論的なテストパケットを変調し、再び復調して全パケットを比較します。

```sh
isdb loopback --synthetic                  # ends with "[loopback] PASS"
isdb loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3
```

注釈付きの多重化設定の例を表示します（利用者が用意するメディアファイルを参照します。[docs/mux-config.md](docs/mux-config.md) を参照）。

```sh
isdb mux --print-config-example > bench.toml
```

メディアを使わない設定からトランスポートストリームを生成し、解析します。

```sh
cat > test.toml <<'EOF'
[network]
network_id = 0x79EE            # placeholder: pick an ID unused where you are
name = "テスト"
area_code = 0x5A5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x79EE

[clock]
source = "fixed"
start = "2026-01-01T12:00:00"

[[service]]
service_id = 0x0400
name = "テストHD"
provider = "テスト"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF
copy_control = { recording_control = "copy-free" }

  [[service.stream]]
  kind = "declared"          # listed in the PMT, no media needed
  pid = 0x0111
  stream_type = 0x02
  component_tag = 0x00

  [[service.event]]
  event_id = 0x0001
  start_offset_s = -600
  duration = "01:00:00"
  name = "試験放送"
EOF
isdb mux -c test.toml --seconds 5 --out-ts test.ts
isdb ts inspect test.ts                    # PIDs, PCR, programs, decoded SI
isdb ts validate test.ts                   # 0 errors (one warning: the declared stream has no packets)
```

次に、ファイル上で物理層を通して往復させます。

```sh
isdb tx --mux-config test.toml --iq test.cf32 --seconds 3   # ≈ 195 MB of cf32
isdb rx --iq test.cf32 --out rx.ts --json
isdb ts diff test.ts rx.ts                 # every section identical
```

次のステップ：[docs/getting-started.md](docs/getting-started.md)（より詳しい手順）、[docs/transmitting.md](docs/transmitting.md)（ベンチの構成、無線機、録画の中継）、[docs/receiving.md](docs/receiving.md)、[docs/analysis.md](docs/analysis.md)、[docs/mux-config.md](docs/mux-config.md)、[docs/test-data.md](docs/test-data.md)、[docs/glossary.md](docs/glossary.md)。

## 検証方法

規格書だけが唯一の拠り所です。他の実装をコピーすることはなく、独立したオラクルとしてのみ使い、食い違いは規格書を読んで判断します。他の ISDB／ARIB 実装のコードや表は一切含んでいません。JIS X 0208 → Unicode の対応表は実行時に依存クレート `encoding_rs`（WHATWG index）から取得しており、gr-isdbt のソースは、実装が完成した後に、オラクルとの食い違いを説明する目的でのみ読みました（[VERIFICATION.md](VERIFICATION.md#discrepancies)）。各層は、実装自体を信用することなく検証しています。

1. **規格書の数値ベクトル** — 規格書に記載された数値（チェック値、表、PRBS の状態、コンスタレーション点、フレーム長、TSP 数）を、その表にちなんだ名前のテストで直接アサートしています。
2. **ロスレスな往復変換** — デコーダはすべてのビットを保持し、`encode(decode(x)) == x` をプロパティベーステストで検証するとともに、合成した多重と、任意で手元のキャプチャの全セクションに対しても実行します。
3. **逆変換の対** — 送信側の各段を受信側の対応する逆変換と突き合わせてテストし、物理層全体も 送信 → 劣化（雑音、CFO、タイミングオフセット、エコー） → 受信 の経路でバイト単位の一致を確認しています。
4. **差分オラクル** — Rust の送信側と受信側は、GNU Radio の gr-isdbt と双方向に、8 通りのパラメータセットでバイト単位で一致して相互に復号できます。両者が異なる箇所はすべて、gr-isdbt の B31 からの逸脱です。
5. **実受信機** — 有線接続で、MyGica S270 USB チューナーが Rust の送信信号をパケットエラーなしで復号し、Sony BDZ シリーズのレコーダー 1 台がサービスを登録して再生しました。

結果、再現手順、未解決の課題は [VERIFICATION.md](VERIFICATION.md) にあります。フィールドの*値*（予約ビット、PID ごとに許される table_id、送出頻度）は、ロスレスなコーデックとは分離された検証器でチェックするため、規格に適合しないキャプチャであっても復号・編集・再符号化できます。

## 規格

| 規格 | 用途 |
|---|---|
| ITU-T H.222.0 (05/2006) \| ISO/IEC 13818-1 | トランスポートストリーム、PES、PSI、T-STD |
| ARIB STD-B10 v5.13 | 番組配列情報（SI） |
| ARIB STD-B24 v6.4 Vol. 1 | 8 単位符号 |
| ARIB STD-B31 v2.2 | ISDB-T の伝送方式 |
| ARIB STD-B32 v3.11 Parts 1–3 | 映像・音声符号化の制約、ストリーム形式 |

規格書の PDF はここでは再配布して**いません**（著作権があるため）。ITU-T 勧告と ARIB 標準規格の英語版は無料でダウンロードできます。[SPECS.md](SPECS.md) には、URL、使用した版の SHA-256 ハッシュ、gitignore 対象の `specs/` ディレクトリへ grep 可能なテキストを抽出する方法、各クレートが実装する条項、およびそれぞれの確認方法をまとめています。コードコメント中の条項番号と表番号はこれらの版を指します。本文が曖昧であったり崩れていたりする箇所は、採用した解釈を引用文とともに各クレートの `SPEC-NOTES.md`（[索引](SPEC-NOTES.md)）に記録しています。

## 現状と制限事項

ライブラリと CLI は ISDB-T の生成・送信・受信・解析をエンドツーエンドで行えますが、部分によって独立した検証の程度には差があります。

- **受信機で確認した構成は 1 つだけです。** 実受信機で確認したのは、モード 3、GI 1/8、部分受信、A 階層 1 セグメント QPSK 2/3、B 階層 12 セグメント 64QAM 3/4（時間インターリーブのインデックス 2）の構成のみで、受信機は 2 台です。その他のモードや階層構成は、オラクルとループバックでのみ検証しています。
- **実放送の確認は 1 地域分です。** ケーブル（CATV）経由で 1 地域の 7 局を受信し、全局の TMCC が全フレームでパリティ検査に合格しました。1 局の TMCC 符号語は本プロジェクトの送信機の出力とビット単位で一致し、1 局は両階層とも RS 誤りなしで復号できました（[VERIFICATION.md](VERIFICATION.md#broadcast-reception)）。地震動警報情報の AC を送る局や DQPSK を使う局はなく、これらはループバックでのみ検証しています。
- **DQPSK、AC の内容、緊急警報放送用起動フラグ、パラメータ切り替え**は、自己整合的なループバックでのみ検証しています（gr-isdbt はいずれも実装していません）。
- **ARIB TR-B14**（地上デジタルテレビジョン放送の運用規定）をプロジェクトでは入手できないため、運用上の値（PCR の位置、SI の階層配置、リモコンキー番号の範囲、PID の割り当て、…）は文書化したうえでの選択であり、[isdbt-mux のノート](crates/isdbt-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers) に一覧があります。
- **AAC はヘッダのレベルでのみチェックしています。** ADTS ヘッダは ARIB STD-B32 Part 2 に照らしてチェックします。crc_check の値は検証も挿入もしません。raw_data_block の解析には ISO/IEC 13818-7 が必要ですが、プロジェクトはこれを持っていないためです。規格に適合しない AAC は既定で拒否します。
- **メディア**：MPEG-2 映像と AAC ADTS ファイルのみです。H.264 のワンセグストリームは PMT で宣言できますが、パケット化はしません。
- **データ放送は、登録済み放送局の識別子のもとでのみ受信機で動作します。** カルーセルとその信号は ARIB STD-B24 第 2〜4 編に従い（配置は地上 4 局の実録画と照合）、ソニーのレコーダーは本プロジェクトの BML ページを表示しましたが、それはサービスが地域局自身のネットワーク／サービス ID をその局のチャンネルで名乗っている場合に限られます。プレースホルダのネットワークでは「d」マークは点灯しますが BML ブラウザは起動しません。ハイブリッドキャスト（IPTVFJ STD-0010／0013）は規格書どおりに実装していますが、出典のない値（start_priority ビット、監視対象の AIT モジュール、AIT の data_component_id）は文書化した既定値であり、ハイブリッドキャスト対応受信機での試験はまだなく、受信機が受け付ける organization_id は IPTV フォーラムの運用に依存します。
- **未実装**：同一 PID のパケットを 2 つの階層で重複して送ること、SFN 同期（IIP の `network_synchronization_information` の構文のみ扱います）、放送 TS 上の AC データ、データ放送のデータイベントとイベントメッセージ、DRCS（外字）、ISDB-T<sub>SB</sub>。
- **無線機**：バックエンドは HackRF One と ADALM-Pluto 系です（他のバックエンドは CLI を変更せずに `isdb-radio` へ追加できます）。Pluto は Ethernet 経由で 512/63 MHz の送信または受信を一度に一方向だけ扱えます。送受信を同時に行うと、ADI のファームウェア（有効な CPU は 1 つ）で動く iiod の処理能力を超え、受信側でサンプルが欠落します。PlutoSky R2 の受信には Tezuka ファームウェア v0.3.22 以降が必要です（工場出荷時のファームウェアでは基準発振器の周波数が安定しません）。復調器は現行の 16 コアのデスクトップ CPU で実時間の約 1.4 倍の速さで動作するため、リアルタイム受信には高速なマシンが必要です。

## ロードマップ

- AC の内容とパリティを実放送で確認し、アンテナからの受信を試し、より多くの構成と受信機で試験する。
- 確認できたものから、運用上の選択を TR-B14 の値に置き換える。
- ISO/IEC 13818-7 そのものから書けるようになり次第、AAC の raw_data_block の解析（crc_check の検証）を実装する。H.264 ワンセグのパケット化。
- TS／SI のクレートを再利用した ISDB-S（ITU-R BO.1408／平成 23 年総務省令第 87 号；ARIB STD-B20）および ISDB-S3（ARIB STD-B44）の受信、その後に送信 — 衛星 IF のキャプチャと解析は完了。ISDB-S は TMCC まで復調済みで、次は主信号（TS 出力）。

## 開発

[mise](https://mise.jdx.dev/) はチェック用の唯一のタスクランナーであり（タスクは [mise.toml](mise.toml) に定義）、CI も同じタスクを実行します。

```sh
mise run check                             # everything CI runs: lint, test, doc, deny, msrv, oracle helper
mise run test                              # unit, integration and doc tests; no radio or data needed
mise run lint                              # rustfmt check + clippy -D warnings
mise tasks                                 # all tasks
```

[CONTRIBUTING.md](CONTRIBUTING.md) には、ルール（規格書を拠り所とすること、SPEC-NOTES への記載、ハードウェア試験時の電波安全、中継時のコピー制御と限定受信に関する方針）、ツールチェーン、git フック、コミットの形式をまとめています。コードの構成は [docs/architecture.md](docs/architecture.md) にあります。セキュリティ上の問題は [SECURITY.md](SECURITY.md) の手順で報告してください。変更履歴は [CHANGELOG.md](CHANGELOG.md) にあります。

リアルタイムで動かすものには `--release` ビルドを使ってください。最適化なしでは変調器は実時間の約 0.2 倍の速さでしか動作せず、HackRF がアンダーランします。重いテストは `#[ignore]` 指定されています（`mise run test-ignored`）。実際のキャプチャ、録画、メディアを使うテストは `ISDB_RS_*` 環境変数によるオプトイン方式で、指定がなければ `SKIP` と表示します（[docs/test-data.md](docs/test-data.md)）。

## ライセンス

Apache License, Version 2.0 の下でライセンスされています（[LICENSE](LICENSE)。[NOTICE](NOTICE) も参照してください）。明示的に別段の意思表示をしない限り、本成果物への取り込みを目的として意図的に提出された貢献はすべて、追加の条項や条件なしに上記のとおりライセンスされるものとします。
