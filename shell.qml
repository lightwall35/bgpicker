import Quickshell

ShellRoot {
    NiriState {
        id: niriService
        subscribed: wallpaperBackend.resourcesEnabled || backdropBackend.resourcesEnabled
    }
    WallpaperBackend { id: wallpaperBackend; sharedNiri: niriService }
    WallpaperBackend { id: backdropBackend; ipcTarget: "backdropBackend"; layerNamespace: "bgpicker:backdrop"; sharedNiri: niriService }
    BgPicker { backend: wallpaperBackend }
}
