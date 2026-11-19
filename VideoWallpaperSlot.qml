import QtQuick
import QtMultimedia
import "WallpaperMath.js" as WallpaperMath

Item {
    id: slot
    property string mediaPath: ""
    property bool decoded: false
    property int frameCount: 0
    property string failure: ""
    property bool trackFrames: true
    property int minFrames: 2
    readonly property bool ready: decoded && frameCount >= minFrames && player.hasVideo && player.error === MediaPlayer.NoError
    readonly property bool playing: player.playing
    readonly property int position: player.position
    readonly property bool seekable: player.seekable
    readonly property real sourceWidth: output.sourceRect.width
    readonly property real sourceHeight: output.sourceRect.height
    readonly property var videoSink: output.videoSink
    function load(path) {
        player.stop(); output.clearOutput(); decoded = false; frameCount = 0; failure = "";
        mediaPath = path;
        // Decoding starts before readiness and continues throughout the effect.
        player.play();
    }
    function pauseIdle() {
        player.pause();
    }
    function resume() {
        if (mediaPath !== "") player.play();
    }
    function release() {
        player.stop(); mediaPath = ""; output.clearOutput(); decoded = false;
    }
    function seek(position) { if (player.seekable) player.position = position; }
    VideoOutput {
        id: output
        anchors.fill: parent
        fillMode: VideoOutput.Stretch
        Connections {
            target: output.videoSink
            enabled: !slot.ready || slot.trackFrames
            function onVideoFrameChanged(frame) {
                if (output.sourceRect.width > 0 && player.hasVideo) {
                    slot.decoded = true;
                    slot.frameCount++;
                }
            }
        }
    }
    MediaPlayer {
        id: player
        source: slot.mediaPath === "" ? "" : WallpaperMath.fileUrl(slot.mediaPath)
        videoOutput: output
        loops: MediaPlayer.Infinite
        onErrorOccurred: (error, message) => { slot.failure = message; console.warn("BgPicker video:", message); }
    }
}
