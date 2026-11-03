# TextureCraft add-on for Blender

`texturecraft/` is a Blender add-on (Blender 2.80 and newer; the manifest also makes it a
Blender 4.2+ extension) with one job: a **TextureCraft** tab in the 3D viewport's sidebar and an
**Open in TextureCraft** entry in the Object menu that hand the selected objects (or the whole
scene) to TextureCraft, either as a glTF with modifiers applied or as a copy of the `.blend` file.

TextureCraft installs it itself: **Edit ▸ Install Blender add-on…** (the command
`blender.installAddon`), shown when a Blender is installed, copies the folder into Blender's user
add-ons folder for that Blender's version, records TextureCraft's executable in
`texturecraft_path.json` beside it, and enables it through a Blender run in the background. To
install by hand, zip the `texturecraft` folder and use Blender's *Install from Disk*, then set the
TextureCraft executable in the add-on's preferences.

The engine embeds these files at build time (`crates/engine/src/blender_cmds.rs`); the add-on is
MIT OR Apache-2.0 like the rest of TextureCraft.
