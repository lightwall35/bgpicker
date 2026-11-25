import QtQuick
import Quickshell
import Quickshell.Io
import "WallpaperTransitions.js" as Transitions

// ==============================================================================
// BgPicker — Carousel wrapper and serialized wallpaper/mode requests.
//
// Connect the shell-agnostic Carousel.qml to bgpickerctl / bgpicker-daemon.
// ==============================================================================

Item {
    id: root

    // ── Configuration ─────────────────────────────────────────────────────────
    // Resolved path to the directory containing this QML file (and the scripts)
    readonly property string installDir: {
        let url = Qt.resolvedUrl(".").toString();
        // Strip trailing slash and file:// prefix
        if (url.endsWith("/")) url = url.slice(0, -1);
        if (url.startsWith("file://")) url = url.slice(7);
        return decodeURIComponent(url);
    }

    // Originals live alongside the project; blurred/thumbnail caches are managed by the daemon.
    readonly property string wallpaperDir: installDir + "/wallpapers"
    readonly property string stateDir:
        (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state")
        + "/bgpicker"
    readonly property string currentWallpaperFile: stateDir + "/current_wallpaper"
    readonly property string settingsPath:
        (Quickshell.env("XDG_CONFIG_HOME") || Quickshell.env("HOME") + "/.config")
        + "/bgpicker/settings.json"

    // ── State ─────────────────────────────────────────────────────────────────
    property string currentWallpaper: ""
    property var pendingCommand: []
    property string wallpaperMode: "awww"
    property string currentVideo: ""
    property string videoMode: "normal"
    property string engine: "full"
    property string transitionType: "fade"
    property var imageCatalog: []
    property var videoCatalog: []
    property string lastError: ""
    readonly property bool busy: switchProcess.running || preferencesProcess.running || pendingCommand.length > 0

    function ctlCommand(args) {
        const script = 'ctl="${BGPICKERCTL:-}"; '
            + '[ -n "$ctl" ] || ctl="$(command -v bgpickerctl 2>/dev/null || true)"; '
            + '[ -n "$ctl" ] || [ ! -x "$1/target/release/bgpickerctl" ] || ctl="$1/target/release/bgpickerctl"; '
            + '[ -n "$ctl" ] || [ ! -x "$1/target/debug/bgpickerctl" ] || ctl="$1/target/debug/bgpickerctl"; '
            + '[ -n "$ctl" ] || { echo "BgPicker: bgpickerctl not found" >&2; exit 127; }; '
            + 'shift; exec "$ctl" "$@"';
        return ["sh", "-c", script, "bgpicker-ctl", root.installDir].concat(args);
    }

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
        path: root.stateDir + "/mode"
        preload: true
        printErrors: false
        watchChanges: true
        onLoaded: {
            root.wallpaperMode = text().trim() === "parallax" ? "parallax" : "awww";
            root.lastError = "";
        }
        onFileChanged: reload()
    }

    FileView {
        path: root.stateDir + "/engine"
        preload: true; watchChanges: true; printErrors: false
        onLoaded: root.engine = text().trim() || "full"
        onFileChanged: reload()
    }

    FileView {
        path: root.stateDir + "/video-wallpaper"
        preload: true; watchChanges: true; printErrors: false
        onLoaded: root.currentVideo = text().replace(/\n$/, "")
        onFileChanged: reload()
    }

    FileView {
        path: root.stateDir + "/video-mode"
        preload: true; watchChanges: true; printErrors: false
        onLoaded: root.videoMode = text().trim() === "parallax" ? "parallax" : "normal"
        onFileChanged: reload()
    }

    FileView {
        id: settingsFile
        path: root.settingsPath
        preload: true; watchChanges: true; printErrors: false
        onLoaded: {
            try {
                const config = JSON.parse(text());
                if (config && typeof config === "object" && !Array.isArray(config)) {
                    root.transitionType = Transitions.settings(config).type;
                }
            } catch (error) {
                console.warn("BgPicker: keeping previous settings; invalid JSON:", error);
            }
        }
        onFileChanged: reload()
    }

    Process {
        id: catalogProcess
        command: root.ctlCommand(["catalog", "--images", root.wallpaperDir, "--videos", root.installDir + "/dynamic"])
        stdout: StdioCollector { id: catalogOutput }
        onExited: (code, status) => {
            if (code === 0) {
                try {
                    const parsed = JSON.parse(catalogOutput.text);
                    root.imageCatalog = parsed.images || [];
                    root.videoCatalog = parsed.videos || [];
                } catch (error) { root.lastError = "媒体目录读取失败。"; }
            }
        }
    }

    function processNextWallpaper() {
        if (switchProcess.running || preferencesProcess.running || pendingCommand.length === 0) return;
        console.info("BgPicker: applying", JSON.stringify(pendingCommand));
        switchProcess.command = pendingCommand;
        pendingCommand = [];
        lastError = "";
        switchProcess.running = true;
    }

    function toggleMode() {
        if (busy) return;
        if (carousel.videoPage && currentVideo !== "") {
            pendingCommand = root.ctlCommand([
                "apply-video",
                videoMode === "normal" ? "parallax" : "normal",
                currentVideo
            ]);
        } else if (!carousel.videoPage && currentWallpaper !== "") {
            if (root.engine !== "full") {
                pendingCommand = root.ctlCommand([
                    "apply-static",
                    currentWallpaper,
                    "--mode",
                    wallpaperMode === "parallax" ? "awww" : "parallax"
                ]);
            } else {
                pendingCommand = root.ctlCommand(["mode", "toggle"]);
            }
        }
        processNextWallpaper();
    }

    function selectTransition(type) {
        if (busy || type === root.transitionType) return;
        lastError = "";
        preferencesProcess.pendingType = type;
        preferencesProcess.command = root.ctlCommand(["transition", type]);
        preferencesProcess.running = true;
    }

    IpcHandler {
        target: "wallpaperPicker"
        function motion(): string { if (root.busy) return "busy"; root.toggleMode(); return "started"; }
        function transition(type: string): string { if (root.busy) return "busy"; root.selectTransition(type); return "saved"; }
        function status(): string {
            return JSON.stringify({
                busy: root.busy,
                engine: root.engine,
                page: carousel.videoPage ? "video" : "static",
                mode: carousel.wallpaperMode,
                transition: root.transitionType,
                error: root.lastError
            });
        }
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
        currentVideoPath: root.currentVideo
        videoFolder: root.installDir + "/dynamic"
        imageCatalog: root.imageCatalog
        useImageCatalog: true
        videoCatalog: root.videoCatalog
        onOpened: if ((root.imageCatalog.length === 0 || root.videoCatalog.length === 0) && !catalogProcess.running) catalogProcess.running = true
        onCatalogRefreshRequested: if (!catalogProcess.running) catalogProcess.running = true
        hasWallpaperConfigured: true
        shellSettingsHint: "Place your wallpapers in:\n" + root.wallpaperDir
        wallpaperMode: carousel.videoPage ? (root.videoMode === "parallax" ? "parallax" : "awww") : root.wallpaperMode
        wallpaperModeBusy: root.busy
        wallpaperModeAvailable: carousel.videoPage ? root.currentVideo !== "" : root.currentWallpaper !== ""
        wallpaperTransition: root.transitionType
        wallpaperTransitionChoices: Transitions.choices
        wallpaperError: root.lastError
        onWallpaperModeToggleRequested: root.toggleMode()
        onWallpaperTransitionSelected: type => root.selectTransition(type)

        // Screen detection — use first available screen
        // (for multi-monitor, this could be enhanced to detect the focused screen)
        getFocusedScreen: () => Quickshell.screens.length > 0 ? Quickshell.screens[0] : null

        onWallpaperPicked: (fullPath, screenName) => {
            root.pendingCommand = carousel.videoPage
                ? root.ctlCommand(["apply-video", root.videoMode, fullPath])
                : root.ctlCommand(["apply-static", fullPath]);
            root.processNextWallpaper();
        }
    }

    // ── Wallpaper switch process ──────────────────────────────────────────────
    Process {
        id: switchProcess
        stderr: StdioCollector { id: switchErrors }
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0 || exitStatus !== 0) {
                root.lastError = switchErrors.text.trim() || "壁纸切换失败，请查看后台服务日志。";
                console.warn("BgPicker: bgpickerctl exited with code", exitCode);
            }
            Qt.callLater(root.processNextWallpaper);
        }
    }

    Process {
        id: preferencesProcess
        property string pendingType: ""
        stderr: StdioCollector { id: preferencesErrors }
        onExited: (exitCode, exitStatus) => {
            if (exitCode === 0 && exitStatus === 0) {
                if (pendingType !== "") root.transitionType = pendingType;
                settingsFile.reload();
            } else {
                root.lastError = preferencesErrors.text.trim() || "切换效果保存失败。";
            }
            pendingType = "";
            Qt.callLater(root.processNextWallpaper);
        }
    }

    Component.onCompleted: {
        catalogProcess.running = true;
        console.info("BgPicker: picker loaded");
        console.info("  wallpaper dir: " + root.wallpaperDir);
        console.info("  Toggle with: qs ipc -p " + root.installDir + " call wallpaperCarousel toggle");
    }
}
