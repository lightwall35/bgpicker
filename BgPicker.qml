import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

// ==============================================================================
// BgPicker — Carousel wrapper for awww + swaybg dual-backend wallpaper picking
//
// This is a thin adapter that connects the shell-agnostic Carousel.qml
// to the awww/swaybg wallpaper backends via switch_wallpaper.sh.
// ==============================================================================

Item {
    id: root

    // ── Configuration ─────────────────────────────────────────────────────────
    // The directory to browse wallpapers from (displayed in the carousel).
    // This should be your swaybg wallpaper directory.
    readonly property string dirSwaybg: "/home/lightwall/swaybg_wallpapers"

    // The awww wallpaper directory. Files are matched by filename.
    // When you select "A.png" from swaybg dir, "A.png" from awww dir is used.
    readonly property string dirAwww: "/home/lightwall/awww_wallpapers"

    // Cache file that stores the current swaybg image path
    readonly property string swaybgCacheFile: Quickshell.env("HOME") + "/.cache/current_swaybg_img"

    // Resolved path to the directory containing this QML file (and the scripts)
    readonly property string installDir: {
        let url = Qt.resolvedUrl(".").toString();
        // Strip trailing slash and file:// prefix
        if (url.endsWith("/")) url = url.slice(0, -1);
        if (url.startsWith("file://")) url = url.slice(7);
        return url;
    }

    // ── State ─────────────────────────────────────────────────────────────────
    property string currentWallpaper: ""

    // ── Read current wallpaper on startup ─────────────────────────────────────
    Process {
        id: readCacheProcess
        command: ["cat", root.swaybgCacheFile]
        running: true
        stdout: SplitParser {
            onRead: data => {
                const trimmed = data.trim();
                if (trimmed.length > 0) {
                    root.currentWallpaper = trimmed;
                }
            }
        }
    }

    // ── The Carousel (copied verbatim from wallpaperCarousel) ─────────────────
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
        defaultWallpaperFolder: root.dirAwww
        currentWallpaperPath: root.currentWallpaper
        hasWallpaperConfigured: true
        shellSettingsHint: "Place your wallpapers in:\n" + root.dirSwaybg + "\n" + root.dirAwww

        // Screen detection — use first available screen
        // (for multi-monitor, this could be enhanced to detect the focused screen)
        getFocusedScreen: () => Quickshell.screens.length > 0 ? Quickshell.screens[0] : null

        // ── The magic happens here ────────────────────────────────────────────
        // When the user picks a wallpaper, switch both awww and swaybg
        onWallpaperPicked: (fullPath, screenName) => {
            // Update local state immediately
            root.currentWallpaper = fullPath;

            // fullPath is from awww dir (original images)
            // Derive the matching swaybg (blurred) path from the filename
            const fileName = fullPath.split('/').pop();
            const swaybgPath = root.dirSwaybg + "/" + fileName;

            console.log("BgPicker: Switching wallpapers");
            console.log("  awww   →", fullPath);
            console.log("  swaybg →", swaybgPath);

            // Execute the switch script (arg1=swaybg, arg2=awww)
            // Kill any previous switch process before starting a new one
            switchProcess.running = false;
            switchProcess.command = [
                root.installDir + "/switch_wallpaper.sh",
                swaybgPath,
                fullPath
            ];
            switchProcess.running = true;
        }
    }

    // ── Wallpaper switch process ──────────────────────────────────────────────
    Process {
        id: switchProcess
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0) {
                console.warn("BgPicker: switch_wallpaper.sh exited with code", exitCode);
            }
        }
    }

    Component.onCompleted: {
        console.info("BgPicker: daemon loaded");
        console.info("  swaybg dir: " + root.dirSwaybg);
        console.info("  awww dir:   " + root.dirAwww);
        console.info("  Toggle with: qs ipc -p " + root.installDir + " call wallpaperCarousel toggle");
    }
}
