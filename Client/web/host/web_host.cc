// IVMP_Web.dll: CEF for the game, behind a small C API so the Rust client never touches C++.
// The game loads it from the web runtime pack folder; it starts CEF windowless, keeps the latest
// BGRA frame for the client to copy into a Direct3D 9 texture, and forwards input. The page is
// locked down (web_app.h): only ivmp://, and ivmp.send is its one way out.
#include <atomic>
#include <mutex>
#include <vector>

#include <functional>
#include "include/cef_client.h"
#include "include/cef_life_span_handler.h"
#include "include/cef_version.h"
#include "include/cef_render_handler.h"
#include "include/wrapper/cef_closure_task.h"
#include "web_app.h"

#define EXPORT extern "C" __declspec(dllexport)

namespace {

using ivmp::Log;

void (*g_bridge)(const char*) = nullptr;

std::atomic<int> g_width{1280};
std::atomic<int> g_height{720};

// The latest frame, copied by the client under the lock.
std::mutex g_frame_lock;
std::vector<uint8_t> g_frame;
int g_frame_w = 0, g_frame_h = 0;
std::atomic<unsigned> g_frame_version{0};

CefRefPtr<CefBrowser> g_browser;
CefRefPtr<ivmp::WebApp> g_app;
bool g_started = false;
HMODULE g_module_dir_anchor = nullptr;

class BridgeHandler : public CefMessageRouterBrowserSide::Handler {
 public:
  bool OnQuery(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int64_t, const CefString& request, bool, CefRefPtr<Callback> callback) override {
    std::string text = request.ToString();
    if (text.size() > 64 * 1024) {
      callback->Failure(1, "message too large");
      return true;
    }
    if (g_bridge) g_bridge(text.c_str());
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

  void OnPaint(CefRefPtr<CefBrowser>, PaintElementType type, const RectList&, const void* buffer, int width, int height) override {
    if (type != PET_VIEW) return;
    std::lock_guard<std::mutex> lock(g_frame_lock);
    g_frame.assign((const uint8_t*)buffer, (const uint8_t*)buffer + (size_t)width * height * 4);
    g_frame_w = width;
    g_frame_h = height;
    g_frame_version++;
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
  bool OnConsoleMessage(CefRefPtr<CefBrowser>, cef_log_severity_t, const CefString& message, const CefString&, int) override {
    Log("web: page: %s", message.ToString().c_str());
    return false;
  }
  void OnLoadEnd(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, int status) override {
    if (frame->IsMain()) Log("web: page loaded (status %d)", status);
  }

  IMPLEMENT_REFCOUNTING(Client);

 private:
  CefRefPtr<CefMessageRouterBrowserSide> router_;
  BridgeHandler bridge_;
};

std::string Narrow(const std::wstring& w) {
  if (w.empty()) return {};
  int n = WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), nullptr, 0, nullptr, nullptr);
  std::string out(n, 0);
  WideCharToMultiByte(CP_UTF8, 0, w.data(), (int)w.size(), out.data(), n, nullptr, nullptr);
  return out;
}

// Runs the closure on CEF's UI thread (CEF's own bind only takes stateless lambdas, so a task).
class FnTask : public CefTask {
 public:
  explicit FnTask(std::function<void()> f) : f_(std::move(f)) {}
  void Execute() override { f_(); }
  IMPLEMENT_REFCOUNTING(FnTask);

 private:
  std::function<void()> f_;
};

template <typename F>
void OnUi(F&& f) {
  CefPostTask(TID_UI, new FnTask(std::function<void()>(std::forward<F>(f))));
}

CefMouseEvent MouseAt(int x, int y) {
  CefMouseEvent e;
  e.x = x;
  e.y = y;
  e.modifiers = 0;
  return e;
}

}  // namespace

EXPORT void ivmp_web_set_log(void (*callback)(const char*)) { ivmp::g_log = callback; }
EXPORT void ivmp_web_set_bridge(void (*callback)(const char*)) { g_bridge = callback; }

// dir: the CEF folder next to IVMP_Client.dll (holds libcef.dll, IVMP_Web.exe and ui\). Returns 1
// on success.
EXPORT int ivmp_web_start(const wchar_t* dir, int width, int height, const char* url) {
  if (g_started) return 1;
  std::wstring folder(dir);
  ivmp::g_ui_dir = Narrow(folder) + "\\ui";
  g_width = width;
  g_height = height;

  CefMainArgs main_args(GetModuleHandle(nullptr));
  g_app = new ivmp::WebApp();
  std::string start_url = url;
  g_app->on_context_initialized = [start_url]() {
    CefWindowInfo window;
    window.SetAsWindowless(nullptr);
    window.runtime_style = CEF_RUNTIME_STYLE_ALLOY;
    CefBrowserSettings settings;
    settings.windowless_frame_rate = 60;
    settings.background_color = 0;  // transparent
    settings.javascript_access_clipboard = STATE_DISABLED;
    settings.local_storage = STATE_DISABLED;
    CefBrowserHost::CreateBrowser(window, new Client(), start_url, settings, nullptr, nullptr);
  };

  CefSettings settings;
  settings.no_sandbox = true;
  settings.windowless_rendering_enabled = true;
  settings.multi_threaded_message_loop = true;  // CEF's own UI thread, never the game's loop
  settings.log_severity = LOGSEVERITY_WARNING;
  CefString(&settings.browser_subprocess_path) = folder + L"\\IVMP_Web.exe";
  CefString(&settings.resources_dir_path) = folder;
  CefString(&settings.locales_dir_path) = folder + L"\\locales";
  CefString(&settings.log_file) = folder + L"\\cef.log";
  CefString(&settings.cache_path) = "";  // in-memory profile
  if (!CefInitialize(main_args, settings, g_app, nullptr)) {
    Log("web: CefInitialize failed");
    return 0;
  }
  g_started = true;
  Log("web: CEF %s started", CEF_VERSION);
  return 1;
}

EXPORT void ivmp_web_shutdown() {
  if (!g_started) return;
  CefShutdown();
  g_started = false;
}

EXPORT void ivmp_web_resize(int width, int height) {
  if (width == g_width && height == g_height) return;
  g_width = width;
  g_height = height;
  OnUi([]() {
    if (g_browser) g_browser->GetHost()->WasResized();
  });
}

EXPORT unsigned ivmp_web_frame_version() { return g_frame_version; }

// Copies the latest frame into dst (BGRA, pitch bytes per row); returns the frame's version, or
// 0 when there is no frame of that size yet.
EXPORT unsigned ivmp_web_copy_frame(uint8_t* dst, int pitch, int width, int height) {
  std::lock_guard<std::mutex> lock(g_frame_lock);
  if (g_frame.empty() || g_frame_w != width || g_frame_h != height) return 0;
  for (int y = 0; y < height; y++) memcpy(dst + (size_t)y * pitch, g_frame.data() + (size_t)y * width * 4, (size_t)width * 4);
  return g_frame_version;
}

EXPORT void ivmp_web_visible(int visible) {
  OnUi([visible]() {
    if (g_browser) {
      g_browser->GetHost()->WasHidden(!visible);
      if (!visible) g_browser->GetHost()->SetFocus(false);
    }
  });
}

EXPORT void ivmp_web_focus(int focus) {
  OnUi([focus]() {
    if (g_browser) g_browser->GetHost()->SetFocus(focus != 0);
  });
}

EXPORT void ivmp_web_mouse_move(int x, int y) {
  OnUi([x, y]() {
    if (g_browser) g_browser->GetHost()->SendMouseMoveEvent(MouseAt(x, y), false);
  });
}

// button: 0 left, 1 right, 2 middle
EXPORT void ivmp_web_mouse_button(int button, int down, int x, int y) {
  OnUi([button, down, x, y]() {
    if (!g_browser) return;
    CefBrowserHost::MouseButtonType type = button == 1 ? MBT_RIGHT : button == 2 ? MBT_MIDDLE : MBT_LEFT;
    g_browser->GetHost()->SendMouseClickEvent(MouseAt(x, y), type, down == 0, 1);
  });
}

EXPORT void ivmp_web_mouse_wheel(int x, int y, int delta) {
  OnUi([x, y, delta]() {
    if (g_browser) g_browser->GetHost()->SendMouseWheelEvent(MouseAt(x, y), 0, delta);
  });
}

// type: 0 key down, 1 key up, 2 character. vk is the Windows virtual key (or the character for
// type 2), lparam the WM_KEY* lParam.
EXPORT void ivmp_web_key(int type, int vk, int lparam, int modifiers) {
  OnUi([type, vk, lparam, modifiers]() {
    if (!g_browser) return;
    CefKeyEvent e;
    e.type = type == 0 ? KEYEVENT_RAWKEYDOWN : type == 1 ? KEYEVENT_KEYUP : KEYEVENT_CHAR;
    e.windows_key_code = vk;
    e.native_key_code = lparam;
    e.is_system_key = false;
    e.character = type == 2 ? (char16_t)vk : 0;
    e.unmodified_character = e.character;
    e.modifiers = modifiers;
    g_browser->GetHost()->SendKeyEvent(e);
  });
}

// Tells the page something: calls ivmp.__emit(name, json) in it.
EXPORT void ivmp_web_emit(const char* name, const char* json) {
  std::string n = name, j = json;
  OnUi([n, j]() {
    if (!g_browser) return;
    CefRefPtr<CefValue> name_value = CefValue::Create();
    name_value->SetString(n);
    std::string call = "window.ivmp && ivmp.__emit(" + CefWriteJSON(name_value, JSON_WRITER_DEFAULT).ToString() + "," + j + ");";
    g_browser->GetMainFrame()->ExecuteJavaScript(call, "ivmp://ui/emit", 0);
  });
}

// True once the page has painted a first frame.
EXPORT int ivmp_web_ready() { return g_frame_version > 0; }
