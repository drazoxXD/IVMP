// IVMP_Web.exe: CEF in its own process, so no CEF thread or window lives inside the game (the Steam
// overlay's hooks crashed on those). The same exe is also CEF's helper process (renderer, GPU, ...).
//
//   IVMP_Web.exe --ivmp-pipe=<name> --ivmp-frame=<mapping> --ivmp-game-pid=<pid> --ivmp-url=<url>
//
// The game talks to it over a named pipe (length-prefixed messages, below) and reads the page's
// frames from a shared-memory mapping. Lockdown (web_app.h): only ivmp://, ivmp.send is the one
// way out of the page, and it ends up as a Bridge message to the game.
#include <algorithm>
#include <atomic>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "include/cef_client.h"
#include "include/cef_life_span_handler.h"
#include "include/cef_render_handler.h"
#include "include/cef_version.h"
#include "include/wrapper/cef_closure_task.h"
#include "web_app.h"

namespace {

using ivmp::Log;

// ---- protocol ---------------------------------------------------------------------------------
// Every message: u32 length (of type + payload), u8 type, payload. Little endian.
enum ToHost : uint8_t { kVisible = 1, kFocus = 2, kResize = 3, kMouseMove = 4, kMouseButton = 5, kMouseWheel = 6, kKey = 7, kEmit = 8, kDevTools = 9 };
enum ToGame : uint8_t { kLog = 0x81, kBridge = 0x82, kReady = 0x83 };

// Version of this protocol; the game refuses a process that reports another one.
const uint32_t kProtocol = 1;

// ---- shared frame -----------------------------------------------------------------------------
// Header: u32 sequence (odd while being written), u32 width, u32 height, u32 reserved; then a ring
// of the dirty rectangle (x, y, w, h as u32) of the last kRing paints, indexed by paint number
// (sequence / 2) % kRing; then the BGRA frame. The sequence grows by 2 per paint, so a reader that
// last saw sequence S and now sees T knows (T - S) / 2 paints happened and which rectangles to copy.
const size_t kMaxWidth = 4096, kMaxHeight = 2304;
const size_t kRing = 16;
const size_t kHeader = 16 + kRing * 16;
volatile uint32_t* g_frame_header = nullptr;
volatile uint32_t* g_frame_ring = nullptr;
uint8_t* g_frame_data = nullptr;

// Two pipes, one per direction: a synchronous pipe handle with a pending read blocks writes on the
// same handle, which hung the game.
HANDLE g_pipe = INVALID_HANDLE_VALUE;     // to the game
HANDLE g_pipe_in = INVALID_HANDLE_VALUE;  // from the game
std::mutex g_pipe_lock;

std::atomic<int> g_width{1280};
std::atomic<int> g_height{720};
CefRefPtr<CefBrowser> g_browser;

void SendToGame(uint8_t type, const std::string& payload) {
  std::lock_guard<std::mutex> lock(g_pipe_lock);
  if (g_pipe == INVALID_HANDLE_VALUE) return;
  uint32_t length = (uint32_t)payload.size() + 1;
  std::string out((const char*)&length, 4);
  out.push_back((char)type);
  out += payload;
  DWORD written = 0;
  WriteFile(g_pipe, out.data(), (DWORD)out.size(), &written, nullptr);
}

void LogToGame(const char* line) { SendToGame(kLog, line); }

class FnTask : public CefTask {
 public:
  explicit FnTask(std::function<void()> f) : f_(std::move(f)) {}
  void Execute() override { f_(); }
  IMPLEMENT_REFCOUNTING(FnTask);

 private:
  std::function<void()> f_;
};

void OnUi(std::function<void()> f) { CefPostTask(TID_UI, new FnTask(std::move(f))); }

class BridgeHandler : public CefMessageRouterBrowserSide::Handler {
 public:
  bool OnQuery(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int64_t, const CefString& request, bool, CefRefPtr<Callback> callback) override {
    std::string text = request.ToString();
    if (text.size() > 64 * 1024) {
      callback->Failure(1, "message too large");
      return true;
    }
    SendToGame(kBridge, text);
    callback->Success("ok");
    return true;
  }
};

class WallResourceRequestHandler : public CefResourceRequestHandler {
 public:
  ReturnValue OnBeforeResourceLoad(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest> request, CefRefPtr<CefCallback>) override {
    std::string url = request->GetURL().ToString();
    if (url.rfind("ivmp://", 0) == 0) return RV_CONTINUE;
    Log("web: wall cancelled %s", url.c_str());
    return RV_CANCEL;
  }
  IMPLEMENT_REFCOUNTING(WallResourceRequestHandler);
};

class Client : public CefClient, public CefRenderHandler, public CefRequestHandler, public CefLifeSpanHandler, public CefDisplayHandler, public CefLoadHandler {
 public:
  Client() {
    CefMessageRouterConfig config;
    router_ = CefMessageRouterBrowserSide::Create(config);
    router_->AddHandler(&bridge_, false);
  }
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }

  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame, CefProcessId source, CefRefPtr<CefProcessMessage> message) override {
    return router_->OnProcessMessageReceived(browser, frame, source, message);
  }

  void GetViewRect(CefRefPtr<CefBrowser>, CefRect& rect) override { rect = CefRect(0, 0, g_width, g_height); }

  void OnPaint(CefRefPtr<CefBrowser>, PaintElementType type, const RectList& dirty, const void* buffer, int width, int height) override {
    if (type != PET_VIEW || !g_frame_header || width <= 0 || height <= 0 || (size_t)width > kMaxWidth || (size_t)height > kMaxHeight) return;
    // The union of this paint's dirty rectangles (the whole frame when the size changed).
    bool full = g_frame_header[1] != (uint32_t)width || g_frame_header[2] != (uint32_t)height || dirty.empty();
    int x0 = 0, y0 = 0, x1 = width, y1 = height;
    if (!full) {
      x0 = width;
      y0 = height;
      x1 = 0;
      y1 = 0;
      for (const CefRect& r : dirty) {
        x0 = std::min(x0, std::max(r.x, 0));
        y0 = std::min(y0, std::max(r.y, 0));
        x1 = std::max(x1, std::min(r.x + r.width, width));
        y1 = std::max(y1, std::min(r.y + r.height, height));
      }
      if (x1 <= x0 || y1 <= y0) return;
    }
    // Seqlock: odd while writing, so the game never uses a half-written frame.
    uint32_t sequence = g_frame_header[0];
    g_frame_header[0] = sequence + 1;
    MemoryBarrier();
    const uint8_t* source = (const uint8_t*)buffer;
    for (int row = y0; row < y1; row++) {
      memcpy(g_frame_data + ((size_t)row * width + x0) * 4, source + ((size_t)row * width + x0) * 4, (size_t)(x1 - x0) * 4);
    }
    volatile uint32_t* slot = g_frame_ring + (((sequence / 2) + 1) % kRing) * 4;
    slot[0] = x0;
    slot[1] = y0;
    slot[2] = x1 - x0;
    slot[3] = y1 - y0;
    g_frame_header[1] = width;
    g_frame_header[2] = height;
    MemoryBarrier();
    g_frame_header[0] = sequence + 2;
  }

  bool OnBeforeBrowse(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest> request, bool, bool) override {
    bool allowed = request->GetURL().ToString().rfind("ivmp://", 0) == 0;
    if (!allowed) Log("web: navigation to %s refused", request->GetURL().ToString().c_str());
    return !allowed;
  }
  CefRefPtr<CefResourceRequestHandler> GetResourceRequestHandler(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest>, bool, bool, const CefString&, bool&) override {
    return new WallResourceRequestHandler();
  }
  bool OnBeforePopup(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int, const CefString& url, const CefString&, CefLifeSpanHandler::WindowOpenDisposition, bool, const CefPopupFeatures&, CefWindowInfo&, CefRefPtr<CefClient>&, CefBrowserSettings&, CefRefPtr<CefDictionaryValue>&, bool*) override {
    Log("web: popup %s refused", url.ToString().c_str());
    return true;
  }
  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    g_browser = browser;
    Log("web: browser created");
  }
  void OnBeforeClose(CefRefPtr<CefBrowser> browser) override {
    router_->OnBeforeClose(browser);
    g_browser = nullptr;
  }
  // The page's console (console.log, warnings, errors and uncaught exceptions) goes to the game's
  // log as the `cef` channel, which the in-game console shows; errors read "error" (shown in red).
  bool OnConsoleMessage(CefRefPtr<CefBrowser>, cef_log_severity_t level, const CefString& message, const CefString& source, int line) override {
    std::string text = message.ToString();
    if (level >= LOGSEVERITY_ERROR) {
      Log("cef: error: %s (%s:%d)", text.c_str(), source.ToString().c_str(), line);
    } else if (level == LOGSEVERITY_WARNING) {
      Log("cef: warning: %s", text.c_str());
    } else {
      Log("cef: %s", text.c_str());
    }
    return true;
  }
  void OnLoadEnd(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, int status) override {
    if (frame->IsMain()) Log("web: page loaded (status %d)", status);
  }
  void OnLoadError(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, ErrorCode code, const CefString& text, const CefString& url) override {
    Log("cef: error: %s could not load: %s (%d)", url.ToString().c_str(), text.ToString().c_str(), (int)code);
  }
  void OnRenderProcessTerminated(CefRefPtr<CefBrowser> browser, TerminationStatus status, int code, const CefString& text) override {
    Log("cef: error: the page's render process ended (status %d, code %d) %s", (int)status, code, text.ToString().c_str());
    router_->OnRenderProcessTerminated(browser);
  }

  IMPLEMENT_REFCOUNTING(Client);

 private:
  CefRefPtr<CefMessageRouterBrowserSide> router_;
  BridgeHandler bridge_;
};

CefMouseEvent MouseAt(int x, int y) {
  CefMouseEvent e;
  e.x = x;
  e.y = y;
  e.modifiers = 0;
  return e;
}

int32_t I32(const std::string& s, size_t at) {
  int32_t v = 0;
  if (s.size() >= at + 4) memcpy(&v, s.data() + at, 4);
  return v;
}

// One message from the game, on the pipe thread; the work happens on CEF's UI thread.
void Handle(uint8_t type, const std::string& p) {
  switch (type) {
    case kVisible: {
      bool visible = I32(p, 0) != 0;
      OnUi([visible]() {
        if (!g_browser) return;
        g_browser->GetHost()->WasHidden(!visible);
        if (!visible) g_browser->GetHost()->SetFocus(false);
      });
      break;
    }
    case kFocus: {
      bool focus = I32(p, 0) != 0;
      OnUi([focus]() {
        if (g_browser) g_browser->GetHost()->SetFocus(focus);
      });
      break;
    }
    case kResize: {
      int w = std::min<int>(std::max(I32(p, 0), 16), (int)kMaxWidth), h = std::min<int>(std::max(I32(p, 4), 16), (int)kMaxHeight);
      if (w == g_width && h == g_height) break;
      g_width = w;
      g_height = h;
      OnUi([]() {
        if (g_browser) g_browser->GetHost()->WasResized();
      });
      break;
    }
    case kMouseMove: {
      int x = I32(p, 0), y = I32(p, 4);
      OnUi([x, y]() {
        if (g_browser) g_browser->GetHost()->SendMouseMoveEvent(MouseAt(x, y), false);
      });
      break;
    }
    case kMouseButton: {
      int button = I32(p, 0), down = I32(p, 4), x = I32(p, 8), y = I32(p, 12);
      OnUi([button, down, x, y]() {
        if (!g_browser) return;
        CefBrowserHost::MouseButtonType t = button == 1 ? MBT_RIGHT : button == 2 ? MBT_MIDDLE : MBT_LEFT;
        g_browser->GetHost()->SendMouseClickEvent(MouseAt(x, y), t, down == 0, 1);
      });
      break;
    }
    case kMouseWheel: {
      int x = I32(p, 0), y = I32(p, 4), delta = I32(p, 8);
      OnUi([x, y, delta]() {
        if (g_browser) g_browser->GetHost()->SendMouseWheelEvent(MouseAt(x, y), 0, delta);
      });
      break;
    }
    case kKey: {
      int kind = I32(p, 0), vk = I32(p, 4), lparam = I32(p, 8), modifiers = I32(p, 12);
      OnUi([kind, vk, lparam, modifiers]() {
        if (!g_browser) return;
        CefKeyEvent e;
        e.type = kind == 0 ? KEYEVENT_RAWKEYDOWN : kind == 1 ? KEYEVENT_KEYUP : KEYEVENT_CHAR;
        e.windows_key_code = vk;
        e.native_key_code = lparam;
        e.is_system_key = false;
        e.character = kind == 2 ? (char16_t)vk : 0;
        e.unmodified_character = e.character;
        e.modifiers = modifiers;
        g_browser->GetHost()->SendKeyEvent(e);
      });
      break;
    }
    case kDevTools: {
      OnUi([]() {
        if (!g_browser) return;
        CefWindowInfo window;
        window.SetAsPopup(nullptr, "IVMP DevTools");
        g_browser->GetHost()->ShowDevTools(window, nullptr, CefBrowserSettings(), CefPoint());
      });
      break;
    }
    case kEmit: {
      // name\0json
      size_t zero = p.find('\0');
      if (zero == std::string::npos) break;
      std::string name = p.substr(0, zero), json = p.substr(zero + 1);
      OnUi([name, json]() {
        if (!g_browser) return;
        CefRefPtr<CefValue> name_value = CefValue::Create();
        name_value->SetString(name);
        std::string call = "window.ivmp && ivmp.__emit(" + CefWriteJSON(name_value, JSON_WRITER_DEFAULT).ToString() + "," + json + ");";
        g_browser->GetMainFrame()->ExecuteJavaScript(call, "ivmp://ui/emit", 0);
      });
      break;
    }
  }
}

bool ReadExactly(HANDLE h, void* out, DWORD size) {
  uint8_t* at = (uint8_t*)out;
  while (size) {
    DWORD got = 0;
    if (!ReadFile(h, at, size, &got, nullptr) || got == 0) return false;
    at += got;
    size -= got;
  }
  return true;
}

std::string ArgValue(CefRefPtr<CefCommandLine> line, const char* name) { return line->GetSwitchValue(name).ToString(); }

std::wstring Widen(const std::string& s) {
  if (s.empty()) return {};
  int n = MultiByteToWideChar(CP_UTF8, 0, s.data(), (int)s.size(), nullptr, 0);
  std::wstring out(n, 0);
  MultiByteToWideChar(CP_UTF8, 0, s.data(), (int)s.size(), out.data(), n);
  return out;
}

std::string ExeDir() {
  char path[MAX_PATH];
  GetModuleFileNameA(nullptr, path, MAX_PATH);
  std::string dir(path);
  return dir.substr(0, dir.find_last_of("\\/"));
}

}  // namespace

int APIENTRY wWinMain(HINSTANCE instance, HINSTANCE, LPWSTR, int) {
  CefMainArgs main_args(instance);
  CefRefPtr<ivmp::WebApp> app = new ivmp::WebApp();

  // Helper processes (renderer, GPU, ...) end here.
  int code = CefExecuteProcess(main_args, app, nullptr);
  if (code >= 0) return code;

  CefRefPtr<CefCommandLine> line = CefCommandLine::CreateCommandLine();
  line->InitFromString(::GetCommandLineW());
  std::string pipe_name = ArgValue(line, "ivmp-pipe");
  std::string frame_name = ArgValue(line, "ivmp-frame");
  std::string url = ArgValue(line, "ivmp-url");
  DWORD game_pid = (DWORD)atoi(ArgValue(line, "ivmp-game-pid").c_str());
  if (pipe_name.empty() || frame_name.empty() || url.empty()) return 3;

  // The frame mapping and the pipe, both created here; the game opens them.
  HANDLE mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, (DWORD)(kHeader + kMaxWidth * kMaxHeight * 4), Widen(frame_name).c_str());
  if (!mapping) return 4;
  uint8_t* view = (uint8_t*)MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, 0);
  if (!view) return 5;
  memset(view, 0, kHeader);
  g_frame_header = (volatile uint32_t*)view;
  g_frame_ring = (volatile uint32_t*)(view + 16);
  g_frame_data = view + kHeader;

  g_pipe = CreateNamedPipeW(Widen("\\\\.\\pipe\\" + pipe_name + "_out").c_str(), PIPE_ACCESS_OUTBOUND, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 1 << 16, 1 << 16, 0, nullptr);
  g_pipe_in = CreateNamedPipeW(Widen("\\\\.\\pipe\\" + pipe_name + "_in").c_str(), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 1 << 16, 1 << 16, 0, nullptr);
  if (g_pipe == INVALID_HANDLE_VALUE || g_pipe_in == INVALID_HANDLE_VALUE) return 6;
  // The game opens _out first, then _in.
  if (!ConnectNamedPipe(g_pipe, nullptr) && GetLastError() != ERROR_PIPE_CONNECTED) return 7;
  if (!ConnectNamedPipe(g_pipe_in, nullptr) && GetLastError() != ERROR_PIPE_CONNECTED) return 7;
  ivmp::g_log = LogToGame;

  std::string folder = ExeDir();
  ivmp::g_ui_dir = folder + "\\ui";
  ivmp::g_resource_dir = ArgValue(line, "ivmp-resources");

  app->on_context_initialized = [url]() {
    CefWindowInfo window;
    window.SetAsWindowless(nullptr);
    window.runtime_style = CEF_RUNTIME_STYLE_ALLOY;
    CefBrowserSettings settings;
    settings.windowless_frame_rate = 60;
    settings.background_color = 0;  // transparent
    settings.javascript_access_clipboard = STATE_DISABLED;
    settings.local_storage = STATE_DISABLED;
    CefBrowserHost::CreateBrowser(window, new Client(), url, settings, nullptr, nullptr);
  };

  CefSettings settings;
  settings.no_sandbox = true;
  settings.windowless_rendering_enabled = true;
  settings.multi_threaded_message_loop = true;
  settings.log_severity = LOGSEVERITY_WARNING;
  CefString(&settings.log_file) = Widen(folder) + L"\\cef.log";
  CefString(&settings.cache_path) = "";  // in-memory profile
  if (!CefInitialize(main_args, settings, app, nullptr)) {
    Log("web: CefInitialize failed");
    return 8;
  }
  Log("web: CEF %s started (own process)", CEF_VERSION);
  SendToGame(kReady, std::string((const char*)&kProtocol, 4));

  // Exit when the game goes away (pipe closes or the process ends).
  HANDLE game = game_pid ? OpenProcess(SYNCHRONIZE, FALSE, game_pid) : nullptr;
  std::thread([game]() {
    if (game) {
      WaitForSingleObject(game, INFINITE);
      ExitProcess(0);
    }
  }).detach();

  // The game's messages, until it closes the pipe.
  for (;;) {
    uint32_t length = 0;
    if (!ReadExactly(g_pipe_in, &length, 4) || length == 0 || length > (1 << 20)) break;
    std::string body(length, 0);
    if (!ReadExactly(g_pipe_in, body.data(), length)) break;
    Handle((uint8_t)body[0], body.substr(1));
  }
  CefShutdown();
  return 0;
}
