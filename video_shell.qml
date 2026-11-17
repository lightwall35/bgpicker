import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
    id: root
    readonly property string videoPath: Quickshell.env("BGPICKER_VIDEO") || ""
    readonly property string blurPath: Quickshell.env("BGPICKER_BLUR") || ""
    readonly property string readyPath: Quickshell.env("BGPICKER_QS_READY") || ""
    readonly property string initialMode: Quickshell.env("BGPICKER_MODE") === "normal" ? "normal" : "parallax"
    property bool initializing: true
    property bool startupMarkerWritten: false

    VideoBackend {
        id: videoService
        playbackMode: root.initialMode
        stagedMode: root.initialMode
    }

    Process { id: readyWriter; command: ["touch", root.readyPath] }

    Timer {
        interval: 50; repeat: true
        running: root.initializing || (!root.startupMarkerWritten && videoService.allReady)
        onTriggered: {
            if (root.initializing) {
                if (videoService.stagedPath === "" && root.videoPath !== "") {
                    videoService.setStaticSeed(
                        Quickshell.env("BGPICKER_STATIC_FRONT") || "",
                        Quickshell.env("BGPICKER_STATIC_BLUR") || "",
                        Quickshell.env("BGPICKER_STATIC_MODE") || "awww",
                        Quickshell.env("BGPICKER_STATIC_BACKDROP_PARALLAX") || "off"
                    );
                    videoService.stage(root.videoPath, root.blurPath, root.initialMode);
                }
                if (videoService.stagedPath !== "" && videoService.ready(videoService.stagedPath) === "ready") {
                    let snapshot = {type: "none", durationMs: 0};
                    try {
                        const configured = Quickshell.env("BGPICKER_INITIAL_TRANSITION") || "";
                        if (configured !== "") snapshot = JSON.parse(configured);
                    } catch (error) { console.warn("Invalid initial video transition:", error); }
                    videoService.commit(videoService.stagedPath, JSON.stringify(snapshot));
                    root.initializing = false;
                }
            }
            if (!root.initializing && videoService.allReady && !videoService.transitionActive && !root.startupMarkerWritten) {
                root.startupMarkerWritten = true;
                if (root.readyPath !== "") readyWriter.running = true;
            }
        }
    }
}
