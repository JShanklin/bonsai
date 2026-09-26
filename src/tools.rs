//! Build tools a tree can use: `bonsai tools`, or the wizard's Build tools step.
//! Installing one is machine-wide and happens once; the settings that use it go
//! in the tree's own `.cargo/config.toml`, so each tree records its own.

use std::io;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use toml_edit::{Array, DocumentMut, Item, Table, value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Sccache,
    Mold,
    Zigbuild,
    Bacon,
}

/// Where a missing program comes from.
#[derive(Clone, Copy)]
enum Source {
    /// `cargo install <crate>`.
    Cargo(&'static str),
    /// The system package manager, with sudo.
    System(&'static str),
}

/// The linker wrapper zigbuild trees link with, relative to the tree.
pub const ZIG_CC: &str = ".cargo/zig-cc";
const MOLD_FLAGS: [&str; 2] = ["-C", "link-arg=-fuse-ld=mold"];
/// The comment the Pi 5 / Zero 2 W templates put above their gcc linker, put
/// back with it when zigbuild is switched off.
const GCC_COMMENT: &str = "# The linker for the Pi's CPU: `sudo apt install gcc-aarch64-linux-gnu`, or zig\n\
                           # (`bonsai tools`, pick zigbuild), which needs no cross toolchain.";

impl Tool {
    pub const ALL: [Tool; 4] = [Tool::Sccache, Tool::Mold, Tool::Zigbuild, Tool::Bacon];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Sccache => "sccache",
            Tool::Mold => "mold",
            Tool::Zigbuild => "zigbuild",
            Tool::Bacon => "bacon",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Tool::Sccache => "reuses compiled crates across builds and trees",
            Tool::Mold => "a fast linker (with clang) for builds that run here",
            Tool::Zigbuild => {
                "links Pi builds with zig: no cross toolchain, right code for every Pi"
            }
            Tool::Bacon => "rebuilds and shows errors each time you save",
        }
    }

    pub fn parse(name: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.name() == name)
    }

    /// Whether it helps a tree on `mcu`. mold and zigbuild are for Linux builds.
    pub fn fits(self, mcu: &str) -> bool {
        match self {
            Tool::Mold | Tool::Zigbuild => mcu == "rpi",
            Tool::Sccache | Tool::Bacon => true,
        }
    }

    /// The programs it needs on `PATH`, and where each comes from.
    fn needs(self) -> &'static [(&'static str, Source)] {
        match self {
            Tool::Sccache => &[("sccache", Source::Cargo("sccache"))],
            Tool::Mold => &[
                ("mold", Source::System("mold")),
                ("clang", Source::System("clang")),
            ],
            Tool::Zigbuild => &[
                ("cargo-zigbuild", Source::Cargo("cargo-zigbuild")),
                ("zig", Source::System("zig")),
            ],
            Tool::Bacon => &[("bacon", Source::Cargo("bacon"))],
        }
    }

    pub fn installed(self) -> bool {
        self.needs().iter().all(|(program, _)| on_path(program))
    }
}

pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// Install whatever `tools` still lack: system packages with sudo, crates with
/// cargo. Returns the tools that are usable afterwards.
pub fn install(tools: &[Tool]) -> Vec<Tool> {
    let mut packages: Vec<&str> = Vec::new();
    let mut crates: Vec<&str> = Vec::new();
    for (program, source) in tools.iter().flat_map(|t| t.needs()) {
        if on_path(program) {
            continue;
        }
        match *source {
            Source::System(p) if !packages.contains(&p) => packages.push(p),
            Source::Cargo(c) if !crates.contains(&c) => crates.push(c),
            _ => {}
        }
    }
    if !packages.is_empty() {
        install_packages(&packages);
    }
    if !crates.is_empty() {
        install_crates(&crates);
    }
    tools
        .iter()
        .copied()
        .filter(|t| {
            let ok = t.installed();
            if !ok {
                eprintln!(
                    "{}: still not installed, so the tree won't use it",
                    t.name()
                );
            }
            ok
        })
        .collect()
}

fn install_packages(packages: &[&str]) {
    let (manager, verb): (&str, &[&str]) = if on_path("pacman") {
        ("pacman", &["-S", "--needed"])
    } else if on_path("dnf") {
        ("dnf", &["install"])
    } else if on_path("apt-get") {
        ("apt-get", &["install"])
    } else {
        eprintln!(
            "no pacman, dnf or apt-get here; install these yourself: {}",
            packages.join(" ")
        );
        return;
    };
    let mut packages = packages.to_vec();
    if manager == "apt-get" && packages.contains(&"zig") {
        eprintln!(
            "zig isn't packaged for Debian or Ubuntu: get it from https://ziglang.org/download/ and put it on your PATH"
        );
        packages.retain(|&p| p != "zig");
    }
    if packages.is_empty() {
        return;
    }
    println!("sudo {manager} {} {}", verb.join(" "), packages.join(" "));
    let ok = Command::new("sudo")
        .arg(manager)
        .args(verb)
        .args(&packages)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("{manager} didn't finish");
    }
}

fn install_crates(crates: &[&str]) {
    // cargo-binstall downloads prebuilt binaries; otherwise cargo compiles them.
    let args: Vec<&str> = if on_path("cargo-binstall") {
        [&["binstall", "-y"][..], crates].concat()
    } else {
        [&["install", "--locked"][..], crates].concat()
    };
    println!("cargo {}", args.join(" "));
    let ok = Command::new("cargo")
        .args(&args)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("cargo {} didn't finish", args[0]);
    }
}

/// The zig flags for a Pi target, or None when zigbuild doesn't cover it.
fn zig_target(target: &str) -> Option<&'static str> {
    match target {
        "aarch64-unknown-linux-gnu" => Some("-target aarch64-linux-gnu"),
        // The Zero W's ARMv6 with VFPv2, as cargo-zigbuild picks for this target.
        "arm-unknown-linux-gnueabihf" => {
            Some("-mcpu=generic+v6+strict_align+vfp2-d32 -target arm-linux-gnueabihf")
        }
        _ => None,
    }
}

/// The linker a Pi template sets when zig isn't used.
fn default_linker(target: &str) -> Option<&'static str> {
    (target == "aarch64-unknown-linux-gnu").then_some("aarch64-linux-gnu-gcc")
}

/// `.cargo/zig-cc` for a zigbuild tree.
pub fn zig_cc_script(target: &str) -> Option<String> {
    let flags = zig_target(target)?;
    Some(format!(
        "#!/bin/sh\n\
         # Links with zig through cargo-zigbuild (`bonsai tools`), so no cross\n\
         # toolchain is needed. Remove zigbuild with `bonsai tools` to undo.\n\
         exec cargo-zigbuild zig cc -- -g -fno-sanitize=all {flags} \"$@\"\n"
    ))
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// The tree's `.cargo/config.toml` with `tools` on and bonsai's other tools
/// off. `target` is the tree's build target; `host` is this computer's, for
/// mold, which speeds up builds that run here.
pub fn configure(config: &str, tools: &[Tool], target: &str, host: &str) -> io::Result<String> {
    let mut doc = DocumentMut::from_str(config).map_err(invalid)?;
    let on = |t: Tool| tools.contains(&t);

    // sccache: wraps rustc for every build of the tree.
    let build = table(&mut doc, &["build"]);
    if on(Tool::Sccache) {
        set(
            build,
            "rustc-wrapper",
            value("sccache"),
            "# sccache reuses compiled crates (`bonsai tools`).",
        );
    } else if build.get("rustc-wrapper").and_then(Item::as_str) == Some("sccache") {
        build.remove("rustc-wrapper");
    }

    // mold: links builds for this computer (`cargo local`). Not when this
    // computer is the tree's target: that table holds the Pi's own linker.
    if host != target {
        let t = table(&mut doc, &["target", host]);
        if on(Tool::Mold) {
            set(
                t,
                "linker",
                value("clang"),
                "# mold links builds for this computer (`bonsai tools`).",
            );
            set(
                t,
                "rustflags",
                Item::Value(Array::from_iter(MOLD_FLAGS).into()),
                "",
            );
        } else if t.get("linker").and_then(Item::as_str) == Some("clang") {
            t.remove("linker");
            t.remove("rustflags");
        }
        if t.is_empty() {
            doc["target"].as_table_mut().map(|all| all.remove(host));
        }
    }

    // zigbuild: links builds for the Pi with zig.
    if zig_target(target).is_some() {
        let t = table(&mut doc, &["target", target]);
        let zig = t.get("linker").and_then(Item::as_str) == Some(ZIG_CC);
        if on(Tool::Zigbuild) {
            set(
                t,
                "linker",
                value(ZIG_CC),
                "# zig links the Pi's build (zigbuild, `bonsai tools`).",
            );
        } else if zig {
            match default_linker(target) {
                Some(gcc) => set(t, "linker", value(gcc), GCC_COMMENT),
                None => {
                    t.remove("linker");
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// The table at `path`, created implicitly (so `[target.x]` stays one header).
fn table<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> &'a mut Table {
    let mut t = doc.as_table_mut();
    for (i, key) in path.iter().enumerate() {
        let last = i + 1 == path.len();
        let item = t.entry(key).or_insert_with(|| {
            let mut new = Table::new();
            new.set_implicit(!last);
            Item::Table(new)
        });
        t = item.as_table_mut().expect("config tables are tables");
    }
    t
}

/// Set `key`, with `comment` above it when it's new or its comment is bonsai's.
fn set(t: &mut Table, key: &str, item: Item, comment: &str) {
    t.insert(key, item);
    if !comment.is_empty()
        && let Some(mut k) = t.key_mut(key)
    {
        k.leaf_decor_mut().set_prefix(format!("{comment}\n"));
    }
}

/// The tools a tree's config switches on (bacon has no settings, so never).
pub fn configured(config: &str) -> Vec<Tool> {
    let Ok(doc) = DocumentMut::from_str(config) else {
        return Vec::new();
    };
    let targets = doc.get("target").and_then(Item::as_table);
    let any_target = |f: &dyn Fn(&Table) -> bool| {
        targets.is_some_and(|all| all.iter().any(|(_, t)| t.as_table().is_some_and(f)))
    };
    let mut tools = Vec::new();
    if doc
        .get("build")
        .and_then(|b| b.get("rustc-wrapper"))
        .and_then(Item::as_str)
        == Some("sccache")
    {
        tools.push(Tool::Sccache);
    }
    if any_target(&|t| {
        t.get("rustflags")
            .is_some_and(|f| f.to_string().contains("fuse-ld=mold"))
    }) {
        tools.push(Tool::Mold);
    }
    if any_target(&|t| t.get("linker").and_then(Item::as_str) == Some(ZIG_CC)) {
        tools.push(Tool::Zigbuild);
    }
    tools
}

/// This computer's target triple, from rustc.
pub fn host_triple() -> Option<String> {
    let out = Command::new("rustc").arg("-vV").output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
}

/// Switch the tree in `dir` to `tools`: edit its config, and write or delete
/// its zig linker wrapper.
pub fn apply(dir: &Path, tools: &[Tool], target: &str) -> io::Result<()> {
    let path = dir.join(".cargo/config.toml");
    let config = std::fs::read_to_string(&path)?;
    let host = host_triple().unwrap_or_default();
    std::fs::write(&path, configure(&config, tools, target, &host)?)?;
    let zig_cc = dir.join(ZIG_CC);
    match zig_cc_script(target).filter(|_| tools.contains(&Tool::Zigbuild)) {
        Some(script) => {
            std::fs::write(&zig_cc, script)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&zig_cc, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        None if zig_cc.exists() => std::fs::remove_file(&zig_cc)?,
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PI5: &str = include_str!("../templates/rpi/pi5/.cargo/config.toml");
    const ZERO_W: &str = include_str!("../templates/rpi/zero-w/.cargo/config.toml");
    const AARCH64: &str = "aarch64-unknown-linux-gnu";
    const ARMV6: &str = "arm-unknown-linux-gnueabihf";
    const HOST: &str = "x86_64-unknown-linux-gnu";

    #[test]
    fn all_on_then_all_off_round_trips() {
        let all = [Tool::Sccache, Tool::Mold, Tool::Zigbuild];
        let on = configure(PI5, &all, AARCH64, HOST).unwrap();
        assert!(on.contains("rustc-wrapper = \"sccache\""), "{on}");
        assert!(on.contains("[target.x86_64-unknown-linux-gnu]"), "{on}");
        assert!(on.contains("\"link-arg=-fuse-ld=mold\""), "{on}");
        assert!(on.contains("linker = \".cargo/zig-cc\""), "{on}");
        assert!(!on.contains("aarch64-linux-gnu-gcc\"\n"), "{on}");
        assert_eq!(configured(&on), all);
        // Idempotent, and the template's other settings stay.
        assert_eq!(configure(&on, &all, AARCH64, HOST).unwrap(), on);
        assert!(on.contains("run-on-pi") && on.contains("BONSAI_PI"), "{on}");

        // Off again is exactly the template.
        assert_eq!(configure(&on, &[], AARCH64, HOST).unwrap(), PI5);
    }

    #[test]
    fn zero_w_gets_a_linker_and_loses_it_again() {
        let on = configure(ZERO_W, &[Tool::Zigbuild], ARMV6, HOST).unwrap();
        assert!(on.contains("linker = \".cargo/zig-cc\""), "{on}");
        assert_eq!(configure(&on, &[], ARMV6, HOST).unwrap(), ZERO_W);
        assert!(zig_cc_script(ARMV6).unwrap().contains("-mcpu=generic+v6"));
    }

    #[test]
    fn mold_skips_the_host_when_it_is_the_target() {
        // Building on a Pi 5 itself: the target table holds the Pi's linker.
        let on = configure(PI5, &[Tool::Mold], AARCH64, AARCH64).unwrap();
        assert!(!on.contains("fuse-ld=mold"), "{on}");
    }

    #[test]
    fn mcu_trees_get_sccache_only() {
        let config = "[build]\ntarget = \"thumbv6m-none-eabi\"\n";
        let fitting: Vec<Tool> = Tool::ALL.into_iter().filter(|t| t.fits("pico")).collect();
        assert_eq!(fitting, [Tool::Sccache, Tool::Bacon]);
        let on = configure(config, &fitting, "thumbv6m-none-eabi", HOST).unwrap();
        assert_eq!(
            on,
            "[build]\ntarget = \"thumbv6m-none-eabi\"\n# sccache reuses compiled crates (`bonsai tools`).\nrustc-wrapper = \"sccache\"\n"
        );
    }

    #[test]
    fn user_linkers_are_left_alone() {
        let config = "[target.aarch64-unknown-linux-gnu]\nlinker = \"my-gcc\"\n";
        let off = configure(config, &[], AARCH64, HOST).unwrap();
        assert!(off.contains("linker = \"my-gcc\""), "{off}");
    }
}
