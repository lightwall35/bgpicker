import Quickshell

ShellRoot {
    WallpaperBackend { id: wallpaperBackend }
    WallpaperBackend { ipcTarget: "backdropBackend"; layerNamespace: "bgpicker:backdrop" }
    BgPicker { backend: wallpaperBackend }
}
