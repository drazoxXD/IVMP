// Web UI spike (docs/web-engine-research.md, Stage 2): a 32-bit exe that renders a page with CEF
// windowless, dumps one OnPaint frame to a PNG, serves the page from a custom ivmp:// scheme and
// proves that everything else is refused (the page tries fetch / XHR / WebSocket / <img> to the
// internet). The one allowed channel is the JS bridge `ivmpSend(name, json)`.
//
//   spike.exe [--engine-wall-only]
//
// --engine-wall-only turns the request-handler wall off, to show the engine-level switches (no DNS,
// no proxy) refuse the traffic on their own.

#include <windows.h>

#include <cstdio>
#include <cstdint>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_command_line.h"
#include "include/cef_life_span_handler.h"
#include "include/cef_parser.h"
#include "include/cef_version.h"
#include "include/cef_render_handler.h"
#include "include/cef_request_handler.h"
#include "include/cef_resource_request_handler.h"
#include "include/cef_scheme.h"
#include "include/wrapper/cef_helpers.h"
#include "include/wrapper/cef_message_router.h"
#include "include/wrapper/cef_stream_resource_handler.h"

namespace {

const int kWidth = 1280;
const int kHeight = 720;
const char kScheme[] = "ivmp";
const char kHost[] = "ui";

bool g_engine_wall_only = false;
bool g_frame_saved = false;
std::vector<uint8_t> g_frame;  // BGRA
FILE* g_log = nullptr;

void Log(const char* format, ...) {
  va_list args;
  va_start(args, format);
  char line[2048];
  vsnprintf(line, sizeof(line), format, args);
  va_end(args);
  printf("%s\n", line);
  if (g_log) {
    fprintf(g_log, "%s\n", line);
    fflush(g_log);
  }
}

std::string ExeDir() {
  char path[MAX_PATH];
  GetModuleFileNameA(nullptr, path, MAX_PATH);
  std::string dir(path);
  return dir.substr(0, dir.find_last_of("\\/"));
}

// ---- PNG writer (stored deflate blocks, no zlib) ----------------------------------------------
uint32_t Crc32(const uint8_t* data, size_t length, uint32_t crc = 0) {
  static uint32_t table[256];
  static bool ready = false;
  if (!ready) {
    for (uint32_t i = 0; i < 256; i++) {
      uint32_t c = i;
      for (int k = 0; k < 8; k++) c = (c & 1) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
      table[i] = c;
    }
    ready = true;
  }
  crc = ~crc;
  for (size_t i = 0; i < length; i++) crc = table[(crc ^ data[i]) & 0xFF] ^ (crc >> 8);
  return ~crc;
}

void PutU32(std::vector<uint8_t>& out, uint32_t v) {
  out.push_back(v >> 24);
  out.push_back(v >> 16);
  out.push_back(v >> 8);
  out.push_back(v);
}

void Chunk(std::vector<uint8_t>& png, const char* type, const std::vector<uint8_t>& data) {
  PutU32(png, (uint32_t)data.size());
  std::vector<uint8_t> body(type, type + 4);
  body.insert(body.end(), data.begin(), data.end());
  png.insert(png.end(), body.begin(), body.end());
  PutU32(png, Crc32(body.data(), body.size()));
}

bool WritePng(const std::string& path, const uint8_t* bgra, int width, int height) {
  std::vector<uint8_t> raw;
  raw.reserve((size_t)(width * 4 + 1) * height);
  for (int y = 0; y < height; y++) {
    raw.push_back(0);  // filter: none
    for (int x = 0; x < width; x++) {
      const uint8_t* p = bgra + ((size_t)y * width + x) * 4;
      raw.push_back(p[2]);
      raw.push_back(p[1]);
      raw.push_back(p[0]);
      raw.push_back(p[3]);
    }
  }
  std::vector<uint8_t> z = {0x78, 0x01};
  uint32_t a = 1, b = 0;
  for (uint8_t v : raw) {
    a = (a + v) % 65521;
    b = (b + a) % 65521;
  }
  for (size_t pos = 0; pos < raw.size();) {
    size_t n = std::min<size_t>(65535, raw.size() - pos);
    z.push_back(pos + n == raw.size() ? 1 : 0);
    z.push_back(n & 0xFF);
    z.push_back(n >> 8);
    z.push_back(~n & 0xFF);
    z.push_back((~n >> 8) & 0xFF);
    z.insert(z.end(), raw.begin() + pos, raw.begin() + pos + n);
    pos += n;
  }
  PutU32(z, (b << 16) | a);
  std::vector<uint8_t> png = {0x89, 'P', 'N', 'G', 0x0D, 0x0A, 0x1A, 0x0A};
  std::vector<uint8_t> ihdr;
  PutU32(ihdr, width);
  PutU32(ihdr, height);
  ihdr.insert(ihdr.end(), {8, 6, 0, 0, 0});
  Chunk(png, "IHDR", ihdr);
  Chunk(png, "IDAT", z);
  Chunk(png, "IEND", {});
  std::ofstream file(path, std::ios::binary);
  file.write((const char*)png.data(), png.size());
  return file.good();
}

// ---- ivmp:// scheme: files from the ui folder, nothing else -----------------------------------
std::string MimeOf(const std::string& path) {
  auto ends = [&](const char* e) { return path.size() >= strlen(e) && path.compare(path.size() - strlen(e), strlen(e), e) == 0; };
  if (ends(".html")) return "text/html";
  if (ends(".js")) return "text/javascript";
  if (ends(".css")) return "text/css";
  if (ends(".png")) return "image/png";
  if (ends(".json")) return "application/json";
  return "application/octet-stream";
}

class SchemeFactory : public CefSchemeHandlerFactory {
 public:
  CefRefPtr<CefResourceHandler> Create(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, const CefString&, CefRefPtr<CefRequest> request) override {
    CefURLParts parts;
    CefParseURL(request->GetURL(), parts);
    std::string host = CefString(&parts.host).ToString();
    std::string path = CefString(&parts.path).ToString();
    if (host != kHost || path.find("..") != std::string::npos) {
      Log("scheme: refused %s", request->GetURL().ToString().c_str());
      return nullptr;
    }
    if (path.empty() || path == "/") path = "/index.html";
    std::ifstream file(ExeDir() + "\\ui" + path, std::ios::binary);
    if (!file) {
      Log("scheme: not found %s", path.c_str());
      return nullptr;
    }
    std::stringstream buffer;
    buffer << file.rdbuf();
    std::string data = buffer.str();
    return new CefStreamResourceHandler(MimeOf(path), CefStreamReader::CreateForData((void*)data.data(), data.size()));
  }
  IMPLEMENT_REFCOUNTING(SchemeFactory);
};

// ---- the wall: every request that is not ivmp:// is cancelled ---------------------------------
class WallResourceRequestHandler : public CefResourceRequestHandler {
 public:
  ReturnValue OnBeforeResourceLoad(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest> request, CefRefPtr<CefCallback>) override {
    std::string url = request->GetURL().ToString();
    if (url.rfind("ivmp://", 0) == 0) return RV_CONTINUE;
    if (g_engine_wall_only) {
      Log("wall: (handler wall off) letting %s through to the engine", url.c_str());
      return RV_CONTINUE;
    }
    Log("wall: cancelled %s", url.c_str());
    return RV_CANCEL;
  }
  IMPLEMENT_REFCOUNTING(WallResourceRequestHandler);
};

// ---- the one bridge: ivmpSend(name, json) -> here ---------------------------------------------
class BridgeHandler : public CefMessageRouterBrowserSide::Handler {
 public:
  bool OnQuery(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int64_t, const CefString& request, bool, CefRefPtr<Callback> callback) override {
    Log("bridge: page sent %s", request.ToString().c_str());
    callback->Success("ok");
    return true;
  }
};

class Client : public CefClient, public CefRenderHandler, public CefRequestHandler, public CefLifeSpanHandler, public CefLoadHandler, public CefDisplayHandler {
 public:
  Client() {
    CefMessageRouterConfig config;
    router_ = CefMessageRouterBrowserSide::Create(config);
    router_->AddHandler(&bridge_, false);
  }
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }

  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame, CefProcessId source, CefRefPtr<CefProcessMessage> message) override {
    return router_->OnProcessMessageReceived(browser, frame, source, message);
  }

  // Rendering: one windowless 1280x720 view.
  void GetViewRect(CefRefPtr<CefBrowser>, CefRect& rect) override { rect = CefRect(0, 0, kWidth, kHeight); }
  void OnPaint(CefRefPtr<CefBrowser>, PaintElementType type, const RectList& dirty, const void* buffer, int width, int height) override {
    if (type != PET_VIEW) return;
    g_frame.assign((const uint8_t*)buffer, (const uint8_t*)buffer + (size_t)width * height * 4);
    frame_w_ = width;
    frame_h_ = height;
    paints_++;
    if (paints_ <= 3) Log("paint: %dx%d, %zu dirty rect(s)", width, height, dirty.size());
  }

  // Navigation and popups: only ivmp://, no popups.
  bool OnBeforeBrowse(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame>, CefRefPtr<CefRequest> request, bool, bool) override {
    std::string url = request->GetURL().ToString();
    bool allowed = url.rfind("ivmp://", 0) == 0;
    if (!allowed) Log("wall: navigation to %s refused", url.c_str());
    return !allowed;
  }
  CefRefPtr<CefResourceRequestHandler> GetResourceRequestHandler(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest>, bool, bool, const CefString&, bool&) override {
    return new WallResourceRequestHandler();
  }
  bool OnBeforePopup(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int, const CefString& url, const CefString&, CefLifeSpanHandler::WindowOpenDisposition, bool, const CefPopupFeatures&, CefWindowInfo&, CefRefPtr<CefClient>&, CefBrowserSettings&, CefRefPtr<CefDictionaryValue>&, bool*) override {
    Log("wall: popup %s refused", url.ToString().c_str());
    return true;
  }
  void OnBeforeClose(CefRefPtr<CefBrowser> browser) override { router_->OnBeforeClose(browser); }
  bool OnConsoleMessage(CefRefPtr<CefBrowser>, cef_log_severity_t, const CefString& message, const CefString&, int) override {
    Log("page console: %s", message.ToString().c_str());
    return false;
  }
  void OnLoadEnd(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, int status) override {
    if (frame->IsMain()) {
      Log("load: finished (status %d)", status);
      loaded_at_ = GetTickCount();
    }
  }

  // After the page had time to run its tests, save the frame.
  bool Done() {
    if (loaded_at_ && GetTickCount() - loaded_at_ > 6000 && !g_frame.empty()) {
      std::string out = ExeDir() + "\\spike_frame.png";
      Log("frame: %s (%dx%d, %d paints)", WritePng(out, g_frame.data(), frame_w_, frame_h_) ? out.c_str() : "write failed", frame_w_, frame_h_, paints_);
      g_frame_saved = true;
      return true;
    }
    return false;
  }

  IMPLEMENT_REFCOUNTING(Client);

 private:
  CefRefPtr<CefMessageRouterBrowserSide> router_;
  BridgeHandler bridge_;
  int frame_w_ = 0, frame_h_ = 0, paints_ = 0;
  DWORD loaded_at_ = 0;
};

class App : public CefApp, public CefBrowserProcessHandler, public CefRenderProcessHandler {
 public:
  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override { return this; }
  CefRefPtr<CefRenderProcessHandler> GetRenderProcessHandler() override { return this; }

  void OnRegisterCustomSchemes(CefRawPtr<CefSchemeRegistrar> registrar) override {
    registrar->AddCustomScheme(kScheme, CEF_SCHEME_OPTION_STANDARD | CEF_SCHEME_OPTION_SECURE | CEF_SCHEME_OPTION_CORS_ENABLED | CEF_SCHEME_OPTION_FETCH_ENABLED);
  }

  // The engine-level wall: no DNS, no proxy, no background traffic, nothing that phones home.
  void OnBeforeCommandLineProcessing(const CefString&, CefRefPtr<CefCommandLine> line) override {
    line->AppendSwitchWithValue("host-resolver-rules", "MAP * ~NOTFOUND");
    line->AppendSwitchWithValue("proxy-server", "direct://");
    line->AppendSwitch("no-proxy-server");
    line->AppendSwitch("disable-background-networking");
    line->AppendSwitch("disable-component-update");
    line->AppendSwitch("disable-sync");
    line->AppendSwitch("disable-extensions");
    line->AppendSwitch("disable-domain-reliability");
    line->AppendSwitch("disable-client-side-phishing-detection");
    line->AppendSwitch("disable-default-apps");
    line->AppendSwitch("no-pings");
    line->AppendSwitchWithValue("disable-features", "MediaRouter,Translate,WebRtcHideLocalIpsWithMdns");
    line->AppendSwitch("enable-begin-frame-scheduling");
  }

  void OnContextInitialized() override {
    CefRegisterSchemeHandlerFactory(kScheme, kHost, new SchemeFactory());
    CefWindowInfo window;
    window.SetAsWindowless(nullptr);
    window.runtime_style = CEF_RUNTIME_STYLE_ALLOY;
    CefBrowserSettings browser_settings;
    browser_settings.windowless_frame_rate = 30;
    browser_settings.background_color = 0;  // transparent
    browser_settings.javascript_access_clipboard = STATE_DISABLED;
    browser_settings.local_storage = STATE_DISABLED;
    client_ = new Client();
    CefBrowserHost::CreateBrowser(window, client_, "ivmp://ui/index.html", browser_settings, nullptr, nullptr);
  }

  // Renderer side of the bridge: window.cefQuery for ivmpSend.
  void OnWebKitInitialized() override {
    CefMessageRouterConfig config;
    renderer_router_ = CefMessageRouterRendererSide::Create(config);
  }
  void OnContextCreated(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame, CefRefPtr<CefV8Context> context) override {
    renderer_router_->OnContextCreated(browser, frame, context);
  }
  void OnContextReleased(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame, CefRefPtr<CefV8Context> context) override {
    renderer_router_->OnContextReleased(browser, frame, context);
  }
  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame, CefProcessId source, CefRefPtr<CefProcessMessage> message) override {
    return renderer_router_->OnProcessMessageReceived(browser, frame, source, message);
  }

  CefRefPtr<Client> client_;
  IMPLEMENT_REFCOUNTING(App);

 private:
  CefRefPtr<CefMessageRouterRendererSide> renderer_router_;
};

}  // namespace

int APIENTRY wWinMain(HINSTANCE instance, HINSTANCE, LPWSTR, int) {
  CefMainArgs main_args(instance);
  CefRefPtr<App> app = new App();

  // Helper processes (renderer, GPU, ...) run this same exe and end here.
  int code = CefExecuteProcess(main_args, app, nullptr);
  if (code >= 0) return code;

  AllocConsole();
  freopen("CONOUT$", "w", stdout);
  g_log = fopen((ExeDir() + "\\spike.log").c_str(), "w");
  CefRefPtr<CefCommandLine> line = CefCommandLine::CreateCommandLine();
  line->InitFromString(::GetCommandLineW());
  g_engine_wall_only = line->HasSwitch("engine-wall-only");
  Log("spike: CEF %s, engine-wall-only=%d", CEF_VERSION, g_engine_wall_only);

  CefSettings settings;
  settings.no_sandbox = true;  // the spike; the game build keeps the helper sandbox if it can
  settings.windowless_rendering_enabled = true;
  settings.log_severity = LOGSEVERITY_WARNING;
  CefString(&settings.cache_path) = "";  // in-memory profile, nothing on disk
  if (!CefInitialize(main_args, settings, app, nullptr)) {
    Log("spike: CefInitialize failed");
    return 1;
  }
  DWORD started = GetTickCount();
  while (GetTickCount() - started < 40000) {
    CefDoMessageLoopWork();
    if (app->client_ && app->client_->Done()) break;
    Sleep(5);
  }
  if (!g_frame_saved) Log("spike: timed out without a frame");
  CefShutdown();
  return g_frame_saved ? 0 : 2;
}
