# Stage 2: web engine for IVMP's UI (research)

Goal: HTML/CSS/JS UI inside the game (like FiveM's NUI, which uses CEF), drawn on top of the game's
Direct3D 9 frame, with mouse and keyboard going to the page while it has focus. Small client size
matters, and GTA IV is a 32-bit process (`IVMP_Client.dll` is i686).

Stage 1 (done) gives the pieces the web layer plugs into: the D3D9 hook (`render.rs`, UI drawn from
EndScene on the back buffer), the window subclass and input capture (`render.rs`, `ui.rs`,
`focus.rs`) and the F1 console. A web page would be one more layer drawn after imgui.

## Candidates

| Engine | Size | 32-bit | Offscreen / texture output | Verdict |
|---|---|---|---|---|
| **WebView2** (Edge, Evergreen runtime) | ~0 in our download (runtime ships with Windows 11 and is installed by Windows/Edge elsewhere); only `WebView2Loader.dll` (~150 KB) | Yes, the SDK has x86 builds | No official offscreen API; two workable routes below | **Recommended** |
| CEF (what FiveM uses) | 100+ MB of binaries, extra helper processes | Chromium's Windows x86 support is being wound down; check the exact CEF build before committing | Yes, first-class (OnPaint buffer / shared texture) | Good tech, wrong size; keep as fallback |
| Ultralight | small | **64-bit only** per its site | Yes, custom GPU driver | Out (32-bit), and the license is paid/proprietary |
| Servo / Blitz | small-ish | not production ready | Possible | Too immature now |
| RmlUi / litehtml / Sciter | tiny | yes | Yes (draw calls into our renderer) | Not web engines (no/limited JS and modern CSS). Fallback if WebView2 fails for a simple HUD |
| Coherent GT | - | yes | yes | Paid, proprietary: out |

## Recommendation: WebView2

Why: nothing to ship (the runtime is already on the machine, as the user noted), small loader,
real Chromium rendering, x86 supported, JS bridge built in (`PostWebMessageAsJson`,
`WebMessageReceived`, `AddHostObjectToScript`).

### How to get its pixels into a D3D9 game

WebView2 renders in a separate process with DirectComposition, so there is no direct "give me a
D3D texture" call. Two routes, in the order to try them:

1. **Composition controller + Windows.Graphics.Capture** (what community overlay projects do, e.g.
   [this gist](https://gist.github.com/pabloko/5b5bfb71ac52d20dfad714c666a0c428)): create the page
   with `ICoreWebView2CompositionController` on a hidden DirectComposition visual, capture that
   visual with `GraphicsCaptureItem` into a D3D11 texture (GPU, with transparency), copy to a
   staging texture and upload the dirty frame into a D3D9 texture that our overlay draws. Input is
   sent with `SendMouseInput` / `SendPointerInput` (keyboard goes through the hosting window).
2. **`CapturePreview`** (simplest, slower): `ICoreWebView2::CapturePreview` to a PNG/JPEG stream
   each time the page changes, decode and upload. Fine for a spike and for mostly static UI.

The game's device is a plain D3D9 device (not D3D9Ex), which cannot open a D3D11 shared texture, so
the upload goes through system memory: a 1920x1080 BGRA frame is about 8 MB, so only upload when
the page actually changed (capture frames carry no dirty rect, so compare or use a page-side
"dirty" message).

### Risks to check in the spike (in this order)

1. A **32-bit process** creating a `CoreWebView2Environment` against the x64 Evergreen runtime
   (should work: the loader picks the runtime and runs it out of process).
2. **Where the message loop runs**: WebView2 needs a thread with a message pump. Use our own thread
   with a hidden window, not the game's main thread.
3. **COM apartment**: the game's thread model vs `CoInitializeEx(STA)` on our thread.
4. **Capture works for a hidden composition visual** (no real window on screen) and keeps
   transparency.
5. **Frame cost**: staging copy + `LockRect` upload per changed frame at 1080p; measure with the
   perf overlay.
6. **Input focus**: while the page has focus, swallow keyboard/mouse the same way the F1 console
   does (`ui::capturing()`, `window_message`), and give it back when it loses focus.
7. **Rust bindings**: `windows` crate has Win32 COM definitions for WebView2 through the
   `webview2-com` crate (generated from the official SDK metadata); the WinRT capture APIs are in
   `windows`'s `Graphics_Capture` features.

## Plan

1. Spike (separate small crate, not the game): hidden window, WebView2 composition controller,
   load a local `index.html`, capture a frame into a D3D11 texture, save it as a PNG. Proves 1-4.
2. Spike in the game: draw the captured frame as a textured quad from the EndScene hook, after
   imgui. Proves 5.
3. Input and focus routing, plus a JS bridge (`ivmp.send(name, json)` and events from the client)
   modelled on NUI: the page talks to the Rust client, which talks to the server.
4. Package: ship `WebView2Loader.dll` next to the DLL; the launcher checks the runtime and tells the
   user how to install it if missing.

## Open questions

- Does the runtime exist on the supported Windows versions (Windows 10 without Edge updates)? The
  launcher can bootstrap it (Microsoft's evergreen bootstrapper), needs the user's OK to download.
- Keep a CEF build as an optional fallback only if WebView2 capture can't be made reliable.
