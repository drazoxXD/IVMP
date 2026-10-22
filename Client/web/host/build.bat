@echo off
rem Builds the CEF part of the client, 32-bit: IVMP_Web.exe, CEF in its own process that is also the
rem helper process for CEF, and assembles the separate cef folder that sits next to IVMP_Client.dll.
rem Needs the CEF windows32 minimal distribution: the folder in the CEF_DIR environment variable,
rem or the cef_binary_* folder under Client\web\cef. Output: Client\web\host\out\cef
setlocal
set "HERE=%~dp0"
if not defined CEF_DIR for /d %%D in ("%HERE%..\cef\cef_binary_*") do set "CEF_DIR=%%~fD"
if not defined CEF_DIR goto :nocef
set "CEF=%CEF_DIR%"

rem The 32-bit MSVC tools: vswhere when there is one, else the Build Tools path.
set "VSROOT=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" for /f "usebackq delims=" %%I in (`"%VSWHERE%" -latest -products * -property installationPath`) do set "VSROOT=%%I"
call "%VSROOT%\VC\Auxiliary\Build\vcvarsall.bat" x86 >nul 2>&1
if errorlevel 1 goto :novs

set "OUT=%HERE%out\cef"
set "OBJ=%HERE%obj"
if not exist "%OUT%" mkdir "%OUT%"
if not exist "%OBJ%" mkdir "%OBJ%"

set "FLAGS=/nologo /MT /O2 /EHsc /std:c++20 /Zc:__cplusplus /DWIN32 /D_WINDOWS /DNOMINMAX /DWIN32_LEAN_AND_MEAN /D_HAS_ITERATOR_DEBUGGING=0 /DWRAPPING_CEF_SHARED /DUNICODE /D_UNICODE /I"%CEF%" /W0"
set "WRAP=%OBJ%\libcef_dll_wrapper.lib"

rem The wrapper library, once.
if not exist "%WRAP%" call :wrapper
if errorlevel 1 exit /b 1

cl %FLAGS% /Fo"%OBJ%\\" "%HERE%host_main.cc" /Fe"%OUT%\IVMP_Web.exe" /link /nologo /SUBSYSTEM:WINDOWS "%WRAP%" "%CEF%\Release\libcef.lib" user32.lib gdi32.lib shell32.lib advapi32.lib ole32.lib || exit /b 1

rem CEF's runtime, what the process needs and nothing else, and the pages.
xcopy /Y /Q /E "%CEF%\Release\*" "%OUT%\" >nul
xcopy /Y /Q /E "%CEF%\Resources\*" "%OUT%\" >nul
del /Q "%OUT%\bootstrap.exe" "%OUT%\bootstrapc.exe" "%OUT%\libcef.lib" 2>nul
xcopy /Y /Q /E /I "%HERE%ui" "%OUT%\ui" >nul
echo done: %OUT%
exit /b 0

:wrapper
echo building libcef_dll_wrapper
if not exist "%OBJ%\wrapper" mkdir "%OBJ%\wrapper"
cl %FLAGS% /MP /c /Fo"%OBJ%\wrapper\\" "%CEF%\libcef_dll\*.cc" "%CEF%\libcef_dll\base\*.cc" "%CEF%\libcef_dll\cpptoc\*.cc" "%CEF%\libcef_dll\cpptoc\test\*.cc" "%CEF%\libcef_dll\ctocpp\*.cc" "%CEF%\libcef_dll\ctocpp\test\*.cc" "%CEF%\libcef_dll\wrapper\*.cc" || exit /b 1
lib /nologo /OUT:"%WRAP%" "%OBJ%\wrapper\*.obj" || exit /b 1
exit /b 0

:nocef
echo CEF not found: set CEF_DIR or extract the minimal windows32 distribution under Client\web\cef
exit /b 1

:novs
echo the Visual Studio C++ tools were not found
exit /b 1