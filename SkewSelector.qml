import QtQuick

// Small segmented controls borrow the carousel's skew while keeping text upright.
Column {
    id: root
    property string caption: ""
    property var choices: []
    property string value: ""
    property real buttonWidth: 122
    property real gap: 10
    property real skew: -0.35
    property bool busy: false
    property bool available: true
    signal chosen(string value)

    spacing: 12
    opacity: available ? 1 : 0.4

    Text {
        visible: root.caption !== ""
        anchors.horizontalCenter: parent.horizontalCenter
        text: root.caption
        color: "#70FFFFFF"
        font.pixelSize: 10
        font.letterSpacing: 3
    }

    Item {
        id: rail
        width: root.choices.length * root.buttonWidth + Math.max(0, root.choices.length - 1) * root.gap
        height: 38
        readonly property real step: root.buttonWidth + root.gap
        readonly property real inset: -root.skew * height / 2
        readonly property int selectedIndex: root.choices.findIndex(c => c.value === root.value)

        Rectangle {
            visible: rail.selectedIndex >= 0
            x: Math.max(0, rail.selectedIndex) * rail.step + rail.inset
            width: root.buttonWidth
            height: parent.height
            radius: 5
            color: "#24FFFFFF"
            border.width: 1
            border.color: "#32FFFFFF"
            transform: Matrix4x4 {
                matrix: Qt.matrix4x4(1, root.skew, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)
            }
            Behavior on x {
                NumberAnimation {
                    duration: 300
                    easing.type: Easing.OutCubic
                }
            }
        }

        Repeater {
            model: root.choices
            Item {
                id: choice
                required property var modelData
                required property int index
                readonly property bool selected: root.value === modelData.value
                x: index * rail.step
                width: root.buttonWidth
                height: rail.height
                scale: mouse.containsMouse && !selected ? 1.035 : 1
                Behavior on scale {
                    NumberAnimation {
                        duration: 250
                        easing.type: Easing.OutBack
                    }
                }

                Rectangle {
                    x: rail.inset
                    width: parent.width
                    height: parent.height
                    radius: 5
                    color: mouse.containsMouse && !choice.selected ? "#12FFFFFF" : "#05FFFFFF"
                    transform: Matrix4x4 {
                        matrix: Qt.matrix4x4(1, root.skew, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)
                    }
                    Behavior on color {
                        ColorAnimation {
                            duration: 160
                        }
                    }
                }

                Row {
                    anchors.centerIn: parent
                    spacing: 9
                    opacity: choice.selected ? 1 : mouse.containsMouse ? 0.8 : 0.46
                    Behavior on opacity {
                        NumberAnimation {
                            duration: 180
                        }
                    }

                    Item {
                        visible: !!choice.modelData.icon
                        width: 16
                        height: 14
                        anchors.verticalCenter: parent.verticalCenter
                        Rectangle {
                            visible: choice.modelData.icon === "frame"
                            anchors.centerIn: parent
                            width: 14
                            height: 11
                            radius: 1.5
                            color: "transparent"
                            border.color: "#E8FFFFFF"
                            border.width: 1
                        }
                        Row {
                            visible: choice.modelData.icon === "layers"
                            anchors.centerIn: parent
                            spacing: 4
                            Repeater {
                                model: 3
                                Rectangle {
                                    width: 1.5
                                    height: 12
                                    color: "#E8FFFFFF"
                                    transform: Matrix4x4 {
                                        matrix: Qt.matrix4x4(1, root.skew, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)
                                    }
                                }
                            }
                        }
                    }
                    Text {
                        text: choice.modelData.label
                        color: "#F5F1EB"
                        font.pixelSize: 13
                        font.letterSpacing: 1
                    }
                }
                MouseArea {
                    id: mouse
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: enabled && !choice.selected ? Qt.PointingHandCursor : Qt.ArrowCursor
                    enabled: !root.busy && root.available
                    onClicked: if (!choice.selected)
                        root.chosen(choice.modelData.value)
                }
            }
        }
    }
}
