import QtQuick
import Quickshell
import Quickshell.Io
import "WallpaperMath.js" as WallpaperMath

Item {
    id: root
    property var sharedNiri: null
    readonly property var activeNiri: root.sharedNiri ? root.sharedNiri : localNiri
    readonly property bool niriConnected: activeNiri.connected
    readonly property var niriState: activeNiri.snapshot

    property bool resourcesEnabled: false
    property var screenViews: []
    property var wallpaperSettings: WallpaperMath.settings({})
    property string playbackMode: "parallax"
    property string stagedMode: playbackMode
    property string currentPath: ""
    property string currentBlur: ""
    property string stagedPath: ""
    property string stagedBlur: ""
    property string stagedStaticPath: ""
    property string stagedStaticBlur: ""
    property string initialStaticFront: ""
    property string initialStaticBlur: ""
    property real initialStaticFrontBlend: 0
    property real initialStaticBackBlend: 0

    readonly property bool allReady: currentPath !== "" && screenViews.length > 0
        && screenViews.length === Quickshell.screens.length
        && screenViews.every(v => v.frontReady && v.backReady)
    readonly property bool transitionActive: screenViews.some(v => v.transitioning)
    readonly property string settingsPath: (Quickshell.env("XDG_CONFIG_HOME")
        || Quickshell.env("HOME") + "/.config") + "/bgpicker/settings.json"

    function registerScreen(view) {
        screenViews = screenViews.concat([view]);
        if (stagedPath !== "") view.stage(stagedPath, stagedBlur);
        else if (currentPath !== "") {
            view.stage(currentPath, currentBlur);
            view.autoCommit = true;
        }
    }

    function unregisterScreen(view) {
        screenViews = screenViews.filter(v => v !== view);
    }

    function setStaticSeed(frontPath, blurPath, frontMode, backdropParallax) {
        initialStaticFront = frontPath || "";
        initialStaticBlur = blurPath || "";
        initialStaticFrontBlend = frontMode === "parallax" ? 1 : 0;
        initialStaticBackBlend = backdropParallax === "on" ? 1 : 0;
        if (resourcesEnabled && currentPath === "") {
            for (const view of screenViews) {
                view.setSeed(initialStaticFront, initialStaticBlur, initialStaticFrontBlend, initialStaticBackBlend);
            }
        }
        return "seeded";
    }

    function clearStaticSeed() {
        initialStaticFront = "";
        initialStaticBlur = "";
        initialStaticFrontBlend = 0;
        initialStaticBackBlend = 0;
        return "cleared";
    }

    function stage(path, blur, mode) {
        if (!path.startsWith("/") || !blur.startsWith("/")) return "error: absolute media paths required";
        if (mode !== "normal" && mode !== "parallax") return "error: unknown video mode";
        if (activeNiri.socketPath === "") return "error: NIRI_SOCKET is missing";
        stagedPath = path; stagedBlur = blur; stagedMode = mode;
        if (!resourcesEnabled) {
            resourcesEnabled = true;
        } else {
            for (const view of screenViews) view.stage(path, blur);
        }
        return "loading";
    }

    function ready(path) {
        if (path !== stagedPath) return "error: superseded request";
        if (!niriConnected || screenViews.length === 0 || screenViews.length !== Quickshell.screens.length) return "loading";
        for (const view of screenViews) {
            const result = view.ready(path);
            if (result !== "ready") return result;
        }
        return "ready";
    }

    function commit(path, json) {
        const result = ready(path);
        if (result !== "ready") return result;
        let snapshot;
        try { snapshot = JSON.parse(json); }
        catch (error) { return "error: invalid transition settings"; }
        if (snapshot.type === "random") snapshot.type = ["fade", "wipe", "wave", "grow", "outer"][Math.floor(Math.random() * 5)];
        playbackMode = stagedMode;
        for (const view of screenViews) view.commit(path, stagedBlur, snapshot);
        currentPath = path; currentBlur = stagedBlur;
        stagedPath = ""; stagedBlur = "";
        clearStaticSeed();
        return "ready";
    }

    function cancel(path) {
        for (const view of screenViews) view.cancel(path);
        if (stagedPath === path) { stagedPath = ""; stagedBlur = ""; }
        clearStaticSeed();
        if (currentPath === "") resourcesEnabled = false;
        return "cancelled";
    }

    function stageStatic(path, blur, frontMode, backdropParallax) {
        if (!path.startsWith("/") || !blur.startsWith("/")) return "error: absolute media paths required";
        if (frontMode !== "awww" && frontMode !== "parallax") return "error: unknown static mode";
        if (backdropParallax !== "on" && backdropParallax !== "off") return "error: unknown backdrop mode";
        stagedStaticPath = path; stagedStaticBlur = blur;
        const frontBlend = frontMode === "parallax" ? 1 : 0;
        const backBlend = backdropParallax === "on" ? 1 : 0;
        for (const view of screenViews) view.stageStatic(path, blur, frontBlend, backBlend);
        return "loading";
    }

    function readyStatic(path) {
        if (path !== stagedStaticPath) return "error: superseded request";
        if (!niriConnected || screenViews.length === 0 || screenViews.length !== Quickshell.screens.length) return "loading";
        for (const view of screenViews) {
            const result = view.readyStatic(path);
            if (result !== "ready") return result;
        }
        return "ready";
    }

    function commitStatic(path, json) {
        const result = readyStatic(path);
        if (result !== "ready") return result;
        let snapshot;
        try { snapshot = JSON.parse(json); }
        catch (error) { return "error: invalid transition settings"; }
        if (snapshot.type === "random") snapshot.type = ["fade", "wipe", "wave", "grow", "outer"][Math.floor(Math.random() * 5)];
        for (const view of screenViews) view.commitStatic(path, stagedStaticBlur, snapshot);
        stagedStaticPath = ""; stagedStaticBlur = "";
        return "ready";
    }

    function cancelStatic(path) {
        for (const view of screenViews) view.cancelStatic(path);
        if (stagedStaticPath === path) { stagedStaticPath = ""; stagedStaticBlur = ""; }
        return "cancelled";
    }

    function setMode(mode) {
        if (mode !== "normal" && mode !== "parallax") return "error: unknown video mode";
        playbackMode = mode;
        return "started";
    }

    function deactivate() {
        for (const view of screenViews) view.releaseAll();
        currentPath = "";
        currentBlur = "";
        stagedPath = "";
        stagedBlur = "";
        stagedStaticPath = "";
        stagedStaticBlur = "";
        clearStaticSeed();
        resourcesEnabled = false;
        Qt.callLater(gc);
        return "inactive";
    }

    NiriState { id: localNiri; subscribed: root.resourcesEnabled && !root.sharedNiri }

    FileView {
        id: settingsFile
        path: root.settingsPath
        preload: true; watchChanges: true; printErrors: false
        onFileChanged: reload()
        onLoaded: {
            try { root.wallpaperSettings = WallpaperMath.settings(JSON.parse(text())); }
            catch (error) { console.warn("BgPicker video: invalid settings:", error); }
        }
    }

    Loader {
        active: root.resourcesEnabled
        sourceComponent: VideoWallpaper { backend: root }
    }

    IpcHandler {
        target: "videoParallax"
        function setStaticSeed(frontPath: string, blurPath: string, frontMode: string, backdropParallax: string): string {
            return root.setStaticSeed(frontPath, blurPath, frontMode, backdropParallax);
        }
        function clearStaticSeed(): string { return root.clearStaticSeed(); }
        function stage(path: string, blur: string, mode: string): string { return root.stage(path, blur, mode); }
        function ready(path: string): string { return root.ready(path); }
        function commit(path: string, snapshot: string): string { return root.commit(path, snapshot); }
        function cancel(path: string): string { return root.cancel(path); }
        function stageStatic(path: string, blur: string, frontMode: string, backdropParallax: string): string {
            return root.stageStatic(path, blur, frontMode, backdropParallax);
        }
        function readyStatic(path: string): string { return root.readyStatic(path); }
        function commitStatic(path: string, snapshot: string): string { return root.commitStatic(path, snapshot); }
        function cancelStatic(path: string): string { return root.cancelStatic(path); }
        function mode(mode: string): string { return root.setMode(mode); }
        function deactivate(): string { return root.deactivate(); }
        function status(): string {
            return JSON.stringify({
                apiVersion: 2,
                active: root.resourcesEnabled,
                ready: root.allReady,
                niriConnected: root.niriConnected,
                currentPath: root.currentPath,
                stagedPath: root.stagedPath,
                mode: root.playbackMode,
                transitionActive: root.transitionActive,
                outputs: root.screenViews.map(v => v.describe())
            });
        }
    }
}
