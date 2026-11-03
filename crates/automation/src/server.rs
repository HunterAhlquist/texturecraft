//! The MCP server (rmcp). Tool names are `snake_case`; each tool maps to one control-channel
//! method (see [`crate::rpc`]), answered in process (headless) or by the running app (bridge).

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::budgets::{MAX_PNG_SIDE, check_png, fit_png, json_text};
use crate::headless::batch_reply;
use crate::security::MAX_BATCH_STEPS;
use crate::workspace::authorize_command;
use crate::{AutomationError, BridgeClient, Headless, bad};

/// Where tools run.
pub enum Backend {
    /// An in-process session.
    Headless(Arc<Mutex<Headless>>),
    /// The running desktop app, over its control channel.
    Bridge(Arc<BridgeClient>),
}

#[derive(Clone)]
pub struct TexturecraftMcp {
    backend: Arc<Backend>,
    tool_router: ToolRouter<Self>,
}

// ---------------------------------------------------------------------------------------------
// Parameters (serialized back to JSON as the control method's params)

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct FilterParams {
    /// Only commands whose id or label contains this text (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RunParams {
    /// Command id from `command_list`, e.g. `layer.add.fill`, `mask.add.black`, `edit.undo`.
    pub id: String,
    /// Command parameters (see the command's `params` doc). Omitted = defaults; never a dialog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BatchParams {
    /// Commands run in order; the batch stops at the first error and reports its index.
    pub steps: Vec<RunParams>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct NewParams {
    /// Built-in mesh: material_ball (default), sphere, cube, rounded_cube, cylinder, torus, plane, crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<String>,
    /// Texture set resolution, a power of two from 128 to 8192 (default 1024).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Project template (channels, normal format, resolution, shader): a name from the
    /// `project.templates` command, e.g. "PBR - Metallic Roughness" (default) or "PBR - Specular Glossiness".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OpenParams {
    /// Forward-slash path relative to the read root.
    pub path: String,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct SaveParams {
    /// Forward-slash path relative to the write root (`.tcraft` is appended if missing). Omit to
    /// save to the project's own file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MaterialSaveParams {
    /// `.tcmat` file relative to the write root (the extension is appended if missing).
    pub path: String,
    /// Layer or folder id (default: the selected layer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<u64>,
    /// Material name (default: the layer name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ImportParams {
    /// Mesh file relative to the read root: .glb, .gltf (with its .bin files and images beside it), .fbx, .dae, .usd, .usda, .usdc,
    /// .usdz, .abc, .blend, .obj, .ply, .stl.
    pub path: String,
    /// Texture set resolution (default 2048).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BakeMeshParams {
    /// High-definition mesh file relative to the read root (any format `mesh_import` reads; base
    /// color textures feed the color map).
    pub path: String,
    /// Set the bake cage (a mesh with the low poly's triangles) instead of adding a high poly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cage: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct ReimportParams {
    /// Mesh file relative to the read root; omit to re-read the project's own copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Painted pixels follow the surface where the new mesh's UVs changed.
    #[serde(default, rename = "preserveStrokes", skip_serializing_if = "Option::is_none")]
    pub preserve_strokes: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MeshExportParams {
    /// Output file relative to the write root; the format's extension is appended if missing.
    pub path: String,
    /// obj (MTL and textures written next to it), glb (textures embedded), usda, usdc (textures
    /// written next to it) or usdz (textures packaged); default: from the extension, else obj.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExportParams {
    /// Output folder relative to the write root (created if missing; `.` = the root).
    pub dir: String,
    /// Preset name from `command_run export.presets`, e.g. "glTF PBR Metal Roughness".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Texture set index or "all" (default all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<Value>,
    /// File format override: png, jpg, tga, tif, exr, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// none | dilation | dilationDefault | infinite
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<String>,
    /// Output size in pixels (power of two).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct BakeParams {
    /// Texture set index or "all" (default: the active one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<Value>,
    /// Bake resolution (power of two).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
    /// Mesh map ids, e.g. ["ambientOcclusion", "curvature"] (default: the set's bake settings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maps: Option<Vec<String>>,
    /// Cast the rays on the CPU even when a GPU is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct StrokeParams {
    /// Stroke points `[[x, y], …]` in viewport pixels (origin top-left), as in `viewport_render`.
    pub points: Vec<[f64; 2]>,
    /// Viewport size `[w, h]` the points refer to (default: the session viewport, 1280×800).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<[f64; 2]>,
    /// Base color: `[r, g, b]` linear 0..1 or `"#rrggbb"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Value>,
    /// 0..1
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f64>,
    /// 0..1
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f64>,
    /// -1..1
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// Brush size 0..100 (percent of the largest brush).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// Brush hardness 0..100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardness: Option<f64>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct RenderParams {
    /// `3d` (default) or `2d` (the UV view of the active texture set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    /// Pixels, 16..2048 (default: the session viewport, the space `paint_stroke` points use).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Camera yaw in degrees for this render only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    /// Camera pitch in degrees (-89..89) for this render only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    /// Path trace the 3D view with this many samples per pixel (1..4096) instead of the
    /// real-time viewport: slower, physically based light transport; on the GPU when it has
    /// compute shaders, otherwise on the CPU (no GPU needed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
    /// Light bounces when path tracing (default 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounces: Option<u32>,
    /// Path trace on the CPU even when a GPU is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct ScreenshotParams {
    /// `3d` (default) or `2d`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MenuParams {
    /// Menu item id from the app's `ui.menu.list`, e.g. `file.save`, `viewport.frame`.
    pub id: String,
}

// ---------------------------------------------------------------------------------------------
// Results

fn ok_json(v: &Value) -> CallToolResult {
    match json_text(v) {
        Ok(text) => CallToolResult::success(vec![Content::text(text)]),
        Err(e) => fail(e),
    }
}

fn fail(e: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(e.to_string())])
}

fn to_result(r: Result<Value, AutomationError>) -> Result<CallToolResult, McpError> {
    Ok(match r {
        Ok(v) => ok_json(&v),
        Err(e) => fail(e),
    })
}

/// A `{width, height, png: base64}` reply (render or app screenshot) as image content.
fn image_result(r: Result<Value, AutomationError>, note: &str, refit: bool) -> Result<CallToolResult, McpError> {
    let png = r.and_then(|v| {
        let b64 = v.get("png").and_then(Value::as_str).ok_or_else(|| AutomationError::Other("the reply has no PNG".into()))?;
        if !refit {
            check_png(b64.len() / 4 * 3)?;
            return Ok(b64.to_string());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| AutomationError::Other(format!("bad PNG data: {e}")))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(fit_png(bytes, MAX_PNG_SIDE)?))
    });
    Ok(match png {
        Ok(b64) => CallToolResult::success(vec![Content::image(b64, "image/png"), Content::text(note.to_string())]),
        Err(e) => fail(e),
    })
}

fn params<T: Serialize>(p: &T) -> Value {
    serde_json::to_value(p).unwrap_or(Value::Null)
}

impl TexturecraftMcp {
    pub fn headless(h: Headless) -> Self {
        Self::with_backend(Backend::Headless(Arc::new(Mutex::new(h))))
    }

    pub fn bridge(client: BridgeClient) -> Self {
        Self::with_backend(Backend::Bridge(Arc::new(client)))
    }

    pub fn with_backend(backend: Backend) -> Self {
        TexturecraftMcp { backend: Arc::new(backend), tool_router: Self::tool_router() }
    }

    /// Serve MCP on stdin/stdout until the client disconnects.
    pub async fn serve_stdio(self) -> Result<(), AutomationError> {
        let running = self.serve(rmcp::transport::stdio()).await.map_err(|e| AutomationError::Other(format!("MCP start: {e}")))?;
        running.waiting().await.map_err(|e| AutomationError::Other(e.to_string()))?;
        Ok(())
    }

    fn is_bridge(&self) -> bool {
        matches!(*self.backend, Backend::Bridge(_))
    }

    /// One control-channel method on the backend.
    async fn call(&self, method: &str, p: Value) -> Result<Value, AutomationError> {
        match &*self.backend {
            Backend::Headless(h) => {
                let (h, m) = (h.clone(), method.to_string());
                // A panicking command poisons the lock, but edits run on a document snapshot
                // (`Session::edit`), so the session stays consistent: keep serving.
                tokio::task::spawn_blocking(move || h.lock().unwrap_or_else(PoisonError::into_inner).handle(&m, &p))
                    .await
                    .unwrap_or_else(|_| Err(AutomationError::Other(format!("internal error in `{method}`"))))
            }
            Backend::Bridge(b) => match method {
                "command.run" => {
                    let id = p.get("id").and_then(Value::as_str).unwrap_or_default();
                    authorize_command(id, p.get("params").unwrap_or(&Value::Null))?;
                    b.call(method, p).await
                }
                "project.save" if p.get("path").is_none() => b.call("ui.menu.invoke", json!({"id": "file.save"})).await,
                "mesh.reimport" if p.get("path").is_none() => {
                    b.call("command.run", json!({"id": "file.reimportMesh", "params": {"preserveStrokes": p.get("preserveStrokes")}})).await
                }
                "project.save" | "project.open" | "mesh.import" | "mesh.reimport" | "mesh.export" | "bake.mesh.add" | "resource.import" | "material.save"
                | "material.load" | "textures.export" | "command.batch" | "render" => {
                    Err(bad(format!("`{method}` with paths works in headless mode only: the running app reads and writes its own files (use ui_menu_invoke)")))
                }
                _ => b.call(method, p).await,
            },
        }
    }

    async fn run(&self, id: &str, p: Value) -> Result<Value, AutomationError> {
        self.call("command.run", json!({"id": id, "params": p})).await
    }

    async fn bridge_only(&self, method: &str, p: Value) -> Result<Value, AutomationError> {
        if self.is_bridge() {
            self.call(method, p).await
        } else {
            Err(bad(format!(
                "`{method}` needs the running app: start `texturecraft --control <port>` and run `texturecraft-cli mcp --bridge <port>` (headless renders: viewport_render)"
            )))
        }
    }

    async fn stroke(&self, p: StrokeParams) -> Result<Value, AutomationError> {
        self.run("tool.paint", json!({})).await?;
        if let Some(c) = p.color {
            self.run("tool.material", json!({"channel": "baseColor", "color": c})).await?;
        }
        for (channel, v) in [("roughness", p.roughness), ("metallic", p.metallic), ("height", p.height)] {
            if let Some(v) = v {
                self.run("tool.material", json!({"channel": channel, "value": v})).await?;
            }
        }
        if p.size.is_some() || p.hardness.is_some() {
            self.run("tool.brush", json!({"size": p.size, "hardness": p.hardness})).await?;
        }
        self.run("tool.stroke", json!({"points": p.points, "viewport": p.viewport})).await
    }
}

// ---------------------------------------------------------------------------------------------
// Tools

#[tool_router]
impl TexturecraftMcp {
    #[tool(description = "The whole session as JSON: project, mesh, texture sets (resolution, channels, baked maps), the layer stack \
        top to bottom (fill sources, masks, effects, painted tile counts), tool, camera, display and history.")]
    async fn session_inspect(&self) -> Result<CallToolResult, McpError> {
        to_result(self.call("session.inspect", json!({})).await)
    }

    #[tool(description = "List engine commands: id, label, menu path, shortcut, params doc and whether it can run now.")]
    async fn command_list(&self, Parameters(p): Parameters<FilterParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("command.list", params(&p)).await)
    }

    #[tool(description = "Run an engine command by id with JSON params (see command_list). Runs with defaults, never opens a \
        dialog. Commands given host paths (file.open, file.save, resource.import, material.save, …) are disabled: use the \
        workspace tools (project_open, project_save, mesh_import, mesh_reimport, mesh_export, bake_mesh_add, resource_import, \
        material_save, material_load, textures_export).")]
    async fn command_run(&self, Parameters(p): Parameters<RunParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("command.run", params(&p)).await)
    }

    #[tool(description = "Run several commands in order: steps [{id, params}]. Stops at the first error. Returns {completed, \
        results, failed: {index, id, error} | null}; a failed batch is a tool error.")]
    async fn command_batch(&self, Parameters(p): Parameters<BatchParams>) -> Result<CallToolResult, McpError> {
        if p.steps.len() > MAX_BATCH_STEPS {
            return Ok(fail(format!("at most {MAX_BATCH_STEPS} steps per batch")));
        }
        let reply = if self.is_bridge() {
            let mut results = Vec::new();
            let mut failed = None;
            for (i, s) in p.steps.iter().enumerate() {
                match self.call("command.run", params(s)).await {
                    Ok(v) => results.push(v),
                    Err(e) => {
                        failed = Some((i, s.id.as_str(), e.to_string()));
                        break;
                    }
                }
            }
            batch_reply(results, failed)
        } else {
            match self.call("command.batch", params(&p)).await {
                Ok(v) => v,
                Err(e) => return Ok(fail(e)),
            }
        };
        Ok(if reply["failed"].is_null() { ok_json(&reply) } else { fail(json_text(&reply).unwrap_or_else(|e| e.to_string())) })
    }

    #[tool(description = "Start a new project on a built-in mesh (one texture set per material, each with an empty paint layer). \
        Returns the session.")]
    async fn project_new(&self, Parameters(p): Parameters<NewParams>) -> Result<CallToolResult, McpError> {
        to_result(self.run("project.new", params(&p)).await)
    }

    #[tool(description = "Open a .tcraft project or a mesh file (path relative to the read root). Returns the session.")]
    async fn project_open(&self, Parameters(p): Parameters<OpenParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("project.open", params(&p)).await)
    }

    #[tool(description = "Save the project as .tcraft (path relative to the write root; omit to save to its own file).")]
    async fn project_save(&self, Parameters(p): Parameters<SaveParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("project.save", params(&p)).await)
    }

    #[tool(description = "New project from a mesh file (path relative to the read root): one texture set per material. Returns \
        the session; import warnings are in the session log.")]
    async fn mesh_import(&self, Parameters(p): Parameters<ImportParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("mesh.import", params(&p)).await)
    }

    #[tool(description = "Reimport the project's mesh from a file (path relative to the read root) or, without a path, from \
        the project's own copy. Texture sets map by material index; `preserveStrokes` re-projects painted pixels where the UVs \
        changed. Returns {preserved?}.")]
    async fn mesh_reimport(&self, Parameters(p): Parameters<ReimportParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("mesh.reimport", params(&p)).await)
    }

    #[tool(description = "Export the mesh with its textures (obj, glb, usda, usdc or usdz; path relative to the write root). \
        Returns {path, textures?}.")]
    async fn mesh_export(&self, Parameters(p): Parameters<MeshExportParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("mesh.export", params(&p)).await)
    }

    #[tool(description = "Add a high-definition mesh for baking (path relative to the read root), or with `cage: true` set the \
        bake cage. Files next to a .gltf or USD layer (buffers, base color textures) come along. Returns {name, meshes}; bake \
        with `bake`.")]
    async fn bake_mesh_add(&self, Parameters(p): Parameters<BakeMeshParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("bake.mesh.add", params(&p)).await)
    }

    #[tool(description = "Import an image (png, jpg, tga, tif, bmp, webp, exr, hdr; path relative to the read root) as a project \
        resource. Returns its name, for command_run layer.add.image {name} or tool.stencil {image}.")]
    async fn resource_import(&self, Parameters(p): Parameters<OpenParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("resource.import", params(&p)).await)
    }

    #[tool(description = "Save a layer or folder (default: the selected one) as a smart material file (.tcmat) below the write root.")]
    async fn material_save(&self, Parameters(p): Parameters<MaterialSaveParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("material.save", params(&p)).await)
    }

    #[tool(description = "Add a smart material from a .tcmat file (relative to the read root) above the selection. Returns its layer.")]
    async fn material_load(&self, Parameters(p): Parameters<OpenParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("material.load", params(&p)).await)
    }

    #[tool(description = "Export textures with a preset into a folder relative to the write root. Returns the written files and \
        warnings.")]
    async fn textures_export(&self, Parameters(p): Parameters<ExportParams>) -> Result<CallToolResult, McpError> {
        to_result(self.call("textures.export", params(&p)).await)
    }

    #[tool(description = "Bake mesh maps (ambient occlusion, curvature, thickness, …) for generators and smart materials. Rays run \
        on the GPU when there is one (`cpu: true` forces the CPU); per-map ray settings are `bake.settings` parameters (command_run).")]
    async fn bake(&self, Parameters(p): Parameters<BakeParams>) -> Result<CallToolResult, McpError> {
        to_result(self.run("bake.run", params(&p)).await)
    }

    #[tool(description = "Paint a stroke with the paint tool on the selected layer (or its mask target): sets the brush material \
        (color, roughness, metallic, height) and size/hardness, then strokes through `points` in viewport pixels. See \
        viewport_render for where the mesh is.")]
    async fn paint_stroke(&self, Parameters(p): Parameters<StrokeParams>) -> Result<CallToolResult, McpError> {
        to_result(self.stroke(p).await)
    }

    #[tool(description = "Headless: render the 3D view (or the 2D UV view) as a PNG image, up to 2048 px a side. Camera yaw/pitch \
        apply to this render only. In bridge mode use viewport_screenshot.")]
    async fn viewport_render(&self, Parameters(p): Parameters<RenderParams>) -> Result<CallToolResult, McpError> {
        let note = format!("render {}", params(&p));
        image_result(self.call("render", params(&p)).await, &note, false)
    }

    #[tool(description = "Bridge mode: PNG of the live app's 3D or 2D viewport.")]
    async fn viewport_screenshot(&self, Parameters(p): Parameters<ScreenshotParams>) -> Result<CallToolResult, McpError> {
        image_result(self.bridge_only("viewport.screenshot", params(&p)).await, "screenshot of the app viewport", true)
    }

    #[tool(description = "Bridge mode: PNG of the whole app window (downscaled to 2048 px a side at most).")]
    async fn ui_screenshot(&self) -> Result<CallToolResult, McpError> {
        image_result(self.bridge_only("ui.screenshot", json!({})).await, "screenshot of the app window", true)
    }

    #[tool(description = "Bridge mode: the app's UI state (panels, windows, mode, layout, viewport rect, toasts).")]
    async fn ui_inspect(&self) -> Result<CallToolResult, McpError> {
        to_result(self.bridge_only("ui.inspect", json!({})).await)
    }

    #[tool(description = "Bridge mode: activate a menu item by id like a click (may open a window). ui.menu.list lists ids.")]
    async fn ui_menu_invoke(&self, Parameters(p): Parameters<MenuParams>) -> Result<CallToolResult, McpError> {
        to_result(self.bridge_only("ui.menu.invoke", params(&p)).await)
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "texturecraft",
    instructions = "TextureCraft 3D texture painter. Every edit is an engine command: `command_list` shows ids and params, \
        `command_run`/`command_batch` run them. Start with `project_new` (built-in mesh) or `project_open`/`mesh_import` (files \
        relative to the workspace root), check your work with `session_inspect` and `viewport_render`, paint with `paint_stroke`, \
        bake with `bake` (high polys and the cage: `bake_mesh_add`), then `project_save` (.tcraft), `textures_export` and \
        `mesh_export`. Undo: command_run edit.undo."
)]
impl ServerHandler for TexturecraftMcp {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::Filesystem;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_panic_does_not_wedge_the_session() {
        let s = texturecraft_engine::Session::new_project("t", texturecraft_engine::geom::primitives::cube(2.0), "builtin:cube", None, 128).unwrap();
        let mcp = TexturecraftMcp::headless(Headless::new(s, Filesystem::Denied));
        let Backend::Headless(h) = &*mcp.backend else { panic!("headless") };
        let h2 = h.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _g = h2.lock().unwrap();
            panic!("boom")
        })
        .await;
        assert!(mcp.call("session.inspect", json!({})).await.is_ok());
    }

    #[test]
    fn oversized_replies_become_errors() {
        let r = ok_json(&json!("x".repeat(crate::budgets::MAX_RESPONSE_BYTES)));
        assert_eq!(r.is_error, Some(true));
    }
}
