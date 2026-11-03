//! Headless TextureCraft command line: inspect, script, bake, export and render projects, and
//! serve agents over MCP or JSON lines. Never opens a window.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use texturecraft_automation::security::{client_token, server_token};
use texturecraft_automation::{AuthorizedWorkspace, BridgeClient, Filesystem, Headless, RenderOptions, TexturecraftMcp, rpc};
use texturecraft_engine::core::PROJECT_EXT;
use texturecraft_engine::formats::{MeshReport, import_mesh, import_mesh_bytes, mesh_report};

const USAGE: &str = "\
texturecraft-cli: headless TextureCraft

USAGE:
  texturecraft-cli --version
  texturecraft-cli info <project.tcraft | mesh file | builtin:<id>>
      Texture sets, layers, triangles, materials and the UV report, as JSON.
  texturecraft-cli commands [--filter <text>]
      List the engine commands (id, label, params).
  texturecraft-cli run <input> <command-id> [params-json] [-o <out.tcraft>]
      Run one command and print its JSON result; -o saves the project.
  texturecraft-cli batch <script.json>
      Run [{\"id\", \"params\"}, ...] or {\"open\": <input>, \"steps\": [...], \"save\": <out.tcraft>}.
  texturecraft-cli bake <input> [-o <out.tcraft>] [--resolution <px>] [--maps <id,id,...>]
      Bake mesh maps for every texture set (a project is saved in place without -o).
  texturecraft-cli export <input> <dir> [--preset <name>] [--format <ext>] [--padding <mode>] [--size <px>]
      Export textures (default preset: Document channels + Normal + AO).
  texturecraft-cli render <input> <out.png> [--size <W>x<H>] [--view 3d|2d] [--camera <name>] [--yaw <deg>]
                   [--pitch <deg>] [--samples <n>] [--bounces <n>] [--cpu]
      Render the viewport to PNG (needs a GPU or software adapter); --camera looks through a
      camera saved in the project (camera.add); --samples path traces it instead (Rendering
      mode), on the GPU when it has compute shaders, else (or with --cpu) on the CPU.
  texturecraft-cli new <mesh file | builtin:<id>> <out.tcraft> [--resolution <px>] [--sample]
      New project; --sample bakes and applies a smart material to every texture set. The
      sample's mesh maps are baked at the resolution up to 1024 px (most of the file's size),
      so a 2048 sample project is as large as a 1024 one.
  texturecraft-cli mcp [--bridge <port>] [--token <token>] [--read-root <dir>] [--write-root <dir>]
      MCP server on stdio: headless (file access only below the roots), or bridged to
      `texturecraft --control <port>`. --automation-read-root / --automation-write-root are
      the same as --read-root / --write-root (mcp and serve).
  texturecraft-cli serve [--port <port>] [--read-root <dir>] [--write-root <dir>]
      JSON lines ({\"id\",\"method\",\"params\"}) on stdio, or on 127.0.0.1:<port> with a token.

<input> is a .tcraft project, a mesh file (.glb .gltf .fbx .dae .usd .usda .usdc .usdz .abc .blend .obj .ply .stl)
or builtin:<id> (material_ball, sphere, cube, rounded_cube, cylinder, torus, plane, crate, udim_plane).
See docs/control-protocol.md.
";

/// (name, flags taking a value, bare flags)
const SUBCOMMANDS: &[(&str, &[&str], &[&str])] = &[
    ("info", &[], &[]),
    ("commands", &["--filter"], &[]),
    ("run", &["-o"], &[]),
    ("batch", &[], &[]),
    ("bake", &["-o", "--resolution", "--maps"], &[]),
    ("export", &["--preset", "--format", "--padding", "--size"], &[]),
    ("render", &["--size", "--view", "--camera", "--yaw", "--pitch", "--samples", "--bounces"], &["--cpu"]),
    ("new", &["--resolution"], &["--sample"]),
    ("mcp", &["--bridge", "--token", "--read-root", "--write-root", "--automation-read-root", "--automation-write-root"], &[]),
    ("serve", &["--port", "--read-root", "--write-root", "--automation-read-root", "--automation-write-root"], &[]),
];

struct Args {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(f, _)| f == k).and_then(|(_, v)| v.as_deref())
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == k)
    }
    fn num<T: std::str::FromStr>(&self, k: &str) -> Result<Option<T>, String> {
        self.get(k).map(|v| v.parse().map_err(|_| format!("{k}: `{v}` is not a valid number"))).transpose()
    }
    fn pos(&self, n: usize, what: &str) -> Result<&str, String> {
        self.pos.get(n).map(String::as_str).ok_or_else(|| format!("missing {what}"))
    }
}

fn parse(values: &[&str], bare: &[&str], args: &[String]) -> Result<Args, String> {
    let mut a = Args { pos: Vec::new(), flags: Vec::new() };
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let (k, inline) = match s.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        if values.contains(&k) {
            let v = match inline {
                Some(v) => v,
                None => it.next().ok_or_else(|| format!("{k} needs a value"))?.clone(),
            };
            a.flags.push((k.to_string(), Some(v)));
        } else if bare.contains(&k) && inline.is_none() {
            a.flags.push((k.to_string(), None));
        } else if k.starts_with('-') && k.len() > 1 && k.parse::<f64>().is_err() {
            return Err(format!("unknown option `{s}`"));
        } else {
            a.pos.push(s.clone());
        }
    }
    Ok(a)
}

fn version() -> String {
    let sha = env!("TEXTURECRAFT_BUILD_SHA");
    let short = sha.get(..7).unwrap_or(sha);
    let date = env!("TEXTURECRAFT_BUILD_DATE");
    match (short.is_empty(), date.is_empty()) {
        (true, true) => format!("TextureCraft {}", texturecraft_engine::VERSION),
        (true, false) => format!("TextureCraft {} ({date})", texturecraft_engine::VERSION),
        _ => format!("TextureCraft {} ({short} {date})", texturecraft_engine::VERSION),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => {
            eprint!("{USAGE}");
            2
        }
        Some("--version" | "-V" | "version") => {
            println!("{}", version());
            0
        }
        Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            0
        }
        Some(cmd) => match SUBCOMMANDS.iter().find(|(n, _, _)| *n == cmd) {
            None => {
                eprintln!("texturecraft-cli: unknown command `{cmd}`\n\n{USAGE}");
                2
            }
            Some(_) if args.iter().any(|a| a == "--help" || a == "-h") => {
                print!("{USAGE}");
                0
            }
            Some((name, values, bare)) => match parse(values, bare, args.get(1..).unwrap_or_default()) {
                Err(e) => {
                    eprintln!("texturecraft-cli {name}: {e}\n\n{USAGE}");
                    2
                }
                Ok(a) => match dispatch(name, &a) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("texturecraft-cli {name}: error: {e}");
                        1
                    }
                },
            },
        },
    };
    std::process::exit(code);
}

type R = Result<(), String>;

fn dispatch(name: &str, a: &Args) -> R {
    match name {
        "info" => info(a),
        "commands" => commands(a),
        "run" => run(a),
        "batch" => batch(a),
        "bake" => bake(a),
        "export" => export(a),
        "render" => render(a),
        "new" => new(a),
        "mcp" => mcp(a),
        "serve" => serve(a),
        other => Err(format!("unknown command `{other}`")),
    }
}

fn s(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn print_json(v: &Value) -> R {
    println!("{}", serde_json::to_string_pretty(v).map_err(s)?);
    Ok(())
}

fn is_project(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXT))
}

/// A trusted local session on `input`: a project, a mesh file or `builtin:<id>`.
fn open(input: &str, resolution: Option<u32>) -> Result<Headless, String> {
    let mut h = Headless::new(Headless::default_session().map_err(s)?, Filesystem::TrustedLocal);
    match input.strip_prefix("builtin:") {
        Some(id) => h.command_run("project.new", json!({"mesh": id, "resolution": resolution.unwrap_or(1024), "name": id})).map(|_| ()),
        None if is_project(input) => h.open(input).map(|_| ()),
        None => h.import_mesh(input, resolution, None).map(|_| ()),
    }
    .map_err(s)?;
    Ok(h)
}

fn save(h: &mut Headless, out: &str) -> R {
    let r = h.save(Some(out)).map_err(s)?;
    eprintln!("saved {}", r["path"].as_str().unwrap_or_default());
    Ok(())
}

fn report_json(r: &MeshReport) -> Value {
    json!({
        "triangles": r.triangles, "vertices": r.vertices, "materials": r.materials, "uvsGenerated": r.uvs_generated,
        "uvOutOfRange": r.uv_out_of_range, "overlapRatio": r.overlap_ratio, "zeroAreaUvTriangles": r.zero_area_uv_triangles, "warnings": r.warnings,
    })
}

fn info(a: &Args) -> R {
    let file = a.pos(0, "<file>")?;
    if let Some(id) = file.strip_prefix("builtin:") {
        let m = texturecraft_engine::geom::primitives::builtin(id).ok_or_else(|| format!("unknown built-in mesh `{id}`"))?;
        let report = mesh_report(&m);
        return print_json(&json!({
            "file": file, "kind": "mesh", "triangles": m.triangle_count(), "vertices": m.vertex_count(),
            "materials": m.materials, "textureSets": m.materials, "uv": report_json(&report),
        }));
    }
    if !is_project(file) {
        let m = import_mesh(Path::new(file)).map_err(|e| format!("cannot read {file}: {e}"))?;
        return print_json(&json!({
            "file": file, "kind": "mesh", "triangles": m.mesh.triangle_count(), "vertices": m.mesh.vertex_count(),
            "materials": m.mesh.materials, "textureSets": m.mesh.materials, "uv": report_json(&m.report),
        }));
    }
    let h = open(file, None)?;
    let mut v = h.inspect();
    v["file"] = json!(file);
    v["kind"] = json!("project");
    // The embedded mesh is re-imported for its UV report (built-in meshes have none).
    v["uv"] = match &h.session.mesh_source {
        Some((ext, bytes)) => import_mesh_bytes(&format!("mesh.{ext}"), bytes).map(|m| report_json(&m.report)).map_err(s)?,
        None => Value::Null,
    };
    print_json(&v)
}

fn commands(a: &Args) -> R {
    let h = open("builtin:cube", Some(128))?;
    for c in h.command_list(a.get("--filter")).as_array().into_iter().flatten() {
        let f = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        println!("{:<28} {:<36} {}", f("id"), f("label"), f("params"));
    }
    Ok(())
}

fn run(a: &Args) -> R {
    let (input, id) = (a.pos(0, "<input>")?, a.pos(1, "<command-id>")?);
    let params: Value = match a.pos.get(2) {
        Some(j) => serde_json::from_str(j).map_err(|e| format!("params are not JSON: {e}"))?,
        None => json!({}),
    };
    let mut h = open(input, None)?;
    print_json(&h.command_run(id, params).map_err(s)?)?;
    match a.get("-o") {
        Some(out) => save(&mut h, out),
        None => Ok(()),
    }
}

fn batch(a: &Args) -> R {
    let path = a.pos(0, "<script.json>")?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let script: Value = serde_json::from_str(&text).map_err(|e| format!("{path} is not JSON: {e}"))?;
    let steps =
        script.as_array().or_else(|| script.get("steps").and_then(Value::as_array)).ok_or("the script must be a list of steps or {\"steps\": [...]}")?;
    let field = |k: &str| script.get(k).and_then(Value::as_str);
    let mut h = open(field("open").unwrap_or("builtin:material_ball"), None)?;
    let reply = h.batch(steps);
    print_json(&reply)?;
    if let Some(f) = reply.get("failed").filter(|f| !f.is_null()) {
        return Err(format!("step {} (`{}`) failed: {}", f["index"], f["id"].as_str().unwrap_or_default(), f["error"].as_str().unwrap_or_default()));
    }
    match field("save") {
        Some(out) => save(&mut h, out),
        None => Ok(()),
    }
}

fn bake(a: &Args) -> R {
    let input = a.pos(0, "<input>")?;
    let out = a.get("-o").or(is_project(input).then_some(input)).ok_or("give -o <out.tcraft> to save the baked project")?;
    let mut p = json!({"ts": "all"});
    if let Some(r) = a.num::<u32>("--resolution")? {
        p["resolution"] = json!(r);
    }
    if let Some(m) = a.get("--maps") {
        p["maps"] = json!(m.split(',').map(str::trim).filter(|m| !m.is_empty()).collect::<Vec<_>>());
    }
    let mut h = open(input, None)?;
    h.command_run("bake.run", p).map_err(s)?;
    save(&mut h, out)?;
    print_json(&h.inspect()["textureSets"])
}

fn export(a: &Args) -> R {
    let (input, dir) = (a.pos(0, "<input>")?, a.pos(1, "<dir>")?);
    let mut p = json!({"dir": dir});
    for k in ["preset", "format", "padding"] {
        if let Some(v) = a.get(&format!("--{k}")) {
            p[k] = json!(v);
        }
    }
    if let Some(v) = a.num::<u32>("--size")? {
        p["size"] = json!(v);
    }
    let mut h = open(input, None)?;
    let r = h.export(&p).map_err(s)?;
    for w in r["warnings"].as_array().into_iter().flatten() {
        eprintln!("warning: {}", w.as_str().unwrap_or_default());
    }
    for f in r["files"].as_array().into_iter().flatten() {
        println!("{}", f.as_str().unwrap_or_default());
    }
    Ok(())
}

fn render(a: &Args) -> R {
    let (input, out) = (a.pos(0, "<input>")?, a.pos(1, "<out.png>")?);
    let (width, height) = match a.get("--size") {
        Some(sz) => {
            let (w, h) = sz.split_once(['x', 'X']).ok_or("--size must be <W>x<H>, e.g. 1280x800")?;
            (Some(w.parse().map_err(|_| "bad --size width")?), Some(h.parse().map_err(|_| "bad --size height")?))
        }
        None => (None, None),
    };
    let o = RenderOptions {
        view: a.get("--view").map(str::to_string),
        width,
        height,
        yaw: a.num("--yaw")?,
        pitch: a.num("--pitch")?,
        samples: a.num("--samples")?,
        bounces: a.num("--bounces")?,
        cpu: a.has("--cpu"),
    };
    let mut h = open(input, None)?;
    if let Some(name) = a.get("--camera") {
        h.command_run("camera.select", json!({ "name": name })).map_err(s)?;
    }
    let t0 = std::time::Instant::now();
    let r = h.render_png(&o).map_err(s)?;
    let secs = t0.elapsed().as_secs_f64();
    std::fs::write(out, &r.png).map_err(|e| format!("cannot write {out}: {e}"))?;
    eprintln!("wrote {out} ({}x{}, {}, {secs:.2} s)", r.width, r.height, r.by);
    Ok(())
}

fn new(a: &Args) -> R {
    let (input, out) = (a.pos(0, "<mesh | builtin:id>")?, a.pos(1, "<out.tcraft>")?);
    if is_project(input) {
        return Err(format!("{input} is already a project"));
    }
    let res = a.num::<u32>("--resolution")?;
    let mut h = open(input, res)?;
    if a.has("--sample") {
        let bake_res = h.session.doc.project.texture_sets.first().map_or(1024, |t| t.resolution.min(1024));
        h.command_run("bake.run", json!({"ts": "all", "resolution": bake_res})).map_err(s)?;
        for ts in 0..h.session.set_count() {
            h.command_run("textureSet.select", json!({"ts": ts})).map_err(s)?;
            h.command_run("material.apply", json!({"material": "steel_painted_worn"})).map_err(s)?;
        }
        h.command_run("textureSet.select", json!({"ts": 0})).map_err(s)?;
    }
    save(&mut h, out)
}

fn workspace(a: &Args) -> Result<Filesystem, String> {
    // `--automation-*-root` is the spelling the other crafting apps use.
    let root = |k: &str| a.get(&format!("--automation-{k}")).or_else(|| a.get(&format!("--{k}"))).map(Path::new);
    let (r, w) = (root("read-root"), root("write-root"));
    if r.is_none() && w.is_none() {
        return Ok(Filesystem::Denied);
    }
    Ok(Filesystem::Workspace(AuthorizedWorkspace::new(r, w).map_err(s)?))
}

fn port_of(v: &str) -> Result<u16, String> {
    let p = v.strip_prefix("127.0.0.1:").or_else(|| v.strip_prefix("localhost:")).unwrap_or(v);
    p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| format!("`{v}` is not a loopback port (1-65535)"))
}

fn mcp(a: &Args) -> R {
    let server = match a.get("--bridge") {
        Some(b) => {
            let port = port_of(b)?;
            TexturecraftMcp::bridge(BridgeClient::new(port, client_token(port, a.get("--token").map(str::to_string)).map_err(s)?))
        }
        None => TexturecraftMcp::headless(Headless::new(Headless::default_session().map_err(s)?, workspace(a)?)),
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(s)?;
    rt.block_on(server.serve_stdio()).map_err(s)
}

fn serve(a: &Args) -> R {
    let h = Arc::new(Mutex::new(Headless::new(Headless::default_session().map_err(s)?, workspace(a)?)));
    match a.get("--port") {
        None => rpc::serve_lines(&h, std::io::stdin().lock(), std::io::stdout().lock()).map_err(s),
        Some(p) => {
            // Bind first: a port in use must not replace its server's token file, and port 0's
            // token file is named after the port actually picked.
            let listener = rpc::bind_tcp(port_of(p)?).map_err(s)?;
            let addr = listener.local_addr().map_err(s)?;
            let (token, file) = server_token(addr.port()).map_err(s)?;
            match file {
                Some(f) => eprintln!("texturecraft-cli: serving on {addr}; token in {}", f.display()),
                None => eprintln!("texturecraft-cli: serving on {addr}; token from TEXTURECRAFT_CONTROL_TOKEN"),
            }
            rpc::serve_listener(listener, h, token).map_err(s)
        }
    }
}
