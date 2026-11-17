import QtQuick
import Quickshell
import Quickshell.Io
import "WallpaperMath.js" as WallpaperMath
import "WallpaperTransitions.js" as Transitions

Item {
    id: root
    property string ipcTarget: "wallpaperBackend"
    property string layerNamespace: "bgpicker:wallpaper"
    property bool resourcesEnabled: false
    property bool presented: false
    property string stagedPath: ""
    property string currentPath: ""
    property var renderers: []
    property var settings: WallpaperMath.settings({})
    property var transition: Transitions.settings({})
    property var lastTransition: ({})
    property var sharedNiri: null
    readonly property var activeNiri: root.sharedNiri ? root.sharedNiri : localNiri
    readonly property bool niriConnected: activeNiri.connected
    readonly property var niriState: activeNiri.snapshot
    readonly property string settingsPath: (Quickshell.env("XDG_CONFIG_HOME")
        || Quickshell.env("HOME") + "/.config") + "/bgpicker/settings.json"

    function registerRenderer(view) {
        renderers = renderers.concat([view]);
        if (stagedPath !== "") view.stage(stagedPath);
        else if (currentPath !== "") {
            // A newly plugged-in screen joins the current wallpaper after decode.
            view.stage(currentPath);
            view.autoCommit = true;
        }
    }

    function unregisterRenderer(view) {
        renderers = renderers.filter(v => v !== view);
    }

    function stage(path) {
        if (activeNiri.socketPath === "") return "error: NIRI_SOCKET is missing";
        if (!path.startsWith("/")) return "error: an absolute image path is required";
        stagedPath = path;
        resourcesEnabled = true;
        for (const view of renderers) view.stage(path);
        return "loading";
    }

    function ready(path) {
        if (path !== stagedPath) return "error: superseded request";
        if (!niriConnected || renderers.length === 0 || renderers.length !== Quickshell.screens.length)
            return "loading";
        for (const view of renderers) {
            const result = view.ready(path);
            if (result !== "ready") return result;
        }
        return "ready";
    }

    function commit(path, transitionJson) {
        const result = ready(path);
        if (result !== "ready") return result;
        let snapshot;
        try {
            snapshot = JSON.parse(transitionJson);
        } catch (error) {
            return "error: invalid transition snapshot";
        }
        for (const view of renderers) {
            view.commit(path, snapshot);
            if (snapshot.modeSwitch === "enter") view.animateMode("enter");
        }
        lastTransition = snapshot;
        currentPath = path;
        stagedPath = "";
        presented = true;
        return "ready";
    }

    function cancel(path) {
        if (path !== stagedPath) return;
        for (const view of renderers) view.cancel();
        stagedPath = "";
        if (!presented) resourcesEnabled = false;
    }

    function deactivate() {
        presented = false;
        currentPath = "";
        stagedPath = "";
        resourcesEnabled = false;
    }

    function exitMode() {
        if (!presented || renderers.length === 0) return "error: renderer is not active";
        for (const view of renderers) view.animateMode("exit");
        return "started";
    }

    function modeStatus() {
        if (!presented || renderers.length !== Quickshell.screens.length || renderers.length === 0)
            return "error: display changed during mode transition";
        for (const view of renderers) if (view.modeTransitionRunning) return "loading";
        return "ready";
    }

    function cancelMode() { for (const view of renderers) view.cancelModeTransition(); }

    function reloadSettings() { preferencesFile.reload(); }

    NiriState { id: localNiri; subscribed: root.resourcesEnabled && !root.sharedNiri }

    FileView {
        id: preferencesFile
        path: root.settingsPath
        preload: true
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: {
            try {
                const config = JSON.parse(text());
                if (!config || typeof config !== "object" || Array.isArray(config))
                    throw new Error("settings.json must contain an object");
                root.settings = WallpaperMath.settings(config);
                root.transition = Transitions.settings(config);
            } catch (error) {
                console.warn("BgPicker: keeping previous settings; invalid JSON:", error);
            }
        }
    }

    Loader {
        active: root.resourcesEnabled
        sourceComponent: ParallaxWallpaper { backend: root }
    }

    IpcHandler {
        target: root.ipcTarget
        function stage(path: string): string { return root.stage(path); }
        function ready(path: string): string { return root.ready(path); }
        function commit(path: string, transitionJson: string): string { return root.commit(path, transitionJson); }
        function cancel(path: string): string { root.cancel(path); return "cancelled"; }
        function deactivate(): string { root.deactivate(); return "awww"; }
        function exitMode(): string { return root.exitMode(); }
        function modeStatus(): string { return root.modeStatus(); }
        function cancelMode(): string { root.cancelMode(); return "cancelled"; }
        function reloadSettings(): string { root.reloadSettings(); return "reloading"; }
        function status(): string {
            return JSON.stringify({active: root.presented, wallpaper: root.currentPath,
                niriConnected: root.niriConnected, screens: root.renderers.length,
                progress: root.renderers.map(v => v.describe()),
                settings: root.settings, transition: root.transition, lastTransition: root.lastTransition});
        }
    }
}
