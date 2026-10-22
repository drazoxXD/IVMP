@echo off
rem Builds the CEF spike (32-bit). Needs the CEF windows32 minimal distribution extracted under
rem Client\web\cef (see docs/web-engine-research.md). Output: Client\web\spike\out\spike.exe
setlocal
set "HERE=%~dp0"
for /d %%D in ("%HERE%..\cef\cef_binary_*") do set "CEF=%%~fD"
if not defined CEF (echo CEF not found under Client\web\cef & exit /b 1)
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x86 >nul || exit /b 1

set "OUT=%HERE%out"
set "OBJ=%HERE%obj"
if not exist "%OUT%" mkdir "%OUT%"
if not exist "%OBJ%" mkdir "%OBJ%"

set "FLAGS=/nologo /MT /O2 /EHsc /std:c++20 /Zc:__cplusplus /DWIN32 /D_WINDOWS /DNOMINMAX /DWIN32_LEAN_AND_MEAN /D_HAS_ITERATOR_DEBUGGING=0 /DWRAPPING_CEF_SHARED /DUNICODE /D_UNICODE /I"%CEF%" /W0"

rem The wrapper library (libcef_dll_wrapper), once.
if not exist "%OBJ%\libcef_dll_wrapper.lib" (
  echo building libcef_dll_wrapper...
  if not exist "%OBJ%\wrapper" mkdir "%OBJ%\wrapper"
  cl %FLAGS% /MP /c /Fo"%OBJ%\wrapper\\" "%CEF%\libcef_dll\*.cc" "%CEF%\libcef_dll\base\*.cc" "%CEF%\libcef_dll\cpptoc\*.cc" "%CEF%\libcef_dll\cpptoc\test\*.cc" "%CEF%\libcef_dll\ctocpp\*.cc" "%CEF%\libcef_dll\ctocpp\test\*.cc" "%CEF%\libcef_dll\wrapper\*.cc" || exit /b 1
  lib /nologo /OUT:"%OBJ%\libcef_dll_wrapper.lib" "%OBJ%\wrapper\*.obj" || exit /b 1
)

cl %FLAGS% /Fo"%OBJ%\\" "%HERE%spike.cc" /Fe"%OUT%\spike.exe" /link /nologo /SUBSYSTEM:WINDOWS "%OBJ%\libcef_dll_wrapper.lib" "%CEF%\Release\libcef.lib" user32.lib gdi32.lib shell32.lib advapi32.lib ole32.lib || exit /b 1

rem Runtime files next to the exe.
xcopy /Y /Q /E "%CEF%\Release\*" "%OUT%\" >nul
xcopy /Y /Q /E "%CEF%\Resources\*" "%OUT%\" >nul
xcopy /Y /Q /E /I "%HERE%ui" "%OUT%\ui" >nul
echo done: %OUT%\spike.exe
