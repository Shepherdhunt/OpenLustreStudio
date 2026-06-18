# OpenLustre Studio — dependencies

**The core tool is self-contained.** The installed `openlustre.exe` embeds the
standard block library, so **authoring, type/clock/contract checking,
simulation, and C-Lite generation need no other software**. You can design a
model, check it, simulate it, and generate directional C-Lite out of the box.

The features below are **optional** — each is unlocked by a separate, freely
available tool. Run **`openlustre doctor`** at any time (Start Menu ▸ *OpenLustre
Studio ▸ Check Environment*, or the CLI) to see which are present on your machine
and what each enables.

| Dependency | Unlocks | Needed for | How to get it |
|---|---|---|---|
| **C compiler** — MSVC (`cl`), or `gcc`/`clang` on `PATH` | Local native build of the generated code | *Code ▸ Compile C-Lite*, the debug run, and the **dual-backend equivalence** tests (IR simulator vs compiled C) | Install **Visual Studio Build Tools** (MSVC) on Windows, or `gcc`/`clang` on Linux/macOS |
| **Kind 2** | Machine proof of contracts | The **Verify** tab and `openlustre prove` (BMC / k-induction over your assume/guarantee contracts). Without it, contracts are still checked statically and compiled into runtime monitors | Install from <https://kind2-mc.github.io/kind2/> and put `kind2` on `PATH` |
| **Docker** | Emulated-target runs *(roadmap)* | The planned `openlustre clite-emulate` backend — cross-compile the generated C-Lite and run it under **QEMU** (a third equivalence backend, beside the IR simulator and host-compiled C). The image installs the cross-toolchain + QEMU itself, so Docker will be the only host requirement | Install **Docker Desktop** from <https://www.docker.com/products/docker-desktop>. A fresh install may need its first-run setup (WSL2) or a reboot before the daemon starts |

## Notes

- **Why these aren't bundled.** Each has its own installer and license (Docker
  Desktop, the MSVC Build Tools, Kind 2), so the OpenLustre installer documents
  and *detects* them rather than redistributing them. `openlustre doctor` reports
  exactly what is present, what is missing, and — when a tool is installed but
  not ready (e.g. Docker installed but the daemon isn't running) — says so.
- **Build-time only** (not needed to *run* the Studio): the Rust toolchain and
  Inno Setup 6, used to build the app and its installer from source.

## Quick check

```
openlustre doctor
```

It prints, for the C compiler, Kind 2, and Docker: whether each is present and
the functionality it unlocks — and the core design→check→simulate→generate
workflow needs none of them.
