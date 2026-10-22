// IVMP_Web.exe: the helper process CEF starts for the renderer, GPU and utility work. It does
// nothing but run CEF's subprocess code with the same App (scheme + renderer-side bridge).
#include "web_app.h"

int APIENTRY wWinMain(HINSTANCE instance, HINSTANCE, LPWSTR, int) {
  CefMainArgs main_args(instance);
  CefRefPtr<ivmp::WebApp> app = new ivmp::WebApp();
  return CefExecuteProcess(main_args, app, nullptr);
}
