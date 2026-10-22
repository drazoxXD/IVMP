@echo off
rem Builds the CEF part of the client 32-bit: IVMP_Web.dll loaded by the game and IVMP_Web.exe
rem the helper processes, and assembles the separate "cef" folder that sits next to
rem IVMP_Client.dll. Needs the CEF windows32 minimal distribution under Client\web\cef.
rem Output: Client\web\host\out\cef
setlocal
set "HERE=%~dp0"
set "CEF=%HERE%..\cef\cef_binary_156.0.7+g47dbc1d+chromium-156.0.8078.17_windows32_minimal"
if not defined CEF (echo CEF not found under Client\web\cef & exit /b 1)
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x86 >nul 2>&1 || exit /b 1

set "OUT=%HERE%out\cef"
set "OBJ=%HERE%obj"
if not exist "%OUT%" mkdir "%OUT%"
if not exist "%OBJ%" mkdir "%OBJ%"

set "FLAGS=/nologo /MT /O2 /EHsc /std:c++20 /Zc:__cplusplus /DWIN32 /D_WINDOWS /DNOMINMAX /DWIN32_LEAN_AND_MEAN /D_HAS_ITERATOR_DEBUGGING=0 /DWRAPPING_CEF_SHARED /DUNICODE /D_UNICODE /I"%CEF%" /W0"

rem The wrapper library is shared with the spike build.
set "WRAP=%HERE%..\spike\obj\libcef_dll_wrapper.lib"
if not exist "%WRAP%" echo build the spike first for libcef_dll_wrapper.lib & exit /b 1

cl %FLAGS% /Fo"%OBJ%\\" "%HERE%web_host.cc" /LD /Fe"%OUT%\IVMP_Web.dll" /link /nologo "%WRAP%" "%CEF%\Release\libcef.lib" user32.lib gdi32.lib shell32.lib advapi32.lib ole32.lib || exit /b 1
cl %FLAGS% /Fo"%OBJ%\\" "%HERE%helper.cc" /Fe"%OUT%\IVMP_Web.exe" /link /nologo /SUBSYSTEM:WINDOWS "%WRAP%" "%CEF%\Release\libcef.lib" user32.lib gdi32.lib shell32.lib advapi32.lib ole32.lib || exit /b 1

rem CEF's runtime what the DLL and helper need, nothing else and the pages.
xcopy /Y /Q /E "%CEF%\Release\*" "%OUT%\" >nul
xcopy /Y /Q /E "%CEF%\Resources\*" "%OUT%\" >nul
del /Q "%OUT%\bootstrap.exe" "%OUT%\bootstrapc.exe" "%OUT%\libcef.lib" 2>nul
xcopy /Y /Q /E /I "%HERE%ui" "%OUT%\ui" >nul
echo done: %OUT%
