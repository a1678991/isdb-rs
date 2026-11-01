[English](README.md)

# isdb-rs

> この文書は [README.md](README.md) の日本語訳です。リンク先のドキュメントは英語版のみで、内容に相違がある場合は英語版が優先します。

**日本のデジタル放送 — ISDB-T・ISDB-S・ISDB-S3 — を、規格書から Rust で実装したプロジェクトです。** サービスの記述からトランスポートストリーム（TS）を作り、IQ に変調して有線の試験環境で送信できます。地上波・衛星の信号を受信して TS や TLV ストリームに戻し、得られたものを表示・検証・比較・編集できます。

送信と受信の両方向が同じライブラリの上で動くため、各層をその逆変換、規格書に記載された数値、そして独立した受信機（USB チューナー、テレビ録画機、GNU Radio の gr-isdbt）と突き合わせて検証しています。受信機の試験、プロトコルの学習、研究室での放送技術の実験を目的としています。

```text
 サービス (TOML) ─▶ isdb mux ─▶ TS ─▶ isdb tx ─▶ IQ ファイル / 無線機
                                                    │
 解析 ◀─ isdb ts ◀─ TS / TLV ◀─ isdb rx ◀───────────┘
```

> [!WARNING]
> **法令および電波安全に関する注意**
>
> - **送信には免許が必要です。** 放送用（UHF テレビ）の周波数で電波を発射するには無線局の免許が必要です。日本では電波法、その他の国・地域ではそれぞれの法令に基づきます。免許を受けずに送信することは違法であり、付近のすべての人のテレビ受信に妨害を与えるおそれがあります。
> - **有線接続またはシールド環境でのみ使用してください。** `isdb tx --radio` は、電波を一切空間に放射しない試験環境での使用を想定しています。すなわち、無線機から減衰器（アッテネータ）を経由して受信機へ直接つなぐ同軸ケーブルによる接続、またはシールドボックスです。アンテナは接続しません。電波を発射するのは `--rf-enable` を指定した場合のみです。`--rf-enable` も `--dry-run` も指定しない `--radio` は拒否され、`--dry-run` は RF 設定を表示してデバイスを開く前に終了します。地上波の中心周波数は、`--freq-unrestricted` を指定しない限り UHF テレビ帯の 470〜710 MHz（13〜52 ch）に制限されます。[docs/transmitting.md](docs/transmitting.md) を参照してください。
> - **スクランブル解除は行いません。** B-CAS／ACAS／限定受信方式（CAS）には対応しておらず、本ソフトウェアがスクランブルを解除することはありません。スクランブルされた入力は識別したうえで拒否します（受信機では、数えたうえでそのまま出力します）。中継する番組は、既定で元のコピー制御の信号と CA_descriptor／ECM を保持します。
> - **コンテンツの権利。** 送信または中継するコンテンツ（録画物を含む）について必要な権利を有していることは、利用者の責任です。放送事業者の番組の再送信は、同軸ケーブルを用いた私的なものであっても、お住まいの地域では制限されている場合があります。
> - 本ソフトウェアは Apache License 2.0 の下で**無保証**で提供されます（[LICENSE](LICENSE) を参照）。
> - **提携関係はありません。** isdb-rs は独立したプロジェクトです。ARIB、ITU、ISO/IEC、Great Scott Gadgets、Analog Devices、OpenSourceSDRLab、Sony、MyGica およびいかなる放送事業者とも提携関係になく、これらから承認や後援を受けてもいません。これらの名称、および HackRF、ADALM-Pluto、PlutoSky、PIX-DT295 や BDZ などの製品名は、実装した規格と試験に用いた機器を識別する目的でのみ使用しています。これらはそれぞれの権利者の商標である場合があります（HackRF は Great Scott Gadgets の、ADALM-Pluto は Analog Devices の商標です）。

## できること

| | 送信（生成） | 受信（復号） |
|---|---|---|
| **ISDB-T** — 地上波、ARIB STD-B31 | ✅ サービス → TS → OFDM の IQ → HackRF / Pluto。全モード、全ガードインターバル、部分受信を含む最大 3 階層 | ✅ IQ ファイルまたは無線機 → 階層ごとの TS（TMCC/AC と状態表示つき） |
| **ISDB-S** — BS／110 度 CS、ITU-R BO.1408 | ✅ 最大 8 本の TS → TC8PSK/QPSK/BPSK の IQ。無線機へは PlutoSky R2 の FPGA 経由 | ✅ IF の録音または無線機 → TS_ID ごとの TS |
| **ISDB-S3** — 新 4K8K 衛星放送、ARIB STD-B44 | — | ✅ IF の録音または無線機 → ストリーム ID ごとの TLV ストリーム。MMT-SI を復号し、スクランブルされていないメディアを TS / MKV に再多重化 |

物理層のまわりには次のものがあります。

- **トランスポートストリームと SI** — PSI と ARIB SI を、すべてのテーブルと記述子でロスレス（`encode(decode(x)) == x`）な型付きコーデックとして実装。ARIB 8 単位符号の文字列、各指摘に根拠条項を示す検証器、フィールド単位の差分、TS の編集。
- **サービス多重化器** — ネットワーク・サービス・イベントの TOML 記述と MPEG-2 映像／AAC ファイルから、伝送フレームに合わせて送出される一定レートの多重化ストリームを作ります。スクランブルされていない録画の 1 番組の中継、BML データカルーセルと Hybridcast の AIT、ISDB-S 用のプロセス内多重化にも対応します。
- **新 4K8K 衛星放送の TLV／MMT** — TLV パケット、ヘッダ圧縮 IP、MMTP と MMT-SI テーブル（サービス、イベント、パッケージ、アセット）。スクランブルされていない各アセットのエレメンタリーストリームを取り出します。
- **新 4K8K 衛星放送から再生できるファイルへ** — `isdb-remux` は、スクランブルされていない TLV ストリームを、すべての音声トラック、字幕（SubRip／WebVTT。WebVTT で表せる範囲で放送のレイアウトと色を保持）、データ放送のパッケージを含む、タイムスタンプつきの MPEG-TS または Matroska ファイルにします。
- **SDR の入出力** — HackRF One と ADALM-Pluto 系（USB または Ethernet）、SigMF メタデータつきの IQ 録音、シンボルレート・搬送波・方式を見つける衛星 IF の測定（survey）、PlutoSky R2 用の FPGA シンボル・フロントエンド（Amaranth HDL。CPU 上のモデルとビット単位で一致）。

## クイックスタート

Linux、Rust 1.88 以上。既定のビルドは、無線機をつながない場合でも、無線機のバックエンド用にシステムの `libhackrf` と `libiio` をリンクします。

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

無線機のライブラリがない場合は、`cargo build --release -p isdb-cli --no-default-features` でファイルを扱う機能をすべてビルドできます（片方のバックエンドだけ使うなら `--features hackrf` または `--features pluto` を追加）。

**ハードウェアもデータもなしで試す** — 試験用パケットを変調し、雑音と周波数オフセットを加えて復調し、すべてのパケットを比較します。

```sh
isdb loopback --synthetic                  # 最後に "[loopback] PASS" と表示
isdb loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3
```

**放送を作って受信し直す。** メディアファイルを使わない小さなサービス記述です。

```sh
cat > test.toml <<'EOF'
[network]
network_id = 0x79EE            # 仮の値: お住まいの地域で使われていない ID を選んでください
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
  kind = "declared"          # PMT に記載するだけで、メディアは不要
  pid = 0x0111
  stream_type = 0x02
  component_tag = 0x00

  [[service.event]]
  event_id = 0x0001
  start_offset_s = -600
  duration = "01:00:00"
  name = "試験放送"
EOF

isdb mux -c test.toml --seconds 5 --out-ts test.ts   # トランスポートストリーム
isdb ts inspect test.ts                    # PID、PCR、番組、復号した SI
isdb ts validate test.ts                   # エラー 0（警告 1: 宣言したストリームにパケットがない）

isdb tx --mux-config test.toml --iq test.cf32 --seconds 3   # 変調（cf32 で約 195 MB）
isdb rx --iq test.cf32 --out rx.ts --json  # 復調
isdb ts diff test.ts rx.ts                 # すべてのセクションが一致
```

`isdb mux --print-config-example` は、映像と音声を含む注釈つきの設定例を表示します。この先は [docs/getting-started.md](docs/getting-started.md) が詳しい手引きです。

## コマンド

バイナリは `isdb` ひとつと、`isdb-remux` です。

| コマンド | 内容 |
|---|---|
| `isdb mux` | サービス記述（TOML）とメディア → トランスポートストリーム |
| `isdb tx` | TS → ISDB-T または ISDB-S → IQ ファイルまたは無線機 |
| `isdb rx` | IQ ファイルまたは無線機 → ISDB-T／ISDB-S の TS、または ISDB-S3 の TLV ストリーム |
| `isdb loopback` | 送信 → 雑音・オフセット・エコー → 受信、すべてのパケットを確認 |
| `isdb ts` | トランスポートストリームの `inspect`、`validate`、`diff`、`edit`、`extract` |
| `isdb tlv` | TLV ストリーム（新 4K8K 衛星放送）の `inspect`、`validate`、`extract` |
| `isdb capture` | 無線機からの IQ の録音（受信のみ、SigMF メタデータつき） |
| `isdb iq` | 衛星の録音: `survey`、`symbols`、`tmcc`、`s3sync`、`s3tmcc` |
| `isdb symbols` | 無線機の FPGA フロントエンド、または CPU のモデルによる衛星のシンボル |
| `isdb completions` | シェル補完スクリプト |
| `isdb-remux` | スクランブルされていない新 4K8K 衛星放送の TLV ストリーム → タイムスタンプつき MPEG-TS または Matroska（字幕つき） |

どのコマンドも `--help` があります。バイナリ出力が誤って対話端末に出力されることはありません。`-o FILE` を指定するか、パイプでつなぐか、`-o -` で明示的に指定してください。

## ドキュメント（英語）

| | |
|---|---|
| [Getting started](docs/getting-started.md) | 最初の詳しい手引き |
| [Transmitting](docs/transmitting.md) | 試験環境の構成、無線機、録画の中継、ISDB-S |
| [Receiving](docs/receiving.md) | ファイルと無線機からの ISDB-T・ISDB-S・ISDB-S3 |
| [Analysis](docs/analysis.md) | inspect、validate、diff、edit |
| [Multiplex configuration](docs/mux-config.md) | TOML 記述の全フィールド |
| [Architecture](docs/architecture.md) | クレートの関係とデータの流れ |
| [Glossary](docs/glossary.md) | 放送の略語集 |
| [Test data](docs/test-data.md) | 手持ちのキャプチャを使うオプトインの試験 |
| [FPGA](fpga/README.md) | PlutoSky R2 のシンボル処理ブロック |
| [VERIFICATION.md](VERIFICATION.md) | 何をどう検証したか、未解決の問題 |
| [SPECS.md](SPECS.md) | 規格書とその版、入手先 |

## クレート

ワークスペースは、それぞれ単独でも使えるライブラリクレートの集まりです（各 README へのリンク）。

| 層 | クレート |
|---|---|
| トランスポートストリームと SI | [`isdb-ts`](crates/isdb-ts/README.md)（H.222.0 の TS、PES、PSI）・[`isdb-si`](crates/isdb-si/README.md)（ARIB SI、データカルーセル、AIT）・[`isdb-text`](crates/isdb-text/README.md)（ARIB 8 単位符号）・[`isdb-analyze`](crates/isdb-analyze/README.md)（表示、検証、差分、編集） |
| 多重化 | [`isdbt-mux`](crates/isdbt-mux/README.md)（サービス → TS、中継、データ放送、ISDB-S のスケジュール） |
| 誤り訂正符号 | [`isdb-fec`](crates/isdb-fec/README.md)（各方式で共通の RS、畳み込み符号／ビタビ、PRBS） |
| ISDB-T | [`isdbt-core`](crates/isdbt-core/README.md)・[`isdbt-fec`](crates/isdbt-fec/README.md)・[`isdbt-ofdm`](crates/isdbt-ofdm/README.md)・[`isdbt-phy`](crates/isdbt-phy/README.md) |
| ISDB-S | [`isdbs-core`](crates/isdbs-core/README.md)・[`isdbs-phy`](crates/isdbs-phy/README.md) |
| ISDB-S3 と MMT | [`isdbs3-core`](crates/isdbs3-core/README.md)・[`isdbs3-phy`](crates/isdbs3-phy/README.md)・[`isdb-tlv`](crates/isdb-tlv/README.md)（TLV、IP、MMTP）・[`isdb-mmt`](crates/isdb-mmt/README.md)（MMT-SI、MPU/MFU、エレメンタリーストリーム） |
| メディアファイル | [`isdb-remux`](crates/isdb-remux/README.md)（タイムスタンプつき TS／MKV）・[`isdb-mkv`](crates/isdb-mkv/README.md)（Matroska の書き出し）・[`isdb-caption`](crates/isdb-caption/README.md)（ARIB TTML → SRT／WebVTT） |
| 衛星のフロントエンドと IQ | [`isdb-frontend`](crates/isdb-frontend/README.md)（IQ ⇄ シンボル、FPGA のモデル）・[`isdb-iq`](crates/isdb-iq/README.md)（録音、SigMF、survey） |
| 無線機 | [`isdb-radio`](crates/isdb-radio/README.md)（デバイス非依存）・[`isdb-hackrf`](crates/isdb-hackrf/README.md)・[`isdb-iio`](crates/isdb-iio/README.md)（Pluto） |
| コマンドライン | [`isdb-cli`](crates/isdb-cli/README.md)（`isdb` バイナリ） |

## 検証の方法

唯一の正解は規格書です。ほかの ISDB／ARIB 実装からコードやテーブルは取り込んでおらず、それらは独立した比較対象（オラクル）としてのみ使います。各層は、実装自身を信用せずに次の方法で確認しています。

1. **規格書の数値** — 規格書に記載された数値（チェック値、表、PRBS の状態、フレーム長）を、表の名前をつけた試験で確認。
2. **ロスレスな往復** — 復号器はすべてのビットを保持し、`encode(decode(x)) == x` をプロパティテストで確認。手持ちのキャプチャがあればそれにも適用。
3. **逆変換の組** — 送信の各段を受信側の逆変換と、全体を雑音・搬送波オフセット・タイミングオフセット・エコーを通して、バイト単位で一致することを確認。
4. **差分オラクル** — ISDB-T の送信と受信は GNU Radio の gr-isdbt と双方向で相互に復号でき、見つかった差異はすべて gr-isdbt 側の規格からの逸脱。
5. **実機の受信機と実際の放送** — 同軸接続で、MyGica S270 チューナーは本プロジェクトの ISDB-T をパケット誤りゼロで復号し、Sony の BDZ 録画機はサービスを再生しました。PIX-DT295 は本プロジェクトの ISDB-S の送信に同期して正常に録画しました。受信側は、ケーブル配信の地上波・BS・110 度 CS・新 4K8K 衛星放送を復号します。

規格書の記述があいまいな箇所では、採用した解釈を原文の引用とともに各クレートの `SPEC-NOTES.md`（[索引](SPEC-NOTES.md)）に記録しています。結果と再現手順は [VERIFICATION.md](VERIFICATION.md) にあります。

## 現状と制約

- **試験した構成。** 実機の ISDB-T 受信機で確認したのは 1 つの構成（モード 3、GI 1/8、部分受信、1 セグメント QPSK 2/3 + 12 セグメント 64QAM 3/4）だけです。ほかの構成はオラクルとループバックで検証しています。
- **ループバックのみで確認しているもの。** DQPSK、AC の内容、緊急警報放送の起動フラグ、パラメータ切り替え（送出している局がないため）。
- **運用上の値。** ARIB TR-B14 を入手できないため、PID の割り当てや SI の配置などの値は文書化した選択です（[一覧](crates/isdbt-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers)）。
- **メディア。** 多重化器が扱うのは MPEG-2 映像と AAC ADTS です。AAC はヘッダの水準でのみ確認しています。H.264 のワンセグは PMT に宣言できますが、パケット化はしません。
- **データ放送**が実機の受信機で表示されるのは、登録された放送事業者のネットワーク ID とサービス ID を使った場合だけです。Hybridcast に対応した受信機では試験していません。
- **速度。** 実時間の受信には高速なマシンが必要です。ISDB-S3 の LDPC は多数のコアを使い、ISDB-T の復調器は最近の 16 コアのデスクトップで実時間の約 1.4 倍の速度です。実時間の処理には必ず `--release` ビルドを使ってください。
- **無線機。** HackRF One と ADALM-Pluto 系に対応しています。Pluto は一度に一方向しかストリーミングできません。ISDB-S と ISDB-S3 のレートを Ethernet で連続してストリーミングするには、PlutoSky R2 の FPGA ブロックが必要です。
- **未実装。** ISDB-S3 の送信、SFN 同期、放送 TS への AC データの挿入、DRCS（外字）、ISDB-T<sub>SB</sub>。

## 開発

[mise](https://mise.jdx.dev/) ですべての確認を実行します。CI と同じタスクです（[mise.toml](mise.toml)）。

```sh
mise run check      # CI が実行するすべて: lint、test、doc、deny、msrv
mise run test       # 単体・結合・ドキュメントの試験。無線機もデータも不要
mise run lint       # rustfmt + clippy -D warnings
```

重い試験は `#[ignore]` にしてあり（`mise run test-ignored`）、実際のキャプチャを使う試験は `ISDB_RS_*` 環境変数によるオプトインです（[docs/test-data.md](docs/test-data.md)）。規則（規格書を唯一の正解とすること、SPEC-NOTES の記録、ハードウェア試験での電波安全）とコミットの書式は [CONTRIBUTING.md](CONTRIBUTING.md) にあります。セキュリティ上の問題は [SECURITY.md](SECURITY.md)、変更履歴は [CHANGELOG.md](CHANGELOG.md) を参照してください。

## 規格書

実装は ITU-T H.222.0 | ISO/IEC 13818-1、ARIB STD-B10、B24、B31、B32、B44、B60、ITU-R BO.1408-1、2011 年総務省令第 87 号に従っています。規格書の PDF は著作権があるため再配布していません。ITU-T の勧告と ARIB 規格の英訳は無料で入手できます。[SPECS.md](SPECS.md) には、使用した版、その SHA-256 ハッシュと入手先、各クレートが実装する条項、それぞれの確認方法をまとめています。

## ライセンス

Apache License 2.0 の下でライセンスされています（[LICENSE](LICENSE)、[NOTICE](NOTICE) も参照）。明示的に別段の意思表示をしない限り、本プロジェクトへの取り込みを意図して提出された貢献は、追加の条件なしに上記のとおりライセンスされるものとします。
