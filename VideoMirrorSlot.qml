import QtQuick
import QtMultimedia
import BgPickerVideo

Item {
    id: slot
    property string mediaPath: ""
    property var sourceSlot: null
    property bool liveMirror: true
    property bool decoded: false
    property int frameCount: 0
    property string failure: ""
    property bool trackFrames: true
    property int minFrames: 2
    readonly property bool ready: decoded && frameCount >= minFrames && output.sourceRect.width > 0
    readonly property bool playing: sourceSlot ? sourceSlot.playing : false
    readonly property int position: sourceSlot ? sourceSlot.position : 0
    readonly property bool seekable: false
    readonly property real sourceWidth: output.sourceRect.width
    readonly property real sourceHeight: output.sourceRect.height
    readonly property var videoSink: output.videoSink

    function load(path) {
        decoded = false;
        frameCount = 0;
        failure = "";
        mediaPath = path;
        splitter.clearTarget();
    }
    function pauseIdle() {}
    function resume() {}
    function seek(position) {}
    function release() {
        mediaPath = "";
        decoded = false;
        frameCount = 0;
        splitter.clearTarget();
        output.clearOutput();
    }

    VideoFrameSplitter {
        id: splitter
        sourceSink: slot.sourceSlot && slot.mediaPath !== "" && slot.sourceSlot.mediaPath === slot.mediaPath
            ? slot.sourceSlot.videoSink
            : null
        targetSink: output.videoSink
        live: slot.liveMirror
    }

    VideoOutput {
        id: output
        anchors.fill: parent
        fillMode: VideoOutput.Stretch
        Connections {
            target: output.videoSink
            enabled: !slot.ready || slot.trackFrames
            function onVideoFrameChanged(frame) {
                if (output.sourceRect.width > 0 && slot.mediaPath !== ""
                        && slot.sourceSlot && slot.sourceSlot.mediaPath === slot.mediaPath) {
                    slot.decoded = true;
                    slot.frameCount++;
                }
            }
        }
    }

    DualKawaseBlur {
        anchors.fill: parent
        sourceItem: output
        active: slot.visible && slot.mediaPath !== "" && slot.decoded
    }
}
