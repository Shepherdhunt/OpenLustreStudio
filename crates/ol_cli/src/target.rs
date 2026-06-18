//! Build target profiles — the OS / board a generated C-Lite build is aimed at.
//!
//! SCADE generates portable C that you integrate on the target; OpenLustre does
//! the same. A [`TargetProfile`] tailors the generated `Makefile` (toolchain and
//! flags) and an `INTEGRATION.md` note, so "Generate C-Lite for VxWorks" or
//! "…for embedded Linux (ARM)" produces build files ready for that toolchain.
//!
//! The `host` profile compiles locally (the existing behavior). Every other
//! profile is **directional**: the files are generated here and built with the
//! target's own cross-toolchain on the target / its SDK — exactly the
//! "generate, then build for the embedded OS and hardware" workflow. Cross
//! compilation is not attempted on this machine.

/// A named build target: which OS/arch the generated C is for, the toolchain
/// command its `Makefile` invokes, and integration guidance.
#[derive(Debug, Clone)]
pub(crate) struct TargetProfile {
    /// Stable id used on the wire (`/api/clite/compile` `target` field).
    pub id: &'static str,
    pub label: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    /// Toolchain compiler command the generated `Makefile` calls.
    pub cc: &'static str,
    pub cflags: &'static str,
    pub ldflags: &'static str,
    /// `true` for a non-host target: generate the build files but do not compile
    /// locally (build on the target toolchain).
    pub cross: bool,
    /// One-line integration hint, surfaced in the dialog.
    pub note: &'static str,
}

/// The built-in target profiles, host first.
pub(crate) fn target_profiles() -> Vec<TargetProfile> {
    vec![
        TargetProfile {
            id: "host",
            label: "Host — this machine",
            os: "(host OS)",
            arch: "(host)",
            cc: "cc",
            cflags: "-std=c11 -Wall -Wextra -O2",
            ldflags: "-lm",
            cross: false,
            note: "Builds and runs here with the auto-detected compiler — the CSV \
                   driver for desktop testing.",
        },
        TargetProfile {
            id: "linux-x86_64",
            label: "Linux (x86-64, GCC)",
            os: "Linux",
            arch: "x86-64",
            cc: "gcc",
            cflags: "-std=c11 -Wall -Wextra -O2",
            ldflags: "-lm",
            cross: true,
            note: "Standard glibc Linux. `make` on the Linux host or in your CI image.",
        },
        TargetProfile {
            id: "linux-arm",
            label: "Embedded Linux (ARM, cross GCC)",
            os: "Embedded Linux",
            arch: "ARM (armhf)",
            cc: "arm-linux-gnueabihf-gcc",
            cflags: "-std=c11 -Wall -Wextra -O2",
            ldflags: "-lm",
            cross: true,
            note: "Yocto / Buildroot style. Install the ARM cross-toolchain (or \
                   source the SDK environment), then `make`.",
        },
        TargetProfile {
            id: "vxworks",
            label: "VxWorks (Wind River RTOS)",
            os: "VxWorks",
            arch: "(target CPU)",
            cc: "wr-cc",
            cflags: "-std=c11 -O2",
            ldflags: "",
            cross: true,
            note: "Build as a DKM/RTP in Wind River Workbench; spawn <entry>_step \
                   on a periodic task (see INTEGRATION.md).",
        },
        TargetProfile {
            id: "baremetal-arm",
            label: "Bare-metal (ARM Cortex-M)",
            os: "Bare-metal (no OS)",
            arch: "ARM Cortex-M",
            cc: "arm-none-eabi-gcc",
            cflags: "-std=c11 -O2 -ffreestanding -mcpu=cortex-m4 -mthumb",
            ldflags: "-nostdlib",
            cross: true,
            note: "No OS / no heap. Call <entry>_step from a timer ISR or the \
                   super-loop (see INTEGRATION.md).",
        },
    ]
}

/// Look up a profile by id; defaults to `host` for `None`/unknown so callers
/// never have to special-case the common path.
pub(crate) fn find_target(id: Option<&str>) -> TargetProfile {
    let id = id.unwrap_or("host");
    target_profiles()
        .into_iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| target_profiles().into_iter().next().expect("host profile exists"))
}

impl TargetProfile {
    /// The integration-scaffold idiom `emit_integration_main` should produce for
    /// this target: a portable super-loop, a VxWorks task, or a bare-metal tick.
    pub(crate) fn integration_style(&self) -> ol_clite_emit::harness::IntegrationStyle {
        use ol_clite_emit::harness::IntegrationStyle as S;
        match self.id {
            "vxworks" => S::VxWorksTask,
            "baremetal-arm" => S::BareMetalIsr,
            _ => S::Loop,
        }
    }
}

/// The generated `Makefile` for an entry operator built for `t`.
pub(crate) fn makefile_for_target(entry: &str, t: &TargetProfile) -> String {
    let build_hint = if t.cross {
        "Build with this toolchain on the target / its SDK (not on this machine)."
    } else {
        "Run `make` to build and `./<target>` to run (reads CSV on stdin)."
    };
    format!(
        "# Generated by OpenLustre Studio.
# Target: {label}  —  OS: {os}  ·  arch: {arch}
# Toolchain: {cc}.  {build_hint}
# Integrate `{entry}_step` into your application — see INTEGRATION.md and the
# calls driver.c makes for the exact signatures.

TARGET ?= {entry}
CC ?= {cc}
CFLAGS ?= {cflags}
LDLIBS ?= {ldflags}

SOURCES = openlustre_generated.c driver.c

$(TARGET): $(SOURCES)
\t$(CC) $(CFLAGS) -o $@ $(SOURCES) $(LDLIBS)

run: $(TARGET)
\t./$(TARGET)

clean:
\trm -f $(TARGET) *.o

.PHONY: run clean
",
        label = t.label,
        os = t.os,
        arch = t.arch,
        cc = t.cc,
        cflags = t.cflags,
        ldflags = t.ldflags,
        entry = entry,
        build_hint = build_hint,
    )
}

/// A target-specific integration note written next to the generated sources.
pub(crate) fn integration_readme(entry: &str, t: &TargetProfile) -> String {
    // How the generated integration.c is entered, per target idiom.
    let entry_point = match t.id {
        "vxworks" => format!("`taskSpawn` the `{entry}_task` function (it runs the periodic loop)"),
        "baremetal-arm" => format!(
            "call `{entry}_app_init()` once at boot, then `{entry}_tick()` from your periodic timer ISR"
        ),
        _ => "its `main()` runs the periodic super-loop".to_string(),
    };
    format!(
        "# Integrating `{entry}` on {label}

**Target:** {os} · {arch}
**Toolchain:** `{cc}`

## Build

```sh
make            # uses CC={cc}, CFLAGS={cflags}
```

{build_line}

## Files

- `openlustre_generated.h` / `.c` — the model as portable C-Lite. API:
  `{entry}_init`, `{entry}_step`, and the `{entry}_Input` / `{entry}_Output` /
  `{entry}_State` structs.
- `integration.c` — **your embedded entry point**, generated for this target:
  {entry_point}. Fill in the marked input/output stubs; it `memset`s the inputs
  to zero so it builds as-is.
- `driver.c` — the CSV test harness (reads inputs on stdin, prints the trace),
  for host verification.
- `Makefile` — toolchain + flags (builds `driver.c` by default; compile
  `integration.c` into your application).

## Verifying on the host first

Generate for **Host — this machine** to compile the same model locally and run
the dual-backend / CSV checks before cross-building. (Roadmap: a one-click QEMU
emulation + Docker test harness that runs this build on an emulated board.)
",
        entry = entry,
        label = t.label,
        os = t.os,
        arch = t.arch,
        cc = t.cc,
        cflags = t.cflags,
        entry_point = entry_point,
        build_line = if t.cross {
            "This is a **cross target** — OpenLustre generates the files; build them with the \
             toolchain above on the target or its SDK. Install/source that toolchain first."
        } else {
            "Builds and runs on this machine."
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_host_first_and_complete() {
        let ps = target_profiles();
        assert!(ps.len() >= 5, "expected the built-in target set");
        assert_eq!(ps[0].id, "host", "host must be first (the default)");
        assert!(!ps[0].cross, "host is native");
        // Every named OS the user asked about is represented and is a cross target.
        for id in ["linux-arm", "vxworks", "baremetal-arm"] {
            let t = ps.iter().find(|t| t.id == id).expect("profile present");
            assert!(t.cross, "{id} is a cross target");
        }
    }

    #[test]
    fn find_target_defaults_to_host() {
        assert_eq!(find_target(None).id, "host");
        assert_eq!(find_target(Some("nonexistent")).id, "host");
        assert_eq!(find_target(Some("vxworks")).id, "vxworks");
    }

    #[test]
    fn makefile_uses_the_target_toolchain() {
        let arm = find_target(Some("linux-arm"));
        let mk = makefile_for_target("Doubler", &arm);
        assert!(mk.contains("CC ?= arm-linux-gnueabihf-gcc"), "cross CC in Makefile:\n{mk}");
        assert!(mk.contains("TARGET ?= Doubler"));
        assert!(mk.contains("Embedded Linux"), "target named in the header");

        let host = find_target(Some("host"));
        let hmk = makefile_for_target("Doubler", &host);
        assert!(hmk.contains("CC ?= cc"), "host CC in Makefile:\n{hmk}");
        assert!(!hmk.contains("arm-linux-gnueabihf-gcc"));
    }

    #[test]
    fn integration_readme_has_balanced_braces_and_names_the_entry() {
        // Guard against stray `{{`/`}}` leaking into the doc, and confirm it
        // names the API and the per-target integration entry point.
        for id in ["host", "vxworks", "baremetal-arm", "linux-arm"] {
            let t = find_target(Some(id));
            let doc = integration_readme("Doubler", &t);
            assert!(!doc.contains("{{") && !doc.contains("}}"), "{id}: doubled braces in:\n{doc}");
            assert!(doc.contains("Doubler_step"), "{id}: names the step API");
            assert!(doc.contains("integration.c"), "{id}: points at the integration skeleton");
            assert!(doc.contains(t.cc), "{id}: names the toolchain");
        }
        // The per-target entry point is described (the loop body itself now lives
        // in integration.c, exercised by the harness tests).
        assert!(integration_readme("Doubler", &find_target(Some("vxworks"))).contains("Doubler_task"));
        assert!(integration_readme("Doubler", &find_target(Some("baremetal-arm"))).contains("Doubler_tick"));
    }
}
