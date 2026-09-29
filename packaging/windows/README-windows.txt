OpenLustre Studio for Windows
=============================

Two downloads (64-bit Windows 10 or 11):

  OpenLustreStudio-<version>-windows-x86_64-Setup.exe
      The installer. Windows may show "Windows protected your PC" (the
      download is not code-signed): choose More info, then Run anyway.
      By default it installs for you only (no administrator rights needed);
      the wizard can install for everyone instead. It adds Start Menu
      shortcuts, can put the `openlustre` command on PATH, and uninstalls
      from Settings > Apps.

  openlustre-studio-<version>-windows-x86_64.zip
      Portable: unzip anywhere and run openlustre.exe from a command prompt
      (for example: openlustre studio launch --sample pms).

Start it: Start Menu > OpenLustre Studio (or "OpenLustre Studio - PMS
sample"). A console window shows the Studio's log (close it to stop the
Studio) and the Studio opens in your browser. Samples are copied to
%USERPROFILE%\OpenLustre\samples the first time you open them, so you can
edit them.

Generating and testing C needs a C compiler: Visual Studio Build Tools
(MSVC, found automatically) or MinGW-w64 gcc on PATH.

Proving needs Kind 2, which has no Windows build. Use it through WSL:
  1. wsl --install -d Ubuntu      (once, then restart)
  2. in Ubuntu: download kind2 from https://github.com/kind2-mc/kind2/releases
     (the linux-x86_64 archive) into /usr/local/bin, and: sudo apt install z3
  3. set OPENLUSTRE_KIND2 to the tools\kind2-wsl.cmd file in the install
     folder; `openlustre kind2 doctor` checks it.
Or with Docker Desktop: docker pull kind2/kind2:dev and point
OPENLUSTRE_KIND2 at tools\kind2-docker.sh (from Git Bash or WSL).
Everything else - modelling, simulation, code generation, tests with
MC/DC coverage, evidence reports - works without Kind 2.

OpenLustre Studio is for demonstration and prototyping; it is not a
qualified tool.
