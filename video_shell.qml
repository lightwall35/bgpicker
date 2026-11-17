import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import "WallpaperMath.js" as WallpaperMath
import "WallpaperTransitions.js" as Transitions

ShellRoot {
    id: root
    readonly property string videoPath: Quickshell.env("BGPICKER_VIDEO") || ""
    readonly property string blurPath: Quickshell.env("BGPICKER_BLUR") || ""
    readonly property string readyPath: Quickshell.env("BGPICKER_QS_READY") || ""
    property var screenViews: []
    property var wallpaperSettings: WallpaperMath.settings({})
    property string playbackMode: Quickshell.env("BGPICKER_MODE") === "normal" ? "normal" : "parallax"
    property string stagedMode: playbackMode
    property string currentPath: ""
    property string currentBlur: ""
    property string stagedPath: videoPath
    property string stagedBlur: blurPath
    property string stagedStaticPath: ""
    property string stagedStaticBlur: ""
    property bool initializing: true
    property bool startupMarkerWritten: false
    readonly property bool allReady: currentPath !== "" && screenViews.length > 0
        && screenViews.length === Quickshell.screens.length
        && screenViews.every(v => v.frontReady && v.backReady)
    readonly property bool transitionActive: screenViews.some(v => v.transitioning)

    function registerScreen(view) {
        screenViews = screenViews.concat([view]);
        if (stagedPath !== "") view.stage(stagedPath, stagedBlur);
        else if (currentPath !== "") {
            view.stage(currentPath, currentBlur);
            view.autoCommit = true;
        }
    }
    function unregisterScreen(view) { screenViews = screenViews.filter(v => v !== view); }
    function stage(path, blur, mode) {
        if (!path.startsWith("/") || !blur.startsWith("/")) return "error: absolute media paths required";
        if (mode !== "normal" && mode !== "parallax") return "error: unknown video mode";
        stagedPath = path; stagedBlur = blur; stagedMode = mode;
        for (const view of screenViews) view.stage(path, blur);
        return "loading";
    }
    function ready(path) {
        if (path !== stagedPath) return "error: superseded request";
        if (!niri.connected || screenViews.length === 0 || screenViews.length !== Quickshell.screens.length) return "loading";
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
        return "ready";
    }
    function cancel(path) {
        for (const view of screenViews) view.cancel(path);
        if (stagedPath === path) { stagedPath = ""; stagedBlur = ""; }
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
        if (!niri.connected || screenViews.length === 0 || screenViews.length !== Quickshell.screens.length) return "loading";
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
    NiriState { id: niri; subscribed: true }
    FileView {
        path: (Quickshell.env("XDG_CONFIG_HOME") || Quickshell.env("HOME") + "/.config") + "/bgpicker/settings.json"
        preload: true; watchChanges: true; printErrors: false
        onFileChanged: reload()
        onLoaded: {
            try { root.wallpaperSettings = WallpaperMath.settings(JSON.parse(text())); }
            catch (error) { console.warn("BgPicker video: invalid settings:", error); }
        }
    }
    Process { id: readyWriter; command: ["touch", root.readyPath] }
    Timer {
        interval: 50; repeat: true
        running: root.initializing || (!root.startupMarkerWritten && root.allReady)
        onTriggered: {
            if (root.initializing && root.ready(root.stagedPath) === "ready") {
                let snapshot = {type: "none", durationMs: 0};
                try {
                    const configured = Quickshell.env("BGPICKER_INITIAL_TRANSITION") || "";
                    if (configured !== "") snapshot = JSON.parse(configured);
                } catch (error) { console.warn("Invalid initial video transition:", error); }
                root.commit(root.stagedPath, JSON.stringify(snapshot));
                root.initializing = false;
            }
            if (!root.initializing && root.allReady && !root.transitionActive && !root.startupMarkerWritten) {
                root.startupMarkerWritten = true;
                if (root.readyPath !== "") readyWriter.running = true;
            }
        }
    }
    IpcHandler {
        target: "videoParallax"
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
        function status(): string {
            return JSON.stringify({apiVersion: 2, ready: root.allReady, niriConnected: niri.connected,
                currentPath: root.currentPath, stagedPath: root.stagedPath, mode: root.playbackMode,
                transitionActive: root.transitionActive, outputs: root.screenViews.map(v => v.describe())});
        }
    }
    Variants {
        model: Quickshell.screens
        Scope {
            id: display
            required property var modelData
            readonly property real progress: WallpaperMath.progress(niri.snapshot, modelData.name, root.wallpaperSettings.columnSpan)
            property real animatedProgress: progress
            property real modeBlend: root.playbackMode === "parallax" ? 1 : 0
            property bool autoCommit: false
            readonly property bool frontReady: frontLayer.presented
            readonly property bool backReady: backLayer.presented
            readonly property bool transitioning: frontLayer.transitionActive || backLayer.transitionActive || modeAnimation.running
            Behavior on animatedProgress {
                NumberAnimation { duration: root.wallpaperSettings.durationMs; easing.type: Easing.OutCubic }
            }
            Behavior on modeBlend {
                NumberAnimation { id: modeAnimation; duration: root.wallpaperSettings.modeTransitionMs; easing.type: Easing.InOutCubic }
            }
            function stage(path, blur) { frontLayer.stage(path); backLayer.stage(blur); }
            function ready(path) {
                const front = frontLayer.ready(path);
                return front === "ready" ? backLayer.ready(root.stagedBlur || root.currentBlur) : front;
            }
            function commit(path, blur, snapshot) {
                backLayer.incoming().seek(frontLayer.incoming().position);
                frontLayer.commit(path, snapshot);
                backLayer.commit(blur, snapshot);
            }
            function cancel(path) { frontLayer.cancel(path); backLayer.cancel(root.stagedBlur); }
            function stageStatic(path, blur, frontBlend, backBlend) {
                frontLayer.stageStatic(path, frontBlend); backLayer.stageStatic(blur, backBlend);
            }
            function readyStatic(path) {
                const front = frontLayer.readyStatic(path);
                return front === "ready" ? backLayer.readyStatic(root.stagedStaticBlur) : front;
            }
            function commitStatic(path, blur, snapshot) {
                frontLayer.commitStatic(path, snapshot); backLayer.commitStatic(blur, snapshot);
            }
            function cancelStatic(path) { frontLayer.cancelStatic(path); backLayer.cancelStatic(root.stagedStaticBlur); }
            function describe() {
                const front = frontLayer.describe(), back = backLayer.describe();
                return {output: modelData.name, progress: progress, animatedProgress: animatedProgress,
                    frontReady: frontReady, backReady: backReady, transitionActive: transitioning,
                    frontPosition: front.position, backPosition: back.position,
                    frontPlaying: front.playing, backPlaying: back.playing,
                    frontFrames: front.frames, backFrames: back.frames, frontX: front.x,
                    frontWidth: front.width, front: front, back: back};
            }
            Component.onCompleted: root.registerScreen(display)
            Component.onDestruction: root.unregisterScreen(display)
            PanelWindow {
                screen: display.modelData
                visible: backLayer.presented && frontLayer.presented
                color: "transparent"
                WlrLayershell.namespace: "bgpicker:backdrop"
                WlrLayershell.layer: WlrLayer.Background
                WlrLayershell.exclusionMode: ExclusionMode.Ignore
                WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
                anchors { top: true; bottom: true; left: true; right: true }
                mask: Region {}
                VideoTransitionLayer {
                    id: backLayer
                    anchors.fill: parent
                    settings: root.wallpaperSettings
                    motionProgress: display.animatedProgress
                    modeBlend: display.modeBlend
                    seedBlend: Quickshell.env("BGPICKER_STATIC_BACKDROP_PARALLAX") === "on" ? 1 : 0
                    seedPath: Quickshell.env("BGPICKER_STATIC_BLUR") || ""
                }
            }
            PanelWindow {
                screen: display.modelData
                visible: frontLayer.presented
                color: "transparent"
                WlrLayershell.namespace: "bgpicker:video"
                WlrLayershell.layer: WlrLayer.Background
                WlrLayershell.exclusionMode: ExclusionMode.Ignore
                WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
                anchors { top: true; bottom: true; left: true; right: true }
                mask: Region {}
                VideoTransitionLayer {
                    id: frontLayer
                    anchors.fill: parent
                    settings: root.wallpaperSettings
                    motionProgress: display.animatedProgress
                    modeBlend: display.modeBlend
                    seedBlend: Quickshell.env("BGPICKER_STATIC_MODE") === "parallax" ? 1 : 0
                    seedPath: Quickshell.env("BGPICKER_STATIC_FRONT") || ""
                }
            }
            Timer {
                interval: 500; repeat: true
                running: display.autoCommit
                onTriggered: {
                    if (display.ready(root.currentPath) === "ready") {
                        display.commit(root.currentPath, root.currentBlur, {type: "none", durationMs: 0});
                        display.autoCommit = false;
                    }
                }
            }
            Timer {
                interval: 2000; repeat: true
                running: display.frontReady && display.backReady && !display.transitioning && root.stagedPath === ""
                onTriggered: backLayer.syncWith(frontLayer)
            }
        }
    }
}
