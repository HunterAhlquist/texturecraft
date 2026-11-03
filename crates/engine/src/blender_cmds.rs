//! `blender.installAddon`: TextureCraft's Blender add-on (a sidebar panel and an Object-menu
//! entry that open the selected objects in TextureCraft, `assets/blender-addon`) written into
//! Blender's user add-ons folder for the installed Blender's version and switched on by a
//! Blender started in the background. The command is enabled only where a Blender was found
//! (`$BLENDER`, `blender` on `PATH`, or the usual install locations), so the menu offers it
//! only then. Native only: the browser has no Blender to talk to.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, boolp, strp};
use crate::{EngineError, Result, Session, cmd};

/// The add-on as shipped in this binary.
pub const ADDON_INIT: &str = include_str!("../../../assets/blender-addon/texturecraft/__init__.py");
pub const ADDON_MANIFEST: &str = include_str!("../../../assets/blender-addon/texturecraft/blender_manifest.toml");
/// The add-on's module (folder) name in Blender.
pub const ADDON_MODULE: &str = "texturecraft";
/// How long the background Blender that enables the add-on may take.
#[cfg(not(target_arch = "wasm32"))]
const ENABLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "blender.installAddon",
        "Install Blender add-on...",
        ["Edit"],
        None,
        "{blender?: path to the Blender executable (default: the Blender found on this computer), enable?: bool (default true: a Blender started in the background switches the add-on on and saves the preferences)} → {addon: folder written, blender, version, enabled, message}: installs TextureCraft's Blender add-on (a TextureCraft tab in the 3D viewport's sidebar and an Object-menu entry that open the selected objects in TextureCraft) into Blender's user add-ons folder; enabled only when a Blender is installed",
        blender_installed,
        install
    )]
}

/// Ok when a Blender is installed (the menu shows the command only then).
pub fn blender_installed(_: &Session) -> std::result::Result<(), String> {
    match find_blender() {
        Some(_) => Ok(()),
        None => Err("Blender was not found on this computer (install it, or set BLENDER to its executable)".into()),
    }
}

/// Blender's executable: `$BLENDER`, `blender` on `PATH`, or the usual install locations
/// (newest version first); looked up once per run.
#[cfg(not(target_arch = "wasm32"))]
pub fn find_blender() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| detect(std::env::var_os("BLENDER"), std::env::var_os("PATH"), &install_candidates())).clone()
}

#[cfg(target_arch = "wasm32")]
pub fn find_blender() -> Option<PathBuf> {
    None
}

/// The first existing executable: `env` (`$BLENDER`), `blender` in a `path` folder, else the
/// first of `candidates`.
fn detect(env: Option<std::ffi::OsString>, path: Option<std::ffi::OsString>, candidates: &[PathBuf]) -> Option<PathBuf> {
    if let Some(p) = env.filter(|p| !p.is_empty()).map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let exe = format!("blender{}", std::env::consts::EXE_SUFFIX);
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(&exe);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    candidates.iter().find(|p| p.is_file()).cloned()
}

/// Where Blender installs by default (a plain install first, then versioned ones, newest
/// first, then Steam).
#[cfg(not(target_arch = "wasm32"))]
fn install_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let mut v = Vec::new();
    #[cfg(target_os = "macos")]
    {
        for base in [Some(PathBuf::from("/Applications")), home.as_ref().map(|h| h.join("Applications"))].into_iter().flatten() {
            v.push(base.join("Blender.app/Contents/MacOS/Blender"));
            v.extend(versioned(&base, "Blender", ".app").into_iter().map(|d| d.join("Contents/MacOS/Blender")));
        }
        if let Some(h) = &home {
            v.push(h.join("Library/Application Support/Steam/steamapps/common/Blender/Blender.app/Contents/MacOS/Blender"));
        }
    }
    #[cfg(windows)]
    {
        for base in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(|k| std::env::var_os(k)).map(PathBuf::from) {
            let foundation = base.join("Blender Foundation");
            v.push(foundation.join("Blender").join("blender.exe"));
            v.extend(versioned(&foundation, "Blender", "").into_iter().map(|d| d.join("blender.exe")));
            v.push(base.join("Steam").join("steamapps").join("common").join("Blender").join("blender.exe"));
        }
        let _ = &home;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        v.extend(["/usr/bin/blender", "/usr/local/bin/blender", "/snap/bin/blender"].map(PathBuf::from));
        if let Some(h) = &home {
            v.push(h.join(".local/bin/blender"));
        }
        v.extend(versioned(Path::new("/opt"), "blender", "").into_iter().map(|d| d.join("blender")));
        if let Some(h) = &home {
            v.push(h.join(".steam/steam/steamapps/common/Blender/blender"));
            v.push(h.join(".local/share/Steam/steamapps/common/Blender/blender"));
        }
    }
    v
}

/// Entries of `dir` named `<prefix>…<suffix>` other than `<prefix><suffix>` itself
/// (`Blender 4.2.app` beside `Blender.app`), newest name first.
fn versioned(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let plain = format!("{prefix}{suffix}");
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix) && n != plain))
        .collect();
    v.sort();
    v.reverse();
    v
}

/// `X.Y` from the first `Blender X.Y.Z…` line of `blender --version`.
fn parse_version(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| l.starts_with("Blender "))?;
    let number = line.strip_prefix("Blender ")?.trim().split(|c: char| !c.is_ascii_digit() && c != '.').next()?;
    let mut parts = number.split('.');
    let (major, minor) = (parts.next()?, parts.next()?);
    (major.parse::<u32>().is_ok() && minor.parse::<u32>().is_ok()).then(|| format!("{major}.{minor}"))
}

/// Blender's user scripts folder (where `addons/` lives) for Blender `version`:
/// `$BLENDER_USER_SCRIPTS`, else `scripts` under `$BLENDER_USER_RESOURCES`, else the platform's
/// per-version user folder.
fn scripts_dir(version: &str, env: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    if let Some(s) = env("BLENDER_USER_SCRIPTS") {
        return Some(s);
    }
    if let Some(r) = env("BLENDER_USER_RESOURCES") {
        return Some(r.join("scripts"));
    }
    Some(config_base(env)?.join(version).join("scripts"))
}

/// Blender's user configuration root (`Blender/<version>/…` lives below it).
#[cfg(target_os = "macos")]
fn config_base(env: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    Some(env("HOME")?.join("Library/Application Support/Blender"))
}

#[cfg(windows)]
fn config_base(env: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    Some(env("APPDATA")?.join("Blender Foundation").join("Blender"))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn config_base(env: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    Some(env("XDG_CONFIG_HOME").or_else(|| Some(env("HOME")?.join(".config")))?.join("blender"))
}

/// Write the add-on into `dir` (`…/addons/texturecraft`), recording `executable` as the
/// TextureCraft its button starts.
pub fn write_addon(dir: &Path, executable: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("__init__.py"), ADDON_INIT)?;
    std::fs::write(dir.join("blender_manifest.toml"), ADDON_MANIFEST)?;
    let record = json!({"executable": executable.to_string_lossy(), "texturecraft": env!("CARGO_PKG_VERSION")});
    std::fs::write(dir.join("texturecraft_path.json"), serde_json::to_string_pretty(&record).unwrap_or_default())
}

#[cfg(not(target_arch = "wasm32"))]
fn other(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

/// `X.Y` of the Blender at `exe`.
#[cfg(not(target_arch = "wasm32"))]
fn blender_version(exe: &Path) -> Result<String> {
    let out = crate::link_cmds::quiet(exe).arg("--version").output().map_err(|e| other(format!("Blender ({}) did not start: {e}", exe.display())))?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_version(&text).ok_or_else(|| other(format!("could not read Blender's version from \"{}\"", text.lines().next().unwrap_or_default().trim())))
}

/// Switch the add-on on and save Blender's preferences, through a Blender in the background.
#[cfg(not(target_arch = "wasm32"))]
fn enable_addon(exe: &Path) -> std::result::Result<(), String> {
    use std::io::Read as _;
    let expr = format!("import bpy; bpy.ops.preferences.addon_enable(module='{ADDON_MODULE}'); bpy.ops.wm.save_userpref()");
    let mut child = crate::link_cmds::quiet(exe)
        .args(["-b", "--python-exit-code", "1", "--python-expr", &expr])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Blender did not start: {e}"))?;
    // Drain stderr on a thread so a chatty Blender never blocks on a full pipe.
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_string(&mut text);
        }
        text
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < ENABLE_TIMEOUT => std::thread::sleep(std::time::Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Blender took longer than {} seconds to enable the add-on", ENABLE_TIMEOUT.as_secs()));
            }
            Err(e) => return Err(format!("waiting for Blender: {e}")),
        }
    };
    let text = reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let detail = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string();
        Err(format!("Blender could not enable the add-on ({status}): {detail}"))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn install(_: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "blender.installAddon";
    let exe = match strp(p, "blender") {
        Some(b) => PathBuf::from(b),
        None => find_blender().ok_or_else(|| bad(CMD, "Blender was not found; pass `blender`"))?,
    };
    if !exe.is_file() {
        return Err(bad(CMD, format!("{} is not a file", exe.display())));
    }
    let version = blender_version(&exe)?;
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = scripts_dir(&version, &env).ok_or_else(|| other("no home folder to install the add-on into"))?.join("addons").join(ADDON_MODULE);
    let texturecraft = std::env::current_exe().map_err(|e| other(format!("this program's path: {e}")))?;
    write_addon(&dir, &texturecraft).map_err(|e| other(format!("{}: {e}", dir.display())))?;
    let (enabled, problem) = if boolp(p, "enable").unwrap_or(true) {
        match enable_addon(&exe) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        }
    } else {
        (false, None)
    };
    let message = if enabled {
        format!(
            "TextureCraft's add-on is installed and enabled in Blender {version}: open models from the TextureCraft tab of the 3D viewport's sidebar (restart Blender if it is open)"
        )
    } else {
        format!(
            "TextureCraft's add-on was written to {}; enable it in Blender {version} under Edit > Preferences > Add-ons{}",
            dir.display(),
            problem.as_deref().map(|p| format!(" ({p})")).unwrap_or_default()
        )
    };
    Ok(json!({"addon": dir.display().to_string(), "blender": exe.display().to_string(), "version": version, "enabled": enabled, "message": message}))
}

#[cfg(target_arch = "wasm32")]
fn install(_: &mut Session, _: &Value) -> Result<Value> {
    Err(EngineError::Other("installing the Blender add-on needs the desktop app".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_from_blenders_banner() {
        assert_eq!(parse_version("Blender 5.1.1\n\tbuild date: 2026-04-14\n").as_deref(), Some("5.1"));
        assert_eq!(parse_version("Blender 2.79b").as_deref(), Some("2.79"));
        assert_eq!(parse_version("Blender 4.2.0 LTS (hash abc)").as_deref(), Some("4.2"));
        assert_eq!(parse_version("  Blender 3.6.14"), Some("3.6".into()));
        assert_eq!(parse_version("blender: command not found"), None);
        assert_eq!(parse_version("Blender x.y"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn detection_prefers_the_environment_then_path_then_known_folders() {
        let dir = std::env::temp_dir().join(format!("texturecraft-blender-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let exe = format!("blender{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(a.join(&exe), "").unwrap();
        std::fs::write(b.join("Blender-custom"), "").unwrap();
        let candidates = [dir.join("missing"), b.join("Blender-custom")];
        assert_eq!(detect(Some(b.join("Blender-custom").into()), None, &[]), Some(b.join("Blender-custom")));
        assert_eq!(detect(Some("".into()), Some(std::env::join_paths([&dir, &a]).unwrap()), &candidates), Some(a.join(&exe)));
        assert_eq!(detect(None, Some(dir.clone().into()), &candidates), Some(b.join("Blender-custom")));
        assert_eq!(detect(None, None, &[dir.join("missing")]), None);
        // Versioned installs, newest first, without the plain one.
        for n in ["Blender.app", "Blender 4.2.app", "Blender 5.0.app", "Other.app"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
        }
        assert_eq!(versioned(&dir, "Blender", ".app"), [dir.join("Blender 5.0.app"), dir.join("Blender 4.2.app")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_scripts_folder_follows_blenders_rules() {
        let env = |vars: &'static [(&'static str, &'static str)]| move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| PathBuf::from(v));
        assert_eq!(scripts_dir("5.1", &env(&[("BLENDER_USER_SCRIPTS", "/s"), ("HOME", "/h")])), Some(PathBuf::from("/s")));
        assert_eq!(scripts_dir("5.1", &env(&[("BLENDER_USER_RESOURCES", "/r"), ("HOME", "/h")])), Some(PathBuf::from("/r").join("scripts")));
        let platform = scripts_dir("5.1", &env(&[("HOME", "/h"), ("APPDATA", "/a"), ("XDG_CONFIG_HOME", "/x")])).unwrap();
        let expected = if cfg!(target_os = "macos") {
            PathBuf::from("/h/Library/Application Support/Blender/5.1/scripts")
        } else if cfg!(windows) {
            PathBuf::from("/a").join("Blender Foundation").join("Blender").join("5.1").join("scripts")
        } else {
            PathBuf::from("/x/blender/5.1/scripts")
        };
        assert_eq!(platform, expected);
        if !cfg!(any(target_os = "macos", windows)) {
            assert_eq!(scripts_dir("4.2", &env(&[("HOME", "/h")])), Some(PathBuf::from("/h/.config/blender/4.2/scripts")));
        }
        assert_eq!(scripts_dir("5.1", &env(&[])), None);
    }

    #[test]
    fn the_addon_is_written_with_the_launcher_path() {
        let dir = std::env::temp_dir().join(format!("texturecraft-addon-{}", std::process::id())).join("addons").join(ADDON_MODULE);
        let _ = std::fs::remove_dir_all(&dir);
        write_addon(&dir, Path::new("/apps/TextureCraft.app/Contents/MacOS/texturecraft")).unwrap();
        let init = std::fs::read_to_string(dir.join("__init__.py")).unwrap();
        assert_eq!(init, ADDON_INIT);
        for needle in [
            "bl_info",
            "bl_idname = \"texturecraft.open\"",
            "class VIEW3D_PT_texturecraft",
            "VIEW3D_MT_object",
            "texturecraft_path.json",
            "def register():",
            "def unregister():",
        ] {
            assert!(init.contains(needle), "{needle}");
        }
        assert!(std::fs::read_to_string(dir.join("blender_manifest.toml")).unwrap().contains("id = \"texturecraft\""));
        let record: Value = serde_json::from_slice(&std::fs::read(dir.join("texturecraft_path.json")).unwrap()).unwrap();
        assert_eq!(record["executable"], "/apps/TextureCraft.app/Contents/MacOS/texturecraft");
        assert_eq!(record["texturecraft"], env!("CARGO_PKG_VERSION"));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    /// With a real Blender: installs into `$BLENDER_USER_SCRIPTS` (point it at a scratch folder,
    /// and `BLENDER_USER_CONFIG` too, so your own Blender setup stays untouched) and checks the
    /// add-on is enabled.
    #[test]
    #[ignore]
    #[cfg(not(target_arch = "wasm32"))]
    fn installs_and_enables_in_a_real_blender() {
        let scripts = std::env::var("BLENDER_USER_SCRIPTS").expect("BLENDER_USER_SCRIPTS");
        let mut s = Session::new_project("t", texturecraft_geom::primitives::material_ball(), "builtin:material_ball", None, 128).unwrap();
        let r = crate::execute(&mut s, "blender.installAddon", &json!({})).unwrap();
        eprintln!("{r}");
        assert_eq!(r["enabled"], true, "{r}");
        assert!(Path::new(r["addon"].as_str().unwrap()).starts_with(&scripts));
        assert!(Path::new(r["addon"].as_str().unwrap()).join("__init__.py").is_file());
    }
}
