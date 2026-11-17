import QtQuick
import "WallpaperMath.js" as WallpaperMath
import "WallpaperTransitions.js" as Transitions

Item {
    id: rootLayer
    property var settings: WallpaperMath.settings({})
    property real motionProgress: 0.5
    property real modeBlend: 1
    property real seedBlend: modeBlend
    property string seedPath: ""
    property int frontIndex: -1
    property string stagedPath: ""
    property bool staticStaged: false
    property bool staticActive: false
    property bool transitionActive: false
    property real transitionProgress: 1
    property var transitionSpec: Transitions.settings({})
    property bool shaderFailed: false
    readonly property bool presented: frontIndex >= 0 || staticActive
    readonly property bool shaderEnabled: transitionActive && transitionSpec.type !== "fade"
        && transitionSpec.type !== "none" && GraphicsInfo.api !== GraphicsInfo.Software && !shaderFailed
    clip: true

    function slot(index) { return index === 0 ? slotA : slotB; }
    function incoming() { return slot(frontIndex === 0 ? 1 : 0); }
    function oldItem() {
        if (staticActive) return frontIndex >= 0 ? slot(frontIndex) : seed;
        return frontIndex >= 0 && slot(1 - frontIndex).mediaPath !== "" ? slot(1 - frontIndex) : seed;
    }
    function newItem() { return staticActive ? seed : slot(frontIndex); }
    function finish() {
        transitionAnimation.stop(); transitionActive = false; transitionProgress = 1;
        if (staticActive) {
            slotA.release(); slotB.release(); frontIndex = -1;
        } else {
            if (frontIndex >= 0) slot(1 - frontIndex).release();
            seedPath = "";
        }
    }
    function stage(path) {
        if (transitionActive || frontIndex >= 0) finish();
        staticStaged = false;
        stagedPath = path; shaderFailed = false;
        incoming().load(path);
    }
    function ready(path) {
        if (staticStaged || path !== stagedPath) return "error: superseded request";
        if (frontIndex < 0 && seedPath !== "" && seed.status === Image.Loading) return "loading";
        const next = incoming();
        if (next.failure !== "") return "error: " + next.failure;
        return next.ready ? "ready" : "loading";
    }
    function commit(path, snapshot) {
        if (ready(path) !== "ready") return;
        const hadPrevious = frontIndex >= 0 || staticActive || (seed.source.toString() !== "" && seed.status === Image.Ready);
        staticStaged = false; staticActive = false;
        frontIndex = frontIndex === 0 ? 1 : 0;
        stagedPath = "";
        transitionSpec = Transitions.settings({transition: snapshot});
        if (hadPrevious && transitionSpec.type !== "none" && transitionSpec.durationMs > 0) {
            transitionProgress = 0; transitionActive = true; transitionAnimation.restart();
        } else finish();
    }
    function cancel(path) {
        if (!staticStaged && stagedPath === path) { incoming().release(); stagedPath = ""; }
    }
    function stageStatic(path, blend) {
        if (transitionActive) finish();
        staticStaged = true;
        stagedPath = path; shaderFailed = false;
        seedBlend = blend;
        seedPath = "";
        seedPath = path;
    }
    function readyStatic(path) {
        if (!staticStaged || path !== stagedPath) return "error: superseded request";
        if (seed.status === Image.Error) return "error: Qt could not load the static image";
        return seed.status === Image.Ready ? "ready" : "loading";
    }
    function commitStatic(path, snapshot) {
        if (readyStatic(path) !== "ready") return;
        const hadPrevious = frontIndex >= 0;
        staticStaged = false; staticActive = true;
        stagedPath = "";
        transitionSpec = Transitions.settings({transition: snapshot});
        if (hadPrevious && transitionSpec.type !== "none" && transitionSpec.durationMs > 0) {
            transitionProgress = 0; transitionActive = true; transitionAnimation.restart();
        } else finish();
    }
    function cancelStatic(path) {
        if (staticStaged && stagedPath === path) {
            staticStaged = false; stagedPath = "";
            if (!staticActive) seedPath = "";
        }
    }
    function syncWith(other) {
        for (let i = 0; i < 2; i++) {
            const a = slot(i), b = other.slot(i);
            if (a.ready && b.ready && a.seekable && Math.abs(a.position - b.position) > 300)
                a.seek(b.position);
        }
    }
    function describe() {
        const active = !staticActive && frontIndex >= 0 ? slot(frontIndex) : null;
        const next = stagedPath !== "" && !staticStaged ? incoming() : active;
        const previous = staticActive ? (frontIndex >= 0 ? slot(frontIndex) : null) : (frontIndex >= 0 ? slot(1 - frontIndex) : null);
        const visual = staticActive ? seed : active;
        return {active: presented, staticActive: staticActive, staged: stagedPath, transitionActive: transitionActive,
            transitionProgress: transitionProgress, effect: transitionSpec.type,
            incomingPlaying: next ? next.playing : false, incomingPosition: next ? next.position : 0,
            incomingFrames: next ? next.frameCount : 0,
            oldPlaying: transitionActive && previous ? previous.playing : false,
            oldPosition: transitionActive && previous ? previous.position : 0,
            oldFrames: transitionActive && previous ? previous.frameCount : 0,
            position: active ? active.position : 0, frames: active ? active.frameCount : 0,
            playing: active ? active.playing : false, x: visual ? visual.x : 0,
            width: visual ? visual.width : 0};
    }
    Image {
        id: seed
        readonly property var geometry: WallpaperMath.modeGeometry(rootLayer.width, rootLayer.height, implicitWidth, implicitHeight,
            rootLayer.settings.zoom, rootLayer.motionProgress, rootLayer.seedBlend)
        x: geometry.x; y: geometry.y; width: geometry.width; height: geometry.height
        source: rootLayer.seedPath === "" ? "" : WallpaperMath.fileUrl(rootLayer.seedPath)
        asynchronous: rootLayer.staticStaged
        visible: rootLayer.staticActive || (!rootLayer.staticStaged && (rootLayer.frontIndex < 0 || rootLayer.transitionActive))
        z: rootLayer.staticActive ? 2 : 0
        opacity: rootLayer.staticActive && rootLayer.transitionActive && !rootLayer.shaderEnabled ? rootLayer.transitionProgress : 1
        smooth: true
        cache: false
        sourceSize: Qt.size(rootLayer.settings.maxTextureSize, Math.min(rootLayer.settings.maxTextureSize, Math.ceil(rootLayer.height * 2 * rootLayer.settings.zoom)))
    }
    VideoWallpaperSlot {
        id: slotA
        readonly property bool isStagedSlot: !rootLayer.staticStaged && rootLayer.stagedPath !== "" && rootLayer.frontIndex !== 0
        readonly property var geometry: WallpaperMath.modeGeometry(rootLayer.width, rootLayer.height, sourceWidth, sourceHeight,
            rootLayer.settings.zoom, rootLayer.motionProgress, rootLayer.modeBlend)
        x: geometry.x; y: geometry.y; width: geometry.width; height: geometry.height
        visible: rootLayer.staticActive ? (rootLayer.frontIndex === 0 && rootLayer.transitionActive) : (rootLayer.frontIndex === 0 || rootLayer.transitionActive || isStagedSlot)
        z: !rootLayer.staticActive && rootLayer.frontIndex === 0 ? 1 : (isStagedSlot ? -1 : 0)
        opacity: !rootLayer.staticActive && rootLayer.transitionActive && !rootLayer.shaderEnabled && rootLayer.frontIndex === 0 ? rootLayer.transitionProgress : 1
    }
    VideoWallpaperSlot {
        id: slotB
        readonly property bool isStagedSlot: !rootLayer.staticStaged && rootLayer.stagedPath !== "" && rootLayer.frontIndex === 0
        readonly property var geometry: WallpaperMath.modeGeometry(rootLayer.width, rootLayer.height, sourceWidth, sourceHeight,
            rootLayer.settings.zoom, rootLayer.motionProgress, rootLayer.modeBlend)
        x: geometry.x; y: geometry.y; width: geometry.width; height: geometry.height
        visible: rootLayer.staticActive ? (rootLayer.frontIndex === 1 && rootLayer.transitionActive) : (rootLayer.frontIndex === 1 || rootLayer.transitionActive || isStagedSlot)
        z: !rootLayer.staticActive && rootLayer.frontIndex === 1 ? 1 : (isStagedSlot ? -1 : 0)
        opacity: !rootLayer.staticActive && rootLayer.transitionActive && !rootLayer.shaderEnabled && rootLayer.frontIndex === 1 ? rootLayer.transitionProgress : 1
    }
    Item {
        anchors.fill: parent
        visible: rootLayer.shaderEnabled
        z: 3
        ShaderEffectSource {
            id: beforeTexture
            sourceItem: rootLayer.shaderEnabled ? rootLayer.oldItem() : null
            live: rootLayer.shaderEnabled
            hideSource: false
            textureSize: Qt.size(Math.ceil(rootLayer.width), Math.ceil(rootLayer.height))
        }
        ShaderEffectSource {
            id: afterTexture
            sourceItem: rootLayer.shaderEnabled ? rootLayer.newItem() : null
            live: rootLayer.shaderEnabled
            hideSource: rootLayer.shaderEnabled
            textureSize: Qt.size(Math.ceil(rootLayer.width), Math.ceil(rootLayer.height))
        }
        ShaderEffect {
            anchors.fill: parent
            property var oldSource: beforeTexture
            property var newSource: afterTexture
            property real transitionProgress: rootLayer.transitionProgress
            property int effectKind: Transitions.kind(rootLayer.transitionSpec.type)
            property vector2d resolution: Qt.vector2d(width, height)
            readonly property var before: rootLayer.oldItem()
            readonly property var after: rootLayer.newItem()
            property vector4d oldRect: Qt.vector4d(before.x, before.y, before.width, before.height)
            property vector4d newRect: Qt.vector4d(after.x, after.y, after.width, after.height)
            property vector2d origin: Qt.vector2d(rootLayer.transitionSpec.position[0], rootLayer.transitionSpec.position[1])
            property vector2d direction: Qt.vector2d(-Math.cos(rootLayer.transitionSpec.angle * Math.PI / 180), Math.sin(rootLayer.transitionSpec.angle * Math.PI / 180))
            property vector2d wave: Qt.vector2d(rootLayer.transitionSpec.wave[0], rootLayer.transitionSpec.wave[1])
            fragmentShader: Qt.resolvedUrl("shaders/wallpaper-transition.frag.qsb")
            onStatusChanged: if (status === ShaderEffect.Error) { rootLayer.shaderFailed = true; console.warn("Video transition fallback:", log); }
        }
    }
    NumberAnimation {
        id: transitionAnimation
        target: rootLayer; property: "transitionProgress"
        from: 0; to: 1; duration: rootLayer.transitionSpec.durationMs
        easing.type: Easing.BezierSpline
        easing.bezierCurve: [0.54, 0, 0.34, 0.99, 1, 1]
        onFinished: rootLayer.finish()
    }
}
