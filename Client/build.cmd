@echo off
rem Builds the IVMP client into out\<profile>\ (same as build.ps1).
rem   build.cmd          -^> out\debug\
rem   build.cmd release  -^> out\release\
set "IVMP_FLAGS="
if /i "%~1"=="release" set "IVMP_FLAGS=-Release"
if /i "%~1"=="-release" set "IVMP_FLAGS=-Release"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %IVMP_FLAGS%
exit /b %ERRORLEVEL%
