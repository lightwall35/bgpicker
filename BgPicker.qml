import QtQuick
import Quickshell
import Quickshell.Io
import "WallpaperTransitions.js" as Transitions

// ==============================================================================
// BgPicker — Carousel wrapper and serialized wallpaper/mode requests.
//
// Connect the shell-agnostic Carousel.qml to the mode-aware switcher.
// ==============================================================================

Item {
    id: root
    required property var backend

    // ── Configuration ─────────────────────────────────────────────────────────
    // Resolved path to the directory containing this QML file (and the scripts)
    readonly property string installDir: {
        let url = Qt.resolvedUrl(".").toString();
        // Strip trailing slash and file:// prefix
        if (url.endsWith("/")) url = url.slice(0, -1);
        if (url.startsWith("file://")) url = url.slice(7);
        return decodeURIComponent(url);
    }

    // Originals live alongside the project; blurred images are generated on demand.
    readonly property string wallpaperDir: installDir + "/wallpapers"
    readonly property string currentWallpaperFile:
        (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state")
        + "/bgpicker/current_wallpaper"

    // ── State ─────────────────────────────────────────────────────────────────
    property string currentWallpaper: ""
    property var pendingCommand: []
    property string wallpaperMode: "awww"
    property string lastError: ""
    readonly property bool busy: switchProcess.running || preferencesProcess.running || pendingCommand.length > 0

    // Also observe switches made through the command-line helper.
    FileView {
        path: root.currentWallpaperFile
        preload: true
        printErrors: false
        watchChanges: true
        onLoaded: {
            root.currentWallpaper = text().replace(/\n$/, "");
            root.lastError = "";
        }
        onFileChanged: reload()
    }

    FileView {
        path: root.currentWallpaperFile.slice(0, -"current_wallpaper".length) + "mode"
        preload: true
        printErrors: false
        watchChanges: true
        onLoaded: {
            root.wallpaperMode = text().trim() === "parallax" ? "parallax" : "awww";
            root.lastError = "";
        }
        onFileChanged: reload()
    }

    function processNextWallpaper() {
        if (switchProcess.running || preferencesProcess.running || pendingCommand.length === 0) return;
        switchProcess.command = [root.installDir + "/switch_wallpaper.sh"].concat(pendingCommand);
        pendingCommand = [];
        lastError = "";
        switchProcess.running = true;
    }

    function toggleMode() {
        if (busy || currentWallpaper === "") return;
        pendingCommand = ["--mode", "toggle"];
        processNextWallpaper();
    }

    function selectTransition(type) {
        if (busy || type === backend.transition.type) return;
        lastError = "";
        preferencesProcess.command = ["python3", installDir + "/wallpaper_settings.py", "set-transition", type];
        preferencesProcess.running = true;
    }

    // ── The Carousel ──────────────────────────────────────────────────────────
    Carousel {
        id: carousel
        anchors.fill: parent

        wlrNamespace: "bgpicker:overlay"

        // Visual configuration — tweak these to your liking
        cfg: ({
            carouselMode: "wrap",
            overlayOpacity: 80,
            cornerRadius: 12,
            itemWidth: 300,
            itemHeight: 420,
            borderWidth: 0,
            selectedScale: 108,
            expandSelected: "false",
            expandMultiplier: 120,
            enableHoldExpand: "true",
            holdExpandRatio: 35,
            holdDelay: 1500
        })

        // Shell integration properties
        defaultWallpaperFolder: root.wallpaperDir
        currentWallpaperPath: root.currentWallpaper
        hasWallpaperConfigured: true
        shellSettingsHint: "Place your wallpapers in:\n" + root.wallpaperDir
        wallpaperMode: root.wallpaperMode
        wallpaperModeBusy: root.busy
        wallpaperModeAvailable: root.currentWallpaper !== ""
        wallpaperTransition: root.backend.transition.type
        wallpaperTransitionChoices: Transitions.choices
        wallpaperError: root.lastError
        onWallpaperModeToggleRequested: root.toggleMode()
        onWallpaperTransitionSelected: type => root.selectTransition(type)

        // Screen detection — use first available screen
        // (for multi-monitor, this could be enhanced to detect the focused screen)
        getFocusedScreen: () => Quickshell.screens.length > 0 ? Quickshell.screens[0] : null

        // The script resolves the blurred variant; the UI only passes the original.
        onWallpaperPicked: (fullPath, screenName) => {
            root.pendingCommand = [fullPath];
            root.processNextWallpaper();
        }
    }

    // ── Wallpaper switch process ──────────────────────────────────────────────
    Process {
        id: switchProcess
        stderr: StdioCollector { id: switchErrors }
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0 || exitStatus !== 0) {
                root.lastError = switchErrors.text.trim() || "壁纸切换失败，请查看 Quickshell 日志。";
                console.warn("BgPicker: switch_wallpaper.sh exited with code", exitCode);
            }
            Qt.callLater(root.processNextWallpaper);
        }
    }

    Process {
        id: preferencesProcess
        stderr: StdioCollector { id: preferencesErrors }
        onExited: (exitCode, exitStatus) => {
            if (exitCode === 0 && exitStatus === 0) {
                root.backend.reloadSettings();
            } else {
                root.lastError = preferencesErrors.text.trim() || "切换效果保存失败。";
            }
            Qt.callLater(root.processNextWallpaper);
        }
    }

    Timer {
        interval: 750
        running: true
        onTriggered: {
            if (!root.busy) {
                root.pendingCommand = ["--restore"];
                root.processNextWallpaper();
            }
        }
    }

    Component.onCompleted: {
        console.info("BgPicker: daemon loaded");
        console.info("  wallpaper dir: " + root.wallpaperDir);
        console.info("  Toggle with: qs ipc -p " + root.installDir + " call wallpaperCarousel toggle");
    }
}
