# Sekiro Archipelago Randomizer 1.0.0-alpha.7

This package contains the static randomizer and the mod DLL for integrating _Sekiro: Shadows Die Twice_ into the [Archipelago] multiworld randomizer. You can download this from the "Assets" dropdown on [the Releases page]. If you're already reading this on the Releases page, it's just below this documentation. Setup and game documentation coming soon!

[Archipelago]: https://archipelago.gg
[the Releases page]: https://github.com/fswap/from-software-archipelago-clients/releases?q=Sekiro&expanded=true

You can also check out [the changelog] for information about the changes in the latest randomizer release.

[the changelog]: https://github.com/fswap/ds3-archipelago/blob/main/crates/sdt-archipelago/CHANGELOG.md

# Setup

Download the release and unzip the files directly into the Sekiro game directory: `SteamLibrary\steamapps\common\Sekiro`.

## Generating the Archipelago YAML

1. Launch Archipelago and install the `sekiro.apworld` file with `Install APWorld`.
2. Open `Options Creator` and choose the new `Sekiro: Shadows Die Twice` menu item. Update the settings as desired, save the YAML.
3. Generate a Multiworld using all the player YAMLs (from Step 2)
4. Host the multiworld

## Starting Sekiro

1. Once the multiworld is started, go into the `randomizer` folder and run the .exe.
2. Connect to the multiworld to randomize your items.
3. Once complete, go back to the Sekiro directory and check `modengine.ini`.
4. Make sure if you are using `modengine.ini` to point the mods towards the `randomizer` folder.

```
; If enabled, a mod will be loaded from a specified override directory.
useModOverrideDirectory=1

; The directory from which to load a mod.
modOverrideDirectory="\randomizer"
```

5. Use `launch-sekiro.bat` to start Sekiro with the Archipelago client.
