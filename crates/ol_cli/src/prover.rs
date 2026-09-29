//! Kind 2 provisioning: find the prover and an SMT solver, say what is
//! missing and how to get it, and install both on Linux / macOS.
//!
//! Kind 2 needs an SMT solver (Z3 by default) it can launch. Both are looked
//! for, in order:
//!
//! 1. an explicit path (`--kind2`),
//! 2. `OPENLUSTRE_KIND2` / `OPENLUSTRE_Z3`,
//! 3. the per-user tools directory (`openlustre kind2 install` puts them
//!    there; `OPENLUSTRE_TOOLS` overrides it),
//! 4. next to the `openlustre` executable (a release bundle's `tools/`),
//! 5. `PATH`.
//!
//! A solver found anywhere but `PATH` is passed to Kind 2 explicitly
//! (`--z3_bin …`), so a bundled or installed toolchain works without
//! touching the user's environment. Kind 2 has no native Windows build: the
//! guidance there is WSL or Docker, through the wrapper scripts in `tools/`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The Kind 2 release `install` fetches.
pub const KIND2_VERSION: &str = "v2.2.0";
/// The Z3 release `install` fetches.
pub const Z3_VERSION: &str = "4.13.4";

/// A tool and where it was found.
#[derive(Debug, Clone)]
pub struct Located {
    pub path: PathBuf,
    /// `--kind2`, `OPENLUSTRE_KIND2`, `tools directory`, `bundled`, `PATH`.
    pub via: String,
}

/// An SMT solver Kind 2 can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Solver {
    Z3,
    Cvc5,
    Yices2,
}

impl Solver {
    const ALL: [Solver; 3] = [Solver::Z3, Solver::Cvc5, Solver::Yices2];

    fn exe(self) -> &'static str {
        match self {
            Solver::Z3 => "z3",
            Solver::Cvc5 => "cvc5",
            Solver::Yices2 => "yices-smt2",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Solver::Z3 => "Z3",
            Solver::Cvc5 => "cvc5",
            Solver::Yices2 => "Yices 2",
        }
    }

    /// Kind 2's `--smt_solver` value and binary flag.
    fn flags(self) -> (&'static str, &'static str) {
        match self {
            Solver::Z3 => ("Z3", "--z3_bin"),
            Solver::Cvc5 => ("cvc5", "--cvc5_bin"),
            Solver::Yices2 => ("Yices2", "--yices2_bin"),
        }
    }

    fn env(self) -> &'static str {
        match self {
            Solver::Z3 => "OPENLUSTRE_Z3",
            Solver::Cvc5 => "OPENLUSTRE_CVC5",
            Solver::Yices2 => "OPENLUSTRE_YICES2",
        }
    }
}

/// What this machine has for proving.
#[derive(Debug, Clone)]
pub struct Toolchain {
    pub kind2: Option<Located>,
    /// `kind2 --version`, e.g. `kind2 v2.2.0`.
    pub kind2_version: Option<String>,
    pub solver: Option<(Solver, Located)>,
    pub solver_version: Option<String>,
}

impl Toolchain {
    /// Look for Kind 2 and a solver. `explicit` is a user-given Kind 2 path
    /// (the bare default `kind2` counts as none).
    pub fn detect(explicit: Option<&str>) -> Toolchain {
        let kind2 = explicit
            .filter(|p| !p.is_empty() && *p != "kind2")
            .map(|p| Located { path: PathBuf::from(p), via: "--kind2".into() })
            .or_else(|| find("kind2", "OPENLUSTRE_KIND2"));
        let kind2_version = kind2.as_ref().and_then(|k| version_of(&k.path, "--version"));
        let solver = Solver::ALL.iter().find_map(|s| find(s.exe(), s.env()).map(|l| (*s, l)));
        let solver_version = solver.as_ref().and_then(|(_, l)| version_of(&l.path, "--version"));
        Toolchain { kind2, kind2_version, solver, solver_version }
    }

    /// Kind 2 runs *and* has a solver.
    pub fn ready(&self) -> bool {
        self.kind2_version.is_some() && self.solver.is_some()
    }

    /// The binary to launch (`kind2` when none was found, so the launch
    /// failure is reported the usual way).
    pub fn binary(&self) -> String {
        self.kind2.as_ref().map(|k| k.path.display().to_string()).unwrap_or_else(|| "kind2".into())
    }

    /// Arguments that point Kind 2 at the solver found.
    pub fn solver_args(&self) -> Vec<String> {
        match &self.solver {
            Some((s, l)) => {
                let (name, flag) = s.flags();
                vec!["--smt_solver".into(), name.into(), flag.into(), l.path.display().to_string()]
            }
            None => vec![],
        }
    }

    /// `opts` set up to run this toolchain.
    pub fn apply(&self, mut opts: ol_kind2::Kind2Options) -> ol_kind2::Kind2Options {
        opts.kind2_binary = self.binary();
        let mut args = self.solver_args();
        args.extend(opts.extra_args);
        opts.extra_args = args;
        opts
    }

    /// One line: `Kind 2 v2.2.0 · Z3 4.13.4`, or what is missing.
    pub fn summary(&self) -> String {
        let k = match (&self.kind2, &self.kind2_version) {
            (_, Some(v)) => v.replacen("kind2", "Kind 2", 1),
            (Some(k), None) => format!("Kind 2 at {} does not run", k.path.display()),
            (None, None) => "Kind 2 not found".into(),
        };
        let s = match (&self.solver, &self.solver_version) {
            (Some((s, _)), Some(v)) => format!("{} {}", s.label(), short_version(v)),
            (Some((s, _)), None) => s.label().to_string(),
            (None, _) => "no SMT solver".into(),
        };
        format!("{k} · {s}")
    }

    pub fn to_json(&self) -> serde_json::Value {
        let loc = |l: &Option<Located>| {
            l.as_ref().map(|l| serde_json::json!({ "path": l.path.display().to_string(), "via": l.via }))
        };
        serde_json::json!({
            "ready": self.ready(),
            "summary": self.summary(),
            "kind2": loc(&self.kind2),
            "kind2_version": self.kind2_version,
            "solver": self.solver.as_ref().map(|(s, l)| serde_json::json!({
                "name": s.label(), "path": l.path.display().to_string(), "via": l.via,
            })),
            "solver_version": self.solver_version,
            "tools_dir": tools_dir().display().to_string(),
            "can_install": install_assets().is_ok(),
            "guidance": guidance(self),
        })
    }
}

/// Where `install` puts the tools: `OPENLUSTRE_TOOLS`, else
/// `~/.openlustre/tools` (`%LOCALAPPDATA%\OpenLustre\tools` on Windows).
pub fn tools_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("OPENLUSTRE_TOOLS") {
        return PathBuf::from(d);
    }
    if cfg!(windows) {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(d).join("OpenLustre").join("tools");
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default();
    PathBuf::from(home).join(".openlustre").join("tools")
}

fn exe_name(stem: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{stem}.exe"), format!("{stem}.cmd"), format!("{stem}.bat")]
    } else {
        vec![stem.to_string()]
    }
}

fn find(stem: &str, env: &str) -> Option<Located> {
    if let Some(p) = std::env::var_os(env).filter(|p| !p.is_empty()) {
        return Some(Located { path: PathBuf::from(p), via: env.into() });
    }
    let names = exe_name(stem);
    let in_dir = |dir: &Path, via: &str| {
        names.iter().map(|n| dir.join(n)).find(|p| p.is_file()).map(|path| Located { path, via: via.into() })
    };
    let tools = tools_dir();
    if let Some(l) = in_dir(&tools.join("bin"), "tools directory").or_else(|| in_dir(&tools, "tools directory")) {
        return Some(l);
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        if let Some(l) = in_dir(&dir.join("tools").join("bin"), "bundled")
            .or_else(|| in_dir(&dir.join("tools"), "bundled"))
            .or_else(|| in_dir(&dir, "bundled"))
        {
            return Some(l);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| in_dir(&d, "PATH"))
}

/// First line of `<exe> --version`, if it runs.
fn version_of(exe: &Path, flag: &str) -> Option<String> {
    let out = Command::new(exe).arg(flag).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    // Strip terminal colour codes.
    let mut clean = String::new();
    let mut esc = false;
    for c in line.chars() {
        match (esc, c) {
            (_, '\u{1b}') => esc = true,
            (true, 'm') => esc = false,
            (true, _) => {}
            (false, c) => clean.push(c),
        }
    }
    Some(clean)
}

/// `Z3 version 4.13.4 - 64 bit` → `4.13.4`.
fn short_version(v: &str) -> String {
    v.split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or(v)
        .to_string()
}

/// What to do next, for this platform and what is missing.
pub fn guidance(tc: &Toolchain) -> Vec<String> {
    if tc.ready() {
        return vec![];
    }
    let mut g = Vec::new();
    if let (Some(k), None) = (&tc.kind2, &tc.kind2_version) {
        g.push(format!(
            "{} (via {}) does not run — fix that path{}.",
            k.path.display(),
            k.via,
            if k.via.starts_with("OPENLUSTRE_") { format!(" or unset {}", k.via) } else { String::new() }
        ));
    }
    if install_assets().is_ok() {
        g.push(format!(
            "Run `openlustre kind2 install` (or Install in the Studio's Verify dock) to download Kind 2 {KIND2_VERSION} and Z3 {Z3_VERSION} into {}.",
            tools_dir().display()
        ));
        if tc.kind2_version.is_some() && tc.solver.is_none() {
            g.push("Or install an SMT solver yourself — Z3 (`apt install z3`, `brew install z3`) or cvc5 — and put it on PATH.".into());
        } else {
            g.push("Or install them yourself from https://github.com/kind2-mc/kind2/releases and put `kind2` and `z3` on PATH (or set OPENLUSTRE_KIND2 / OPENLUSTRE_Z3).".into());
        }
    } else if cfg!(windows) {
        g.push("Kind 2 has no native Windows build. Use WSL: `wsl --install -d Ubuntu`, then inside Ubuntu download kind2 from https://github.com/kind2-mc/kind2/releases and `sudo apt install z3`.".into());
        g.push("Then set OPENLUSTRE_KIND2 to the `tools\\kind2-wsl.cmd` wrapper shipped with OpenLustre Studio; it runs Kind 2 inside WSL with your file paths translated.".into());
        g.push("Or with Docker Desktop: `docker pull kind2/kind2:dev` and point OPENLUSTRE_KIND2 at `tools/kind2-docker.sh` (from Git Bash or WSL).".into());
    } else {
        g.push("No Kind 2 release for this platform: build it from source (https://github.com/kind2-mc/kind2) or use Docker — `docker pull kind2/kind2:dev` and set OPENLUSTRE_KIND2 to `tools/kind2-docker.sh`.".into());
    }
    g
}

/// The release assets for this platform: (Kind 2 tarball URL, Z3 zip URL).
pub fn install_assets() -> Result<(String, String), String> {
    let kind2_os = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "x86_64") => "macos-12-x86_64",
        ("macos", "aarch64") => "macos-12-arm64",
        (os, arch) => return Err(format!("no Kind 2 release for {os}/{arch}")),
    };
    let z3_os = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x64-glibc-2.35",
        ("linux", "aarch64") => "arm64-glibc-2.34",
        ("macos", "x86_64") => "x64-osx-13.7.1",
        _ => "arm64-osx-13.7.1",
    };
    Ok((
        format!("https://github.com/kind2-mc/kind2/releases/download/{KIND2_VERSION}/kind2-{KIND2_VERSION}-{kind2_os}.tar.gz"),
        format!("https://github.com/Z3Prover/z3/releases/download/z3-{Z3_VERSION}/z3-{Z3_VERSION}-{z3_os}.zip"),
    ))
}

/// Download Kind 2 and Z3 into `dir/bin`, then check they run. `log`
/// receives progress lines.
pub fn install(dir: &Path, log: &mut dyn FnMut(String)) -> Result<Toolchain, String> {
    let (kind2_url, z3_url) = install_assets()?;
    let bin = dir.join("bin");
    let work = dir.join("download");
    std::fs::create_dir_all(&bin).map_err(|e| format!("creating {}: {e}", bin.display()))?;
    std::fs::create_dir_all(&work).map_err(|e| format!("creating {}: {e}", work.display()))?;

    let tarball = work.join("kind2.tar.gz");
    log(format!("downloading {kind2_url}"));
    download(&kind2_url, &tarball)?;
    run("tar", &["-xzf".as_ref(), tarball.as_os_str(), "-C".as_ref(), work.as_os_str()])?;
    let kind2 = find_file(&work, "kind2").ok_or("the Kind 2 archive has no `kind2` binary")?;
    copy_exe(&kind2, &bin.join("kind2"))?;

    let zip = work.join("z3.zip");
    log(format!("downloading {z3_url}"));
    download(&z3_url, &zip)?;
    let unpacked = work.join("z3");
    unzip(&zip, &unpacked)?;
    let z3 = find_file(&unpacked, "z3").ok_or("the Z3 archive has no `z3` binary")?;
    copy_exe(&z3, &bin.join("z3"))?;
    let _ = std::fs::remove_dir_all(&work);

    let tc = Toolchain {
        kind2: Some(Located { path: bin.join("kind2"), via: "tools directory".into() }),
        kind2_version: version_of(&bin.join("kind2"), "--version"),
        solver: Some((Solver::Z3, Located { path: bin.join("z3"), via: "tools directory".into() })),
        solver_version: version_of(&bin.join("z3"), "--version"),
    };
    if !tc.ready() {
        return Err(format!("installed into {}, but the binaries do not run here ({})", bin.display(), tc.summary()));
    }
    log(format!("installed {} into {}", tc.summary(), bin.display()));
    Ok(tc)
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    run("curl", &["-fsSL".as_ref(), "--retry".as_ref(), "3".as_ref(), "-o".as_ref(), to.as_os_str(), url.as_ref()])
}

fn unzip(zip: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    // unzip where present; bsdtar (macOS, Windows) reads zip too; Python last.
    run("unzip", &["-q".as_ref(), "-o".as_ref(), zip.as_os_str(), "-d".as_ref(), to.as_os_str()])
        .or_else(|_| run("tar", &["-xf".as_ref(), zip.as_os_str(), "-C".as_ref(), to.as_os_str()]))
        .or_else(|_| run("python3", &["-m".as_ref(), "zipfile".as_ref(), "-e".as_ref(), zip.as_os_str(), to.as_os_str()]))
}

fn run(cmd: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("could not run `{cmd}`: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("`{cmd}` failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

fn copy_exe(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to).map_err(|e| format!("copying {} → {}: {e}", from.display(), to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Prove a one-property model end to end: the toolchain launches, finds its
/// solver, parses our Lustre, and answers. Returns Kind 2's verdict.
pub fn smoke_test(tc: &Toolchain) -> Result<String, String> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let work = std::env::temp_dir().join(format!("openlustre_kind2_smoke_{stamp}"));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let lus = work.join("smoke.lus");
    std::fs::write(
        &lus,
        "node Smoke(x: int) returns (y: int);\nlet\n  --%MAIN;\n  y = if x > 0 then x else 0;\n  --%PROPERTY \"nonneg\" y >= 0;\ntel\n",
    )
    .map_err(|e| e.to_string())?;
    let opts = tc.apply(ol_kind2::Kind2Options { timeout_seconds: Some(30), ..Default::default() });
    let result = ol_kind2::run_kind2(&lus, &opts).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&work);
    let result = result?;
    if let Some(e) = result.errors.first() {
        return Err(format!("Kind 2 reported: {e}"));
    }
    if result.exit_code == -1 {
        return Err(result.stderr);
    }
    match result.properties.first() {
        Some(p) if p.outcome() == ol_kind2::Outcome::Holds => Ok(p.status.clone()),
        Some(p) => Err(format!("the smoke property came back `{}`", p.status)),
        None => Err(format!("Kind 2 answered nothing (exit {})", result.exit_code)),
    }
}
