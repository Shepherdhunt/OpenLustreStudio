@echo off
rem Run Kind 2 inside WSL (Kind 2 has no native Windows build).
rem
rem Once, in a WSL Ubuntu shell:
rem   curl -fsSL -o kind2.tgz https://github.com/kind2-mc/kind2/releases/download/v2.2.0/kind2-v2.2.0-linux-x86_64.tar.gz
rem   tar xzf kind2.tgz && sudo install kind2 /usr/local/bin/ && sudo apt install -y z3
rem Then point OpenLustre at this script:
rem   setx OPENLUSTRE_KIND2 "C:\path\to\tools\kind2-wsl.cmd"
rem
rem Windows paths among the arguments are translated with `wslpath`; the
rem --z3_bin path OpenLustre may add for a Windows solver is dropped (Kind 2
rem uses the z3 installed inside WSL).
setlocal enabledelayedexpansion
set "ARGS="
set "SKIP="
for %%a in (%*) do (
  set "A=%%~a"
  if defined SKIP (
    set "SKIP="
  ) else if /i "!A!"=="--z3_bin" (
    set "SKIP=1"
  ) else if /i "!A!"=="--cvc5_bin" (
    set "SKIP=1"
  ) else if /i "!A!"=="--yices2_bin" (
    set "SKIP=1"
  ) else (
    if exist "!A!" (
      for /f "usebackq delims=" %%p in (`wsl wslpath -a "!A!"`) do set "A=%%p"
    )
    set ARGS=!ARGS! "!A!"
  )
)
wsl kind2 !ARGS!
exit /b %ERRORLEVEL%
