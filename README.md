# scrcpy-agent

Android 端末を **scrcpy-server** 経由で操作し、その操作を **MCP ツール**としてエージェントに公開するネイティブ GTK4 アプリです。
Electron / WebView は使いません。画面は scrcpy の H.264 ストリームを ffmpeg でデコードして GTK4 の `Picture` に描画し、
入力は scrcpy の control ソケットに制御メッセージとして送ります。

## 特徴

- 端末を選んで「接続」→ ライブプレビュー、その状態で「MCPサーバー起動」→ エージェントから操作可能
- 操作系は Playwright 相当（スクショ / タップ / ダブルタップ / 長押し / スワイプ / ドラッグ / スクロール / 文字入力 / キー / 戻る / 回転 / 通知）
  からアクセシビリティツリー参照を除いたもの
- MCP は 127.0.0.1 の **Streamable HTTP**（既定 `http://127.0.0.1:18765/mcp`、ツールのみ）
- 単一ファイル AppImage をビルド可能（`scripts/build-appimage.sh`）
- GUI なしの `--headless` モードあり

## 必要なもの

ビルド時:
- Rust（1.85+）、GTK4 開発ヘッダ（4.10 以上、Fedora なら `gtk4-devel`）
- ffmpeg 開発ヘッダ（`pkg-config libavcodec` が通ること。Fedora は `ffmpeg-free-devel`）
- scrcpy 5.0.1 の `scrcpy-server`（既定 `/usr/share/scrcpy/scrcpy-server`。`SCRCPY_SERVER_JAR` で変更可）
- `scrcpy --version` と jar の版数は一致させてください（このクライアントは 5.0.1 のプロトコルに固定）

実行時:
- `adb`（PATH 上、または `--adb` / `config.toml` で指定）
- USB デバッグが有効で、端末側で許可済みの Android 端末

## ビルドと実行

```bash
cargo build --release
./target/release/scrcpy-agent            # GUI
./target/release/scrcpy-agent --headless --serial <serial> --mcp-port 18765
```

## 使い方

1. 端末を USB 接続し、端末側で「USB デバッグを許可」
2. `scrcpy-agent` 起動 → 左上の端末一覧で選択 → **接続**
3. プレビューが出たら **MCPサーバー起動**（URL が表示され、コピーできます）
4. エージェント側に MCP を登録（下記）して、ツールを呼ぶ

### DSH への登録

`~/.dsh/profiles/web/cordis.patch.yml` の末尾に追加:

```yaml
- insert:
    - id: mcp-android
      name: "@deepseek-ai/dsh-mcp-client"
      config:
        serverName: android
        transport: streamable-http
        url: http://127.0.0.1:18765/mcp
        toolCallTimeoutMs: 30000
        failOnStartupError: false
        reconnect:
          enabled: true
          initialDelayMs: 1000
          maxDelayMs: 5000
          maxAttempts: 1000
```

ツール名は `mcp__android__screenshot` / `mcp__android__tap` のようになります。
MCP サーバーはアプリで起動ボタンを押すまで待ち受けません。DSH 側は再接続を続ける設定にしてあるので、アプリを後から起動しても数秒でツールが生えます（設定の反映には DSH の再起動が必要です）。

## MCP ツール

| ツール | 内容 |
|---|---|
| `list_devices` / `session_status` / `start_session` / `stop_session` | 端末一覧とセッション管理 |
| `screenshot` | 現在の画面 PNG＋`width`/`height`（座標系はこの画像のピクセル） |
| `screen_info` / `current_app` | 解像度・密度・前面アプリ |
| `tap` / `double_tap` / `long_press` / `swipe` / `drag` / `scroll` | ジェスチャ |
| `type_text` | 既定でクリップボード経由のペースト（IME の影響を受けない）。`mode:"key"` で生キーイベント |
| `press_key` / `back` / `rotate` / `notifications` / `wake` | キー・システム操作 |
| `clipboard_get` / `clipboard_set` | 端末クリップボード |
| `launch_app` / `list_apps` | パッケージ操作 |
| `shell` | adb shell（上級者向け） |

座標は常に「直前の `screenshot` のピクセル」です。操作後は `screenshot` で確認してください。
各ジェスチャは `settle_ms` までに画面が変化したかを `changed` で返します。

## AppImage

```bash
./scripts/build-appimage.sh
./dist/scrcpy-agent-x86_64.AppImage
```

GTK4/GLib/ffmpeg まわりを同梱し、GPU ドライバ（libGL/libEGL/libvulkan/libdrm）と glibc はホストのものを使います。
ビルドした環境（Fedora 44 / glibc 2.42）以降での実行を想定しています。

## 設定

`~/.config/scrcpy-agent/config.toml`（任意）:

```toml
adb_path = "adb"
mcp_bind = "127.0.0.1"
mcp_port = 18765
max_fps = 0
max_size = 0
video_bit_rate = 8000000
stay_awake = false
manual_input = true
settle_ms = 120
# auth_token = "..."   # 設定すると Authorization: Bearer が必要になります
```

## 仕組み

```
[GTK4 main thread]  端末一覧 / プレビュー / ログ / MCP 起動ボタン
      | Arc<FrameStore>（最新フレームのみ）
[Session worker]   adb reverse → scrcpy-server 起動 → ビデオ/コントロール接続
      | H.264 → ffmpeg（ffmpeg-next）で RGBA に変換
[Tokio runtime]    axum 製 Streamable HTTP MCP（127.0.0.1:18765/mcp）
```

- 端末側は `scrcpy-server`（jar をビルド時に埋め込み、`/data/local/tmp/scrcpy-agent-5.0.1.jar` に push）
- トンネルは `adb reverse` を優先し、使えない場合は `adb forward` にフォールバック
- 映像パケットは scrcpy 5.0.1 の 12 バイトヘッダ（セッション / config / keyframe / PTS / サイズ）を解釈
- 制御メッセージは `control_msg.c` のレイアウトをそのまま実装（ゴールデンバイトのテストあり）

## 制限・既知の注意

- scrcpy-server は **5.0.1 固定**。別版の jar を使う場合は `SCRCPY_SERVER_JAR` と `SCRCPY_SERVER_VERSION` を両方合わせてください
- `type_text` は既定でクリップボード経由のペースト（日本語 IME でも安全）。`mode:"key"` は生キーイベントで、IME に合成されます
- `clipboard_get` は Android 10 以降のクリップボード制限で空を返すことがあります
- `FLAG_SECURE` の画面や画面オフ時は真っ黒なスクショになります
- マルチタッチ（ピンチ）は未対応（1 ポインタのみ）
- 音声転送は未対応
- `shell` は端末上で任意コマンドを実行できます。扱いに注意してください

## ライセンス

MIT。埋め込む `scrcpy-server` は [scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0）に由来します。
