OpenLustre Studio for macOS
===========================

Needs macOS 13 (Ventura) or later. Three ways to install (pick the download
for your Mac's chip: arm64 for Apple Silicon M1/M2/M3/M4, x86_64 for
Intel):

  .dmg      Open it and drag "OpenLustre Studio" onto the Applications
            folder in the same window — the standard macOS install. macOS
            may say it "can't be opened because it is from an unidentified
            developer" (the download is not signed by Apple): right-click
            the .dmg, choose Open, then Open again. Dragging installs the
            apps; run the install.sh you see beside the folder afterwards
            if you also want the `openlustre` terminal command.

  .pkg      Double-click it. macOS may say it "can't be opened because it
            is from an unidentified developer" (the download is not signed
            by Apple): right-click the .pkg, choose Open, then Open again.
            Installs the "OpenLustre Studio" folder in Applications and the
            `openlustre` command in /usr/local/bin.

  .tar.gz   In Terminal:  tar -xzf openlustre-studio-*.tar.gz
                          cd openlustre-studio-*/ && ./install.sh
            Installs into ~/Applications/OpenLustre Studio and
            ~/.local/bin/openlustre (no administrator password needed).

Start it: open "OpenLustre Studio" or "PMS Sample" in Applications ›
OpenLustre Studio. A Terminal window shows the Studio's log (close it to
stop the Studio) and the Studio opens in your browser. From a terminal:
`openlustre studio launch`, or `openlustre studio launch --sample pms`.

Kind 2 and Z3 (the prover) are bundled. Generating and testing C needs a C
compiler: run `xcode-select --install` once if `cc` is missing.

Uninstall: sudo "/Applications/OpenLustre Studio/OpenLustre Studio.app/Contents/Resources/uninstall.sh"
(or ./install.sh --uninstall for the per-user install).

OpenLustre Studio is for demonstration and prototyping; it is not a
qualified tool.
