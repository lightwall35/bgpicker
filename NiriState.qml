import QtQuick
import Quickshell
import Quickshell.Io
import "WallpaperMath.js" as WallpaperMath

Item {
    id: root
    property bool subscribed: false
    readonly property bool connected: eventSocket.connected
    readonly property string socketPath: Quickshell.env("NIRI_SOCKET") || ""
    property var snapshot: WallpaperMath.initialState()
    readonly property bool overviewOpen: !!(snapshot && snapshot.overviewOpen)

    function connectSocket() {
        if (subscribed && socketPath !== "" && !eventSocket.connected)
            eventSocket.connected = true;
    }

    onSubscribedChanged: {
        if (subscribed) connectSocket();
        else {
            eventSocket.connected = false;
            snapshot = WallpaperMath.initialState();
        }
    }

    Socket {
        id: eventSocket
        path: root.socketPath
        onConnectedChanged: {
            if (connected) {
                root.snapshot = WallpaperMath.initialState();
                write('"EventStream"\n');
                flush();
            }
        }
        onError: error => {
            console.warn("BgPicker: niri IPC disconnected:", error);
            connected = false;
        }
        parser: SplitParser {
            onRead: line => {
                try {
                    root.snapshot = WallpaperMath.reduce(root.snapshot, JSON.parse(line));
                } catch (error) {
                    console.warn("BgPicker: invalid niri event:", error);
                }
            }
        }
    }

    // Only reconnect on disconnection; no timer or process polls window state.
    Timer {
        interval: 1500
        repeat: true
        running: root.subscribed && root.socketPath !== "" && !root.connected
        onTriggered: root.connectSocket()
    }
    Component.onCompleted: connectSocket()
}
