// Shared by IVMP_Web.dll (the browser process side, inside the game) and IVMP_Web.exe (the helper
// processes CEF starts: renderer, GPU, ...). Both must register the same scheme and run the
// renderer side of the bridge, so they use this one App.
//
// Lockdown (docs/web-engine-research.md): the page can only load ivmp:// and can only talk to the
// client through ivmp.send / ivmp.on.
#pragma once

#include <windows.h>

#include <algorithm>
#include <cstdarg>
#include <cstdio>
#include <fstream>
#include <functional>
#include <sstream>
#include <string>

#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_command_line.h"
#include "include/cef_parser.h"
#include "include/cef_request_handler.h"
#include "include/cef_resource_request_handler.h"
#include "include/cef_scheme.h"
#include "include/wrapper/cef_message_router.h"
#include "include/wrapper/cef_stream_resource_handler.h"

namespace ivmp {

inline const char* kScheme = "ivmp";
inline const char* kHost = "ui";

// Where the page files are (set by the DLL; the helper never serves pages).
inline std::string g_ui_dir;
// Where the files of the resources the server sent are: <dir>/<resource>/<path>.
inline std::string g_resource_dir;
// The log sink (a callback into the game's log); null in helpers.
inline void (*g_log)(const char*) = nullptr;

inline void Log(const char* format, ...) {
  if (!g_log) return;
  va_list args;
  va_start(args, format);
  char line[2048];
  vsnprintf(line, sizeof(line), format, args);
  va_end(args);
  g_log(line);
}

inline std::string MimeOf(const std::string& path) {
  auto ends = [&](const char* e) { return path.size() >= strlen(e) && path.compare(path.size() - strlen(e), strlen(e), e) == 0; };
  if (ends(".html")) return "text/html";
  if (ends(".js")) return "text/javascript";
  if (ends(".css")) return "text/css";
  if (ends(".png")) return "image/png";
  if (ends(".svg")) return "image/svg+xml";
  if (ends(".json")) return "application/json";
  if (ends(".woff2")) return "font/woff2";
  if (ends(".woff")) return "font/woff";
  if (ends(".ttf")) return "font/ttf";
  if (ends(".jpg") || ends(".jpeg")) return "image/jpeg";
  if (ends(".gif")) return "image/gif";
  if (ends(".webp")) return "image/webp";
  if (ends(".mp3")) return "audio/mpeg";
  if (ends(".ogg")) return "audio/ogg";
  if (ends(".wav")) return "audio/wav";
  return "application/octet-stream";
}

// A resource's name in a URL host: letters, digits, '_', '-' and '.', nothing that is a path.
inline bool ValidResourceName(const std::string& name) {
  if (name.empty() || name[0] == '.') return false;
  for (char c : name) {
    if (!isalnum((unsigned char)c) && c != '_' && c != '-' && c != '.') return false;
  }
  return name.find("..") == std::string::npos;
}

// A resource page gets ivmp://ui/nui.js first thing (callbacks, FiveM's http://<resource>/ posts and
// GetParentResourceName), right after <head>, or after the doctype when there is no head.
inline void InjectNui(std::string& html) {
  const std::string tag = "<script src=\"ivmp://ui/nui.js\"></script>";
  std::string lower(html.size(), '\0');
  std::transform(html.begin(), html.end(), lower.begin(), [](unsigned char c) { return (char)tolower(c); });
  size_t at = lower.find("<head");
  if (at != std::string::npos) at = lower.find('>', at);
  if (at == std::string::npos && lower.rfind("<!doctype", 0) == 0) at = lower.find('>');
  html.insert(at == std::string::npos ? 0 : at + 1, tag);
}

// ivmp://ui/<path>: files from the ui folder. ivmp://<resource>/<path>: files of that resource from
// the folder the server's files were downloaded to. Nothing else.
class SchemeFactory : public CefSchemeHandlerFactory {
 public:
  CefRefPtr<CefResourceHandler> Create(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, const CefString&, CefRefPtr<CefRequest> request) override {
    CefURLParts parts;
    CefParseURL(request->GetURL(), parts);
    std::string host = CefString(&parts.host).ToString();
    std::string path = CefString(&parts.path).ToString();
    std::string base;
    if (host == kHost) {
      base = g_ui_dir;
    } else if (ValidResourceName(host) && !g_resource_dir.empty()) {
      base = g_resource_dir + "\\" + host;
    }
    if (base.empty() || path.find("..") != std::string::npos || path.find('\\') != std::string::npos) {
      Log("web: scheme refused %s", request->GetURL().ToString().c_str());
      return nullptr;
    }
    if (path.empty() || path == "/") path = "/index.html";
    std::ifstream file(base + path, std::ios::binary);
    if (!file) {
      Log("web: scheme: not found %s", path.c_str());
      return nullptr;
    }
    std::stringstream buffer;
    buffer << file.rdbuf();
    std::string data = buffer.str();
    if (host != kHost && MimeOf(path) == "text/html") InjectNui(data);
    // Files of another resource are fetched by the shell page (the audio file of a sound): same scheme,
    // another host, so the response says it may be read.
    CefResponse::HeaderMap headers;
    headers.insert(std::make_pair(CefString("Access-Control-Allow-Origin"), CefString("*")));
    return new CefStreamResourceHandler(200, "OK", MimeOf(path), headers, CefStreamReader::CreateForData((void*)data.data(), data.size()));
  }
  IMPLEMENT_REFCOUNTING(SchemeFactory);
};

class WebApp : public CefApp, public CefRenderProcessHandler {
 public:
  // Set by the DLL: runs once the browser context exists (UI thread).
  std::function<void()> on_context_initialized;

  class BrowserSide : public CefBrowserProcessHandler {
   public:
    explicit BrowserSide(WebApp* app) : app_(app) {}
    void OnContextInitialized() override {
      // Every host of the scheme: "ui" and one per resource.
      CefRegisterSchemeHandlerFactory(kScheme, "", new SchemeFactory());
      if (app_->on_context_initialized) app_->on_context_initialized();
    }
    IMPLEMENT_REFCOUNTING(BrowserSide);

   private:
    WebApp* app_;
  };

  WebApp() : browser_side_(new BrowserSide(this)) {}
  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override { return browser_side_; }
  CefRefPtr<CefRenderProcessHandler> GetRenderProcessHandler() override { return this; }

  void OnRegisterCustomSchemes(CefRawPtr<CefSchemeRegistrar> registrar) override {
    registrar->AddCustomScheme(kScheme, CEF_SCHEME_OPTION_STANDARD | CEF_SCHEME_OPTION_SECURE | CEF_SCHEME_OPTION_CORS_ENABLED | CEF_SCHEME_OPTION_FETCH_ENABLED);
  }

  // The engine-level wall: no DNS, no proxy, nothing that phones home.
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
    // Sounds play without a click on the page (the page is never clicked before a sound starts).
    line->AppendSwitchWithValue("autoplay-policy", "no-user-gesture-required");
    line->AppendSwitchWithValue("disable-features", "MediaRouter,Translate,WebRtcHideLocalIpsWithMdns");
  }

  // Renderer side of the bridge: window.cefQuery, wrapped by ui/ivmp.js as ivmp.send.
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

  IMPLEMENT_REFCOUNTING(WebApp);

 private:
  CefRefPtr<BrowserSide> browser_side_;
  CefRefPtr<CefMessageRouterRendererSide> renderer_router_;
};

}  // namespace ivmp
