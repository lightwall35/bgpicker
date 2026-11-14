import QtQuick
import Quickshell
import Quickshell.Wayland
import "WallpaperMath.js" as WallpaperMath
import "WallpaperTransitions.js" as Transitions

Item {
    id: root
    required property var backend

    Variants {
        model: Quickshell.screens

        PanelWindow {
            id: view
            required property var modelData
            screen: modelData
            visible: root.backend.presented && frontIndex >= 0
            color: "transparent"
            WlrLayershell.namespace: root.backend.layerNamespace
            // Background keeps desktop widgets on Bottom visible. The switcher
            // clears awww to transparent only after this image has decoded.
            WlrLayershell.layer: WlrLayer.Background
            WlrLayershell.exclusionMode: ExclusionMode.Ignore
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }
            mask: Region {}

            property int frontIndex: -1
            property string stagedPath: ""
            property bool autoCommit: false
            property bool transitionActive: false
            property real transitionProgress: 1
            property var transitionSpec: Transitions.settings({})
            property bool shaderFailed: false
            property int lastShaderStatus: 0
            property real modeBlend: 1
            readonly property bool modeTransitionRunning: modeAnimation.running
            readonly property real progress: WallpaperMath.progress(root.backend.niriState, modelData.name, root.backend.settings.columnSpan)

            function imageAt(index) {
                return index === 0 ? imageA : imageB;
            }
            function describe() {
                const image = imageAt(frontIndex);
                return {
                    output: modelData.name,
                    progress: progress,
                    animatedProgress: canvas.animatedProgress,
                    x: frontIndex >= 0 ? image.x : 0,
                    width: frontIndex >= 0 ? image.width : 0,
                    height: frontIndex >= 0 ? image.height : 0,
                    decodedWidth: frontIndex >= 0 ? image.implicitWidth : 0,
                    decodedHeight: frontIndex >= 0 ? image.implicitHeight : 0,
                    transitionActive: transitionActive,
                    transitionProgress: transitionProgress,
                    effect: transitionSpec.type,
                    shaderStatus: lastShaderStatus,
                    modeBlend: modeBlend,
                    modeTransitionRunning: modeTransitionRunning,
                    loadedImages: (imageA.source.toString() !== "" ? 1 : 0) + (imageB.source.toString() !== "" ? 1 : 0)
                };
            }
            function finishTransition() {
                transitionAnimation.stop();
                transitionActive = false;
                transitionProgress = 1;
                if (frontIndex >= 0) {
                    imageAt(1 - frontIndex).source = "";
                }
            }
            function stage(path) {
                finishTransition();
                stagedPath = path;
                autoCommit = false;
                shaderFailed = false;
                const next = imageAt(frontIndex === 0 ? 1 : 0);
                next.source = WallpaperMath.fileUrl(path);
            }
            function ready(path) {
                if (path !== stagedPath)
                    return "loading";
                const image = imageAt(frontIndex === 0 ? 1 : 0);
                if (image.status === Image.Error)
                    return "error: Qt could not load the image";
                return image.status === Image.Ready ? "ready" : "loading";
            }
            function commit(path, snapshot) {
                if (ready(path) !== "ready")
                    return;
                const hadPrevious = frontIndex >= 0;
                frontIndex = frontIndex === 0 ? 1 : 0;
                imageAt(frontIndex).z = 1;
                imageAt(1 - frontIndex).z = 0;
                stagedPath = "";
                transitionSpec = Transitions.settings({
                    transition: snapshot
                });
                if (hadPrevious && transitionSpec.type !== "none" && transitionSpec.durationMs > 0) {
                    transitionProgress = 0;
                    transitionActive = true;
                    transitionAnimation.restart();
                } else
                    finishTransition();
            }
            function cancel() {
                if (stagedPath !== "")
                    imageAt(frontIndex === 0 ? 1 : 0).source = "";
                stagedPath = "";
            }
            function animateMode(target) {
                finishTransition();
                modeAnimation.stop();
                modeAnimation.from = target === "enter" ? 0 : modeBlend;
                modeAnimation.to = target === "enter" ? 1 : 0;
                modeBlend = modeAnimation.from;
                if (root.backend.settings.modeTransitionMs === 0) modeBlend = modeAnimation.to;
                else modeAnimation.start();
            }
            function cancelModeTransition() {
                modeAnimation.stop();
                modeBlend = 1;
            }
            function imageLoaded() {
                if (autoCommit && ready(stagedPath) === "ready") {
                    autoCommit = false;
                    commit(stagedPath, root.backend.transition);
                }
            }

            Item {
                id: canvas
                anchors.fill: parent
                clip: true
                opacity: view.modeBlend
                readonly property bool useShader: view.transitionActive && Transitions.kind(view.transitionSpec.type) > 0 && GraphicsInfo.api !== GraphicsInfo.Software && !view.shaderFailed

                // A single per-screen progress animation drives both images,
                // keeping their positions aligned while crossfading.
                property real animatedProgress: view.progress
                Behavior on animatedProgress {
                    enabled: view.visible
                    NumberAnimation {
                        duration: root.backend.settings.durationMs
                        easing.type: Easing.OutCubic
                    }
                }

                Image {
                    id: imageA
                    readonly property var geometry: WallpaperMath.modeGeometry(canvas.width, canvas.height, implicitWidth, implicitHeight, root.backend.settings.zoom, canvas.animatedProgress, view.modeBlend)
                    x: geometry.x
                    y: geometry.y
                    width: geometry.width
                    height: geometry.height
                    asynchronous: true
                    cache: false
                    smooth: true
                    visible: source.toString() !== "" && !canvas.useShader && (view.frontIndex === 0 || view.transitionActive)
                    opacity: view.transitionActive && view.frontIndex === 0 ? view.transitionProgress : 1
                    sourceSize: Qt.size(root.backend.settings.maxTextureSize, Math.min(root.backend.settings.maxTextureSize, Math.ceil(view.modelData.height * view.modelData.devicePixelRatio * root.backend.settings.zoom)))
                    onStatusChanged: view.imageLoaded()
                }

                Image {
                    id: imageB
                    readonly property var geometry: WallpaperMath.modeGeometry(canvas.width, canvas.height, implicitWidth, implicitHeight, root.backend.settings.zoom, canvas.animatedProgress, view.modeBlend)
                    x: geometry.x
                    y: geometry.y
                    width: geometry.width
                    height: geometry.height
                    asynchronous: true
                    cache: false
                    smooth: true
                    visible: source.toString() !== "" && !canvas.useShader && (view.frontIndex === 1 || view.transitionActive)
                    opacity: view.transitionActive && view.frontIndex === 1 ? view.transitionProgress : 1
                    sourceSize: imageA.sourceSize
                    onStatusChanged: view.imageLoaded()
                }

                // Sample Image textures directly: no full-screen offscreen
                // copies. Account for each image's crop and parallax position.
                Loader {
                    id: transitionEffect
                    anchors.fill: parent
                    active: canvas.useShader
                    z: 2
                    sourceComponent: ShaderEffect {
                        anchors.fill: parent
                        property var oldSource: view.imageAt(1 - view.frontIndex)
                        property var newSource: view.imageAt(view.frontIndex)
                        property real transitionProgress: view.transitionProgress
                        property int effectKind: Transitions.kind(view.transitionSpec.type)
                        property vector2d resolution: Qt.vector2d(width, height)
                        property vector4d oldRect: oldSource ? Qt.vector4d(oldSource.x, oldSource.y, oldSource.width, oldSource.height) : Qt.vector4d(0, 0, 1, 1)
                        property vector4d newRect: newSource ? Qt.vector4d(newSource.x, newSource.y, newSource.width, newSource.height) : Qt.vector4d(0, 0, 1, 1)
                        property vector2d origin: Qt.vector2d(view.transitionSpec.position[0], view.transitionSpec.position[1])
                        property vector2d direction: Qt.vector2d(-Math.cos(view.transitionSpec.angle * Math.PI / 180), Math.sin(view.transitionSpec.angle * Math.PI / 180))
                        property vector2d wave: Qt.vector2d(view.transitionSpec.wave[0], view.transitionSpec.wave[1])
                        fragmentShader: Qt.resolvedUrl("shaders/wallpaper-transition.frag.qsb")
                        onStatusChanged: {
                            view.lastShaderStatus = status;
                            if (status === ShaderEffect.Error) {
                                console.warn("BgPicker: transition shader failed, using fade:", log);
                                view.shaderFailed = true;
                            }
                        }
                    }
                }
            }

            NumberAnimation {
                id: modeAnimation
                target: view
                property: "modeBlend"
                duration: root.backend.settings.modeTransitionMs
                easing.type: Easing.InOutCubic
            }

            NumberAnimation {
                id: transitionAnimation
                target: view
                property: "transitionProgress"
                from: 0
                to: 1
                duration: view.transitionSpec.durationMs
                easing.type: Easing.BezierSpline
                easing.bezierCurve: [0.54, 0, 0.34, 0.99, 1, 1]
                onFinished: view.finishTransition()
            }
            Component.onCompleted: root.backend.registerRenderer(view)
            Component.onDestruction: root.backend.unregisterRenderer(view)
        }
    }
}
