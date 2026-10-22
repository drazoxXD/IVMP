# Stage 2: web engine for IVMP's UI

**Decision: CEF** (latest stable, windowless/offscreen), locked down so the page can only talk to
IVMP's own script runner and never the internet. Chosen over WebView2 because CEF has a real
offscreen API (pixel buffer + dirty rectangles + input events), which is exactly what a D3D9 game
overlay needs; WebView2 only has indirect capture routes (see "Why not WebView2").

Goal: HTML/CSS/JS UI inside the game (like FiveM's NUI, which also uses CEF), drawn on top of the
game's Direct3D 9 frame, with mouse and keyboard going to the page while it has focus. GTA IV is a
32-bit process (`IVMP_Client.dll` is i686).

Stage 1 (done) gives the pieces the web layer plugs into: the D3D9 hooks (`render.rs`, UI drawn at
Present), the window subclass and input capture (`render.rs`, `ui.rs`, `focus.rs`, including the raw
DirectInput mouse) and the F1 console. The web page is one more layer drawn after imgui.

## Which CEF

Checked on the CEF build server (Spotify's automated builds, `cef-builds.spotifycdn.com`):

| | |
|---|---|
| Latest stable with a **32-bit Windows** build | CEF 156.0.7 (Chromium 156.0.8078.17), `windows32` |
| Previous stable | CEF 152.0.15 (Chromium 152.0.7977.169) |
| Archive sizes, windows32 | minimal 154 MB, client 152 MB, standard 302 MB (compressed, with headers/libs/symbols-free) |

So 32-bit Windows is still built for the latest stable Chromium. We use the **latest stable
windows32 build**, pinned by version in the repo, and bump it deliberately (Chromium security
releases are frequent, which matters for anything that renders HTML).

"Minimized version": CEF cannot be built much smaller without compiling Chromium ourselves, so the
size win comes from what we ship, not from a different binary:

- Use the **minimal** distribution (no sample apps, no `cefclient`/`cefsimple`).
- Ship only what the runtime needs: `libcef.dll`, `chrome_elf.dll`, `d3dcompiler_47.dll`,
  `libEGL.dll`, `libGLESv2.dll`, `v8_context_snapshot.bin`, `icudtl.dat`, `resources.pak`,
  `chrome_100_percent.pak`, `locales/en-US.pak` only. Drop `vk_swiftshader*`/`vulkan-1.dll` if the
  GPU process runs on ANGLE/D3D11 without them (test), `devtools_resources.pak` in release builds,
  and every other locale.
- Do **not** put CEF in `IVMP_Client.zip`. The runtime is large (libcef.dll alone is a few hundred
  MB unpacked), so it is a separate "IVMP web runtime" pack: the launcher downloads it once into a
  cache folder and checks its hash. The client zip stays small and the web layer is optional.
  (Where it is hosted, and the launcher downloading it, needs the user's go-ahead.)

## Lockdown: the page can only talk to our script runner

The browser is a UI renderer, not a browser. Rules, enforced in code, not by convention:

1. **Only our own scheme.** All pages and assets load from a custom scheme `ivmp://<pack>/...`
   served by a `CefSchemeHandlerFactory` from local files (UI packs on disk, hash-checked). A
   `CefResourceRequestHandler` **cancels every other request**: `http`, `https`, `ws`, `wss`,
   `ftp`, `file`, `data` navigations to other origins, everything. No remote origin ever loads.
2. **No network at the engine level either**, as a second wall: command-line switches
   `--host-resolver-rules="MAP * ~NOTFOUND"` (no DNS), `--proxy-server=direct://`,
   `--disable-background-networking`, `--disable-component-update`, `--disable-sync`,
   `--disable-extensions`, `--disable-domain-reliability`, `--disable-client-side-phishing-detection`,
   `--disable-default-apps`, `--no-pings`, `--metrics-recording-only` off, WebRTC/media router
   features off (`--disable-features=...`). No cookies/cache on disk (in-memory request context).
3. **No navigation, popups, downloads, file access, devtools in release**: `OnBeforePopup` returns
   true, `OnBeforeBrowse` only allows `ivmp://`, no download handler, `file_access_from_file_urls`
   off, remote debugging port off (devtools only behind a developer flag, `nui_devtools` entry in
   the F1 menu).
4. **One channel out.** JS gets exactly one bridge, `ivmp.send(name, json)` / `ivmp.on(name, fn)`,
   implemented with CEF's message router in the renderer process. Messages go to the browser
   process in the game, then to the **client script runner** (the scripting engine we still have
   to build) which decides what to do (call natives, talk to the server, answer the page). The page
   has no other API: no `fetch` to anywhere (blocked by 1 and 2), no native callbacks, no direct
   access to game memory or the filesystem.
5. **Messages are data only**: JSON, size-limited, rate-limited per second, schema-checked by the
   script runner; unknown message names are dropped and logged to the F1 console channel `ui`.
6. **Process isolation**: the renderer, GPU and utility work in CEF's own helper processes
   (`IVMP_Web.exe`, 32-bit, our own small executable via `browser_subprocess_path`). Only the
   browser-process side (window management, painting, the bridge) lives inside the game process.
   CEF's sandbox for the helper processes stays on if the build allows it.
7. **UI packs come from the server** (like FiveM resources) but are only data files under
   `ivmp://`: HTML/CSS/JS/images/fonts, verified by a hash the server announces, stored by hash.
   A pack can't change the lockdown, only the page content.

## Rendering into the D3D9 frame

Windowless (offscreen) mode: `CefWindowInfo::SetAsWindowless`, `windowless_frame_rate` 60,
transparent background. CEF calls `OnPaint(buffer, width, height, dirtyRects)` with a BGRA frame; we
keep one D3D9 texture (managed pool, survives device reset like our font texture), `LockRect` and
copy **only the dirty rectangles**, and draw it as a textured quad after imgui in the Present hook
(`render.rs`). The page sees the game's back buffer size (`GetViewRect`), `OnDeviceReset` resizes
it. The accelerated shared-texture path is D3D11 and not usable by a plain D3D9 device, so the
software `OnPaint` copy is the route.

Input: while a page has focus (the F1 menu or a script asks for it) the same capture as the console
applies (`ui::capturing()`, raw DirectInput mouse in `focus.rs`, `window_message` in `ui.rs`);
mouse/wheel/keys are forwarded with `SendMouseMoveEvent`, `SendMouseClickEvent`,
`SendMouseWheelEvent`, `SendKeyEvent`; a page can release the focus through the bridge.

Threading: `multi_threaded_message_loop = true` so CEF runs its own UI thread and never touches the
game's loop; the paint callback only copies into a staging buffer and the render thread uploads it.
`CefInitialize` happens after the game has unpacked, from the same post-unpack thread as the other
installs, and never on a loading screen (same `hang::game_running()` gate as the UI).

## Plan

1. **Spike** (a standalone 32-bit exe, not the game): download the pinned windows32 CEF, build
   `libcef_dll_wrapper` (CMake) or evaluate a Rust binding (`cef-rs`), open a windowless browser on
   an `ivmp://` page, dump one `OnPaint` frame to a PNG. Proves: 32-bit runtime, OSR, our scheme
   handler, and that every outside request is refused (test page that tries `fetch('https://...')`,
   a websocket and an image from the internet: all must fail).
2. **In the game**: initialise CEF after unpack, upload dirty rects into a D3D9 texture, draw it
   after imgui. Measure frame cost with the F1 perf overlay (target: no visible cost when the page
   is static).
3. **Input and focus** routing, plus the JS bridge to a stub script runner (`ivmp.send` ->
   log line in the F1 console channel `ui`).
4. **Script runner hookup** once the client script engine exists (needs the native hook): the bridge
   becomes the page-side of the scripting API.
5. **Packaging**: separate web runtime pack with hash, launcher download and cache, credits for
   Chromium/CEF licences (BSD plus third-party notices) in the release.

## Risks

- **Size**: the CEF runtime is the biggest file set in the project by far; mitigated by the
  separate optional pack.
- **Memory in a 32-bit game**: the browser process part lives inside the game's address space (the
  renderer and GPU are separate processes). Keep one browser, no extra tabs, cap the frame size.
- **Chromium update cadence**: pin a version, update on security releases, rebuild the pack.
- **Multiple CEF-using mods**: Steam overlay and other injected DLLs already hook D3D9 in this
  process; our hooks are chained (see `render.rs`), the CEF subprocess must not load them
  (`IVMP_Web.exe` is a separate exe, so ASI/overlay injection into the game does not apply).
- **Lockdown bugs**: the default for a new request type must be "deny"; the spike's test page is
  kept as a regression test.

## Why not WebView2

Small and already on Windows, but it has no offscreen API: pixels would have to come from capturing
a hidden DirectComposition visual with Windows.Graphics.Capture (community projects only, e.g.
[this gist](https://gist.github.com/pabloko/5b5bfb71ac52d20dfad714c666a0c428)) or from
`CapturePreview` (slow), then copied through system memory anyway. Input, focus and lockdown
(blocking all network from a system-wide runtime we don't own) are also less controllable. Kept
only as a note; revisit only if the CEF runtime size becomes a blocker.

## Other engines considered

| Engine | Verdict |
|---|---|
| Ultralight | 64-bit only, proprietary license: out |
| Servo / Blitz | not production ready |
| RmlUi / litehtml / Sciter | not full web engines (limited JS / modern CSS): only a fallback for a simple HUD |
| Coherent GT | paid, proprietary: out |
