import QtQuick
import Quickshell
import Quickshell.Wayland
import "WallpaperMath.js" as WallpaperMath

Item {
    id: root
    required property var backend

    Variants {
        model: Quickshell.screens

        Scope {
            id: display
            required property var modelData
            readonly property real progress: WallpaperMath.progress(root.backend.niriState, modelData.name, root.backend.wallpaperSettings.columnSpan)
            property real animatedProgress: progress
            property real modeBlend: root.backend.playbackMode === "parallax" ? 1 : 0
            property bool autoCommit: false
            readonly property bool frontReady: frontLayer.presented
            readonly property bool backReady: backLayer.presented
            readonly property bool transitioning: frontLayer.transitionActive || backLayer.transitionActive || modeAnimation.running

            Behavior on animatedProgress {
                NumberAnimation { duration: root.backend.wallpaperSettings.durationMs; easing.type: Easing.OutCubic }
            }
            Behavior on modeBlend {
                NumberAnimation { id: modeAnimation; duration: root.backend.wallpaperSettings.modeTransitionMs; easing.type: Easing.InOutCubic }
            }

            function stage(path, blur) {
                backLayer.stage(path);
                frontLayer.stage(path);
            }
            function ready(path) {
                const front = frontLayer.ready(path);
                if (front !== "ready") return front;
                return backLayer.ready(path);
            }
            function commit(path, blur, snapshot) {
                backLayer.commit(path, root.backend.overviewOpen ? snapshot : ({type: "none", durationMs: 0}));
                frontLayer.commit(path, snapshot);
            }
            function cancel(path) {
                frontLayer.cancel(path);
                backLayer.cancel(path);
            }
            function stageStatic(path, blur, frontBlend, backBlend) {
                frontLayer.stageStatic(path, frontBlend); backLayer.stageStatic(blur, backBlend);
            }
            function readyStatic(path) {
                const front = frontLayer.readyStatic(path);
                return front === "ready" ? backLayer.readyStatic(root.backend.stagedStaticBlur) : front;
            }
            function commitStatic(path, blur, snapshot) {
                frontLayer.commitStatic(path, snapshot); backLayer.commitStatic(blur, snapshot);
            }
            function cancelStatic(path) { frontLayer.cancelStatic(path); backLayer.cancelStatic(root.backend.stagedStaticBlur); }
            function releaseAll() { frontLayer.releaseAll(); backLayer.releaseAll(); }
            function setSeed(frontPath, blurPath, frontBlend, backBlend) {
                frontLayer.seedBlend = frontBlend;
                frontLayer.seedPath = frontPath;
                backLayer.seedBlend = backBlend;
                backLayer.seedPath = blurPath;
            }
            function describe() {
                const front = frontLayer.describe(), back = backLayer.describe();
                return {output: modelData.name, progress: progress, animatedProgress: animatedProgress,
                    frontReady: frontReady, backReady: backReady, transitionActive: transitioning,
                    frontPosition: front.position, backPosition: back.position,
                    frontPlaying: front.playing, backPlaying: back.playing,
                    frontFrames: front.frames, backFrames: back.frames, frontX: front.x,
                    frontWidth: front.width, front: front, back: back};
            }

            Component.onCompleted: root.backend.registerScreen(display)
            Component.onDestruction: root.backend.unregisterScreen(display)

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
                    settings: root.backend.wallpaperSettings
                    motionProgress: display.animatedProgress
                    modeBlend: display.modeBlend
                    mirrorSourceLayer: frontLayer
                    liveMirror: root.backend.overviewOpen
                    allowShader: root.backend.overviewOpen
                    textureScale: 0.5
                    minSlotFrames: 2
                    releaseDelayMs: 0
                    Component.onCompleted: {
                        backLayer.seedBlend = root.backend.initialStaticBackBlend;
                        backLayer.seedPath = root.backend.initialStaticBlur;
                    }
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
                    settings: root.backend.wallpaperSettings
                    motionProgress: display.animatedProgress
                    modeBlend: display.modeBlend
                    minSlotFrames: 6
                    releaseDelayMs: 140
                    Component.onCompleted: {
                        frontLayer.seedBlend = root.backend.initialStaticFrontBlend;
                        frontLayer.seedPath = root.backend.initialStaticFront;
                    }
                }
            }

            Timer {
                interval: 500; repeat: true
                running: display.autoCommit
                onTriggered: {
                    if (display.ready(root.backend.currentPath) === "ready") {
                        display.commit(root.backend.currentPath, root.backend.currentBlur, {type: "none", durationMs: 0});
                        display.autoCommit = false;
                    }
                }
            }
        }
    }
}
