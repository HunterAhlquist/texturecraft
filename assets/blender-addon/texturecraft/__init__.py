# TextureCraft add-on for Blender: a button that opens the selected objects (or the whole
# scene) in TextureCraft to paint their textures.
#
# TextureCraft installs this add-on itself (Edit > Install Blender add-on...) and writes
# texturecraft_path.json next to this file with the path of its executable; set the executable
# in the add-on preferences when TextureCraft lives somewhere else.
#
# Part of TextureCraft (https://github.com/storytold/texturecraft), MIT OR Apache-2.0.

bl_info = {
    "name": "TextureCraft",
    "author": "TextureCraft contributors",
    "version": (1, 0, 0),
    "blender": (2, 80, 0),
    "location": "3D Viewport > Sidebar > TextureCraft, and the Object menu",
    "description": "Open the selected objects (or the whole scene) in TextureCraft to paint their textures",
    "doc_url": "https://github.com/storytold/texturecraft",
    "category": "Import-Export",
}

import json
import os
import shutil
import subprocess
import sys
import tempfile

import bpy
from bpy.props import EnumProperty, StringProperty

# The module name this add-on is registered under ("texturecraft", or
# "bl_ext.<repository>.texturecraft" when installed as a Blender 4.2+ extension).
ADDON = __name__


def _recorded_executable():
    """The TextureCraft executable the installer recorded next to this file, else $TEXTURECRAFT."""
    here = os.path.dirname(os.path.abspath(__file__))
    try:
        with open(os.path.join(here, "texturecraft_path.json"), encoding="utf-8") as f:
            path = json.load(f).get("executable", "")
        if path and os.path.exists(path):
            return path
    except (OSError, ValueError):
        pass
    return os.environ.get("TEXTURECRAFT", "")


def find_executable():
    """A TextureCraft executable: the recorded one, one on PATH, or a usual install location."""
    recorded = _recorded_executable()
    if recorded:
        return recorded
    exe = "texturecraft.exe" if sys.platform.startswith("win") else "texturecraft"
    candidates = []
    on_path = shutil.which(exe)
    if on_path:
        candidates.append(on_path)
    if sys.platform == "darwin":
        candidates += [
            "/Applications/TextureCraft.app/Contents/MacOS/texturecraft",
            os.path.expanduser("~/Applications/TextureCraft.app/Contents/MacOS/texturecraft"),
        ]
    elif sys.platform.startswith("win"):
        for base in (os.environ.get("ProgramFiles", r"C:\Program Files"), os.environ.get("LOCALAPPDATA", "")):
            if base:
                candidates += [os.path.join(base, "TextureCraft", exe), os.path.join(base, "Programs", "TextureCraft", exe)]
    else:
        candidates += [
            "/usr/bin/texturecraft",
            "/usr/local/bin/texturecraft",
            os.path.expanduser("~/.local/bin/texturecraft"),
            "/opt/texturecraft/texturecraft",
        ]
    for candidate in candidates:
        if os.path.isfile(candidate):
            return candidate
    return ""


def _launch(executable, path):
    """Start TextureCraft on `path` without waiting for it."""
    if sys.platform == "darwin" and executable.endswith(".app"):
        subprocess.Popen(["open", "-a", executable, path])
    else:
        subprocess.Popen([executable, path], close_fds=True)


def _prefs(context):
    return context.preferences.addons[ADDON].preferences


class TextureCraftPreferences(bpy.types.AddonPreferences):
    bl_idname = ADDON

    executable: StringProperty(
        name="TextureCraft executable",
        description="The TextureCraft program to open models with (found automatically when empty)",
        subtype='FILE_PATH',
        default="",
    )
    mode: EnumProperty(
        name="Send",
        description="What the button hands to TextureCraft",
        items=[
            (
                'GLB',
                "glTF with modifiers applied",
                "Export the objects as a .glb next to this .blend file, with modifiers applied, and open that",
            ),
            (
                'BLEND',
                "A copy of this .blend file",
                "Save a copy of this file next to it and open the copy (TextureCraft reads the base meshes; modifiers are not applied)",
            ),
        ],
        default='GLB',
    )

    def draw(self, context):
        layout = self.layout
        layout.prop(self, "executable")
        layout.prop(self, "mode")
        executable = self.executable or find_executable()
        if executable:
            layout.label(text="Using " + executable, icon='INFO')
        else:
            layout.label(text="TextureCraft was not found: set its executable above", icon='ERROR')


class TEXTURECRAFT_OT_open(bpy.types.Operator):
    """Open the selected objects (or every visible one when nothing is selected) in TextureCraft to paint their textures"""

    bl_idname = "texturecraft.open"
    bl_label = "Open in TextureCraft"
    bl_options = {'REGISTER'}

    @classmethod
    def poll(cls, context):
        return any(o.type == 'MESH' for o in context.scene.objects)

    def execute(self, context):
        prefs = _prefs(context)
        executable = prefs.executable or find_executable()
        if not executable:
            self.report({'ERROR'}, "TextureCraft was not found: set its executable in the add-on preferences")
            return {'CANCELLED'}
        mode = prefs.mode
        if mode == 'GLB' and not hasattr(bpy.ops.export_scene, "gltf"):
            mode = 'BLEND'
        try:
            path = self._export_glb(context) if mode == 'GLB' else self._save_copy()
        except RuntimeError as e:
            self.report({'ERROR'}, str(e))
            return {'CANCELLED'}
        try:
            _launch(executable, path)
        except OSError as e:
            self.report({'ERROR'}, "TextureCraft (%s) did not start: %s" % (executable, e))
            return {'CANCELLED'}
        self.report({'INFO'}, "Opened %s in TextureCraft" % os.path.basename(path))
        return {'FINISHED'}

    @staticmethod
    def _target(suffix):
        """Where the file for TextureCraft goes: next to the .blend, or the temp folder."""
        blend = bpy.data.filepath
        if blend:
            return os.path.splitext(blend)[0] + "_texturecraft" + suffix
        return os.path.join(tempfile.gettempdir(), "texturecraft_scene" + suffix)

    def _export_glb(self, context):
        selected = [o for o in context.selected_objects if o.type == 'MESH']
        path = self._target(".glb")
        wanted = {
            "filepath": path,
            "export_format": 'GLB',
            "use_selection": bool(selected),
            "use_visible": not selected,
            "export_apply": True,
            "export_cameras": True,
            "export_yup": True,
        }
        # Only the options this Blender's exporter has.
        known = bpy.ops.export_scene.gltf.get_rna_type().properties.keys()
        kwargs = {k: v for k, v in wanted.items() if k in known}
        result = bpy.ops.export_scene.gltf(**kwargs)
        if 'FINISHED' not in result or not os.path.isfile(path):
            raise RuntimeError("the glTF export did not write %s" % path)
        return path

    def _save_copy(self):
        path = self._target(".blend")
        result = bpy.ops.wm.save_as_mainfile(filepath=path, copy=True, compress=False)
        if 'FINISHED' not in result or not os.path.isfile(path):
            raise RuntimeError("could not save a copy to %s" % path)
        return path


class VIEW3D_PT_texturecraft(bpy.types.Panel):
    bl_label = "TextureCraft"
    bl_space_type = 'VIEW_3D'
    bl_region_type = 'UI'
    bl_category = "TextureCraft"

    def draw(self, context):
        prefs = _prefs(context)
        column = self.layout.column()
        column.prop(prefs, "mode", text="")
        selected = len([o for o in context.selected_objects if o.type == 'MESH'])
        label = "Open %d selected in TextureCraft" % selected if selected else "Open scene in TextureCraft"
        column.operator(TEXTURECRAFT_OT_open.bl_idname, text=label, icon='BRUSH_DATA')


def _object_menu(self, context):
    self.layout.separator()
    self.layout.operator(TEXTURECRAFT_OT_open.bl_idname, icon='BRUSH_DATA')


CLASSES = (TextureCraftPreferences, TEXTURECRAFT_OT_open, VIEW3D_PT_texturecraft)


def register():
    for cls in CLASSES:
        bpy.utils.register_class(cls)
    bpy.types.VIEW3D_MT_object.append(_object_menu)


def unregister():
    bpy.types.VIEW3D_MT_object.remove(_object_menu)
    for cls in reversed(CLASSES):
        bpy.utils.unregister_class(cls)


if __name__ == "__main__":
    register()
