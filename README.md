# RB Sampler

Pioneer DDJ-RB のパッドでサンプルを鳴らすだけの軽量アプリ（Tauri 2 + Rust）。
rekordbox を使わずに、DDJ-RB 本体の RCA（マスター出力）から PA ミキサーへ音を出せる。

- パッドを叩くとサンプル再生、パッドLEDも点灯・点滅
- 4バンク × 16パッド（DECK1 8個 + DECK2 8個）
- ファイルはドラッグ＆ドロップで割り当て、設定は自動保存
- 実機（DDJ-RB）の RCA 出力で動作確認済み

## インストール（使う人向け）

1. **DDJ-RB のドライバを入れる**
   Pioneer DJ のサポートサイトから DDJ-RB の Windows 用ドライバをダウンロードしてインストール。
   入れないと MIDI（パッド・LED）は動くが、RCA から音が出ない（PC のスピーカーから鳴る）。
2. **`RB Sampler_x.x.x_x64-setup.exe` を実行してインストール**
   初回は「Windows によって PC が保護されました」と出る（コード署名なしのため）。
   「詳細情報」→「実行」で進める。
3. **rekordbox を完全に終了してから** RB Sampler を起動（下の「rekordbox との併用」参照）
4. 画面上部に `● 出力: ...DDJ-RB...` と `● MIDI: DDJ-RB` が出ていれば準備完了

## 使い方

- **再生**: 本体の SAMPLER モードでパッドを叩く／画面のパッドをクリック／キーボード
  - DECK1: `1 2 3 4` / `Q W E R`、DECK2: `5 6 7 8` / `T Y U I`
  - `F1`〜`F4`: バンクA〜D、`Esc`: 全停止
- **割り当て**: 音声ファイル（wav / mp3 / flac / ogg / m4a / aiff）をパッドへドラッグ＆ドロップ。
  複数ファイルは連続したパッドへ入る。
- **編集**: パッドを右クリック → 表示名・再生方法・音量・割り当て解除・MIDI学習
- **再生方法**: ワンショット / トグル（もう一度押すと停止） / 押している間 / ループ
- **設定タブ**: 出力デバイス、出力チャンネル（DDJ-RB の RCA は ch1/2）、バッファ、MIDI ポート、MIDI 割り当て
- **MIDIモニタタブ**: 本体から届いた MIDI を表示（割り当ての確認用）

設定とパッドの割り当ては `%APPDATA%\com.rbsampler.app\settings.json` に保存される。
別の PC へ設定を移すときはこのファイルをコピーする（サンプルのファイルパスは同じ場所にある必要あり）。

## rekordbox との併用

**DDJ-RB の MIDI は、同時に 1 つのソフトしか使えない。**
rekordbox が起動中（またはバックグラウンドに残っている）だと、RB Sampler からはパッドが反応しない。
本体に「認証」や「ロック」が書き込まれるわけではないので、rekordbox を閉じれば元に戻る。

うまくいかないときは次の順で試す:

1. RB Sampler を閉じる
2. rekordbox を終了し、タスクマネージャーで `rekordbox` / `rekordboxAgent` が残っていないか確認して終了
3. DDJ-RB の USB を抜き挿しする
4. RB Sampler を起動（起動中なら「設定」→ MIDI の「適用」で再接続）

逆に rekordbox を使うときは、先に RB Sampler を閉じる。

## トラブルシューティング

| 症状 | 対処 |
|---|---|
| パッドが反応しない | rekordbox を完全終了（上記）。「設定」で MIDI 入力が DDJ-RB か確認 |
| 音が PC のスピーカーから出る | DDJ-RB のドライバが未インストール。入れて USB を抜き挿し |
| 本体のレベル表示は振れるが PA から音が出ない | 本体の MASTER LEVEL と PA ミキサーの入力を確認 |
| レベル表示が振れない | 「設定」→ 出力チャンネルを ch3/4 に変えて試す |
| 押してから鳴るまでが遅い | 「設定」→ バッファを 256 → 128 と下げる（音が途切れたら戻す） |
| パッドの割り当てがずれている | パッドを右クリック →「MIDI学習」→ 本体のパッドを押す |

## 開発

必要なもの: Rust（stable）、Node.js、Visual Studio Build Tools（C++）、WebView2（Windows 11 は標準搭載）

```powershell
npm install
npm run tauri dev
```

### 配布用ビルド

```powershell
npm run tauri build
```

- インストーラ: `src-tauri/target/release/bundle/nsis/RB Sampler_<version>_x64-setup.exe`
- 単体 exe: `src-tauri/target/release/rb-sampler.exe`（インストール不要）

バージョンは `src-tauri/tauri.conf.json` の `version` で変更する。

### 構成

| ファイル | 役割 |
|---|---|
| `src-tauri/src/audio.rs` | cpal による音声出力。リアルタイムスレッドはチャンネル経由でのみ通信（ロックなし） |
| `src-tauri/src/decode.rs` | symphonia でデコード → ステレオ f32、出力サンプルレートへ変換 |
| `src-tauri/src/midi.rs` | midir による MIDI 入出力 |
| `src-tauri/src/config.rs` | 設定（JSON 保存）、DDJ-RB の既定 MIDI 割り当て |
| `src-tauri/src/lib.rs` | 状態管理、MIDI 学習、LED 制御、Tauri コマンド |
| `src/main.ts` | UI |

DDJ-RB の既定 MIDI 割り当て: DECK1 = Note ch8 `0x30`〜`0x37`、DECK2 = Note ch10 `0x30`〜`0x37`（SAMPLER モード）

## 既知の制限

- 音声出力は WASAPI（共有モード）。さらに遅延を詰めるなら ASIO 対応（cpal の `asio` 機能、ASIO SDK が必要）を検討
- サンプルレート変換は線形補間（ファイルと出力のレートが同じなら変換なし）
- コード署名なしのため、初回起動時に SmartScreen の警告が出る
