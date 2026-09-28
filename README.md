# vOBOS

A personal voice agent with an Iced GUI.

## Run

```sh
nix develop
cargo run --release
```

The default launch opens **vOBOS — Home**, using Iced's built-in
**Kanagawa Dragon** theme. It initializes SAM and the agent's tools/MCP
connections in the background, using the same configuration and CLI options
as headless mode. Ollama must be running with your configured model available.

- **Manual input:** type a command and press Enter or Send.
- **Voice input:** click Begin listening. Whisper and the voice detector are
  downloaded/cached as needed and loaded at this point, not at startup. Spoken
  commands are submitted automatically. Stop listening releases the microphone;
  Cancel listening also cancels model preparation. Starting again reloads the
  speech models from the cache.
- **Space shortcut:** press Space to toggle listening when the typing box is not
   focused. Click outside the box to leave it. Space still inserts
  spaces while typing; holding it down or using modifier keys does not repeatedly
  toggle listening. You can stop an active microphone while a reply is streaming.
- **Processing:** initialization and speech-model preparation have a loading
  view. During agent execution, the conversation stays visible with a compact
  thinking/tool status. You can scroll and draft your next command while waiting.
  Prompts run one at a time.
- **Streamed replies:** the GUI uses Rig's `stream_prompt` API to display text
  as it arrives, with Markdown headings, lists, emphasis, and code blocks. Updates
  are batched at 50 ms intervals and Markdown is parsed incrementally. Partial
   replies remain visible if a request fails.
- **Stop (Esc):** click Stop or press Escape while the window is focused to cancel
   an active prompt and stop speech. Escape also works while typing in the input
   box. Partial replies and your draft are preserved, and you can submit another
   prompt once cancellation is acknowledged. Stop remains available after a reply
   finishes to silence its playback; queued speech from the cancelled turn is
   discarded. Stopping a prompt does not turn off the microphone—use Stop listening
   for that. Already-started external tool actions may continue independently.
- **Speech playback:** the GUI asks the agent to return text directly and queues
  the completed response through SAM. Explicit `say` tool replies are also shown,
  with duplicate display/playback suppressed. Headless mode retains its voice-first
  `say` instructions.
- **Copy:** each reply has a Copy button. Clicking a Markdown link copies its URL
  to the clipboard.

Listening pauses automatically during agent execution and SAM playback to avoid
transcribing the assistant's own voice. The microphone is off until you enable
it. Errors appear in the GUI; initialization can be retried, and prompt failures
leave the conversation and input available for another attempt.

For example, select an Ollama model or configuration file for the GUI:

```sh
cargo run --release -- --ollama-model your-model --config /path/to/config.toml
```

To run the existing voice agent:

```sh
cargo run --release -- --headless
# Existing agent options still work:
cargo run --release -- --headless --config /path/to/config.toml
```

### Image interpretation

With a vision-capable Ollama model, ask the agent to inspect a local image, for
example: `Describe the image at /home/me/Pictures/photo.png`.

The `image` tool accepts `{"path":"/path/to/image.png"}` and loads PNG, JPEG,
GIF, or WebP files up to 20 MiB. Relative paths resolve from the application's
working directory. For a URL, the agent can use `cmd` to save an image first,
then open it with `image`. For the current screen, use `screencap` directly.

Images are sent as visual input on the next model turn and remain available in
conversation history. This works in both GUI and headless mode; format support
also depends on the selected Ollama model.

### Screen capture

Ask the agent to inspect the screen, for example: `What is on my screen?`.
The `screencap` tool uses xcap and returns a PNG directly as visual input, without
saving a screenshot to disk. Call it with `{}` for the primary monitor (falling
back to the first), or `{"monitor": 1}` for a specific zero-based monitor index.
The result includes the monitor name, image dimensions, and available index range.

Screen capture is enabled by either the `wayland` or `x11` Cargo feature. xcap
detects the active session at runtime; its Linux backend dependencies are bundled
together even when only one project feature is enabled. For an X11-only build:

```sh
cargo run --release --no-default-features --features x11,wgpu
```

Linux builds need XCB, Wayland, PipeWire, EGL, and GBM development libraries;
the Nix shell supplies them. On Wayland, capture depends on the compositor's
capture protocols or a working desktop portal, which may show a capture dialog.
Capture and PNG encoding run on a blocking worker so the GUI stays responsive.

### GUI dependencies

Iced 0.14 has defaults disabled; `tiny-skia`, `tokio`, `svg`, `markdown`, and `advanced`
are enabled, with `wayland` supplied by the project's default `wayland` feature.
The GUI uses software rendering, the existing Tokio ecosystem,
and embedded vector icons, without Iced's X11 or WGPU backends. The existing
speech/input dependencies may still use Vulkan or X11 independently.

On Linux, run from a Wayland session with `libwayland-client`, `libxkbcommon`,
and a system sans-serif font available. The Nix shell supplies the native
libraries alongside the agent's existing build dependencies.

The one-time window-open subscription is removed after startup. Keyboard shortcuts
are handled directly by the widget tree, so ordinary scrolling is not forwarded
through Iced's bounded subscription event queues. Reply generation has no animation
subscription; streamed content drives redraws.

## Window behavior on Wayland

The window requests centered placement and sizes itself once to **40% of the
current monitor's logical width × 30% of its logical height**, with a 360 × 240
minimum for readability. Its fallback size is 768 × 324. It remains resizable,
and the home content scrolls at smaller sizes. These proportions describe a
rectangle on typical monitors, rather than a literal square.

Wayland gives the compositor authority over positioning and tiling. An app
cannot reliably force floating or centering through ordinary window settings.
Use the stable app ID **`com.seedse.vobos`** to apply a compositor rule.
For example, in Sway:

```sway
for_window [app_id="^com\\.seedse\\.vobos$"] floating enable, resize set width 40 ppt height 30 ppt, move position center
```

For other tiling compositors, add equivalent float/center rules matching that
app ID. Window borders, corners, and the native title bar follow the compositor;
the app's content uses Iced's built-in Kanagawa Dragon theme.

## Desktop icon

The Nix package installs the desktop entry and scalable application icon.
For a local Cargo build, ensure `vobos` is on your `PATH`, then install them:

```sh
install -Dm644 assets/com.seedse.vobos.desktop \
  "$HOME/.local/share/applications/com.seedse.vobos.desktop"
install -Dm644 assets/com.seedse.vobos.svg \
  "$HOME/.local/share/icons/hicolor/scalable/apps/com.seedse.vobos.svg"
```

Wayland shells associate the window with this desktop entry to show its icon
in launchers and task switchers. UI icons are embedded in the binary and do not
require an icon font or files in the working directory.

## Next GUI milestones

- Add session management and tool/settings screens.
- Add richer per-tool progress and cancellation of agent work.
