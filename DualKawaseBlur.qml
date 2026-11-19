import QtQuick

Item {
    id: root
    required property Item sourceItem
    property bool active: true
    property int passes: 3
    property real offset: 1.25
    property real saturation: 1.12
    property real brightness: -0.04

    readonly property int baseW: Math.max(64, Math.ceil(width * 0.5))
    readonly property int baseH: Math.max(64, Math.ceil(height * 0.5))
    readonly property int w1: Math.max(32, Math.ceil(baseW * 0.5))
    readonly property int h1: Math.max(32, Math.ceil(baseH * 0.5))
    readonly property int w2: Math.max(16, Math.ceil(w1 * 0.5))
    readonly property int h2: Math.max(16, Math.ceil(h1 * 0.5))
    readonly property int w3: Math.max(8, Math.ceil(w2 * 0.5))
    readonly property int h3: Math.max(8, Math.ceil(h2 * 0.5))

    ShaderEffectSource {
        id: s0
        sourceItem: root.active ? root.sourceItem : null
        live: root.active
        hideSource: root.active
        smooth: true
        textureSize: Qt.size(root.baseW, root.baseH)
    }

    ShaderEffect {
        id: d1
        width: root.w1; height: root.h1
        visible: false
        property variant source: s0
        property vector2d halfpixel: Qt.vector2d(0.5 / root.baseW, 0.5 / root.baseH)
        property real offset: root.offset
        fragmentShader: Qt.resolvedUrl("shaders/kawase-down.frag.qsb")
    }
    ShaderEffectSource {
        id: s1
        sourceItem: root.active && root.passes >= 1 ? d1 : null
        live: root.active && root.passes >= 1
        smooth: true
        textureSize: Qt.size(root.w1, root.h1)
    }

    ShaderEffect {
        id: d2
        width: root.w2; height: root.h2
        visible: false
        property variant source: s1
        property vector2d halfpixel: Qt.vector2d(0.5 / root.w1, 0.5 / root.h1)
        property real offset: root.offset
        fragmentShader: Qt.resolvedUrl("shaders/kawase-down.frag.qsb")
    }
    ShaderEffectSource {
        id: s2
        sourceItem: root.active && root.passes >= 2 ? d2 : null
        live: root.active && root.passes >= 2
        smooth: true
        textureSize: Qt.size(root.w2, root.h2)
    }

    ShaderEffect {
        id: d3
        width: root.w3; height: root.h3
        visible: false
        property variant source: s2
        property vector2d halfpixel: Qt.vector2d(0.5 / root.w2, 0.5 / root.h2)
        property real offset: root.offset
        fragmentShader: Qt.resolvedUrl("shaders/kawase-down.frag.qsb")
    }
    ShaderEffectSource {
        id: s3
        sourceItem: root.active && root.passes >= 3 ? d3 : null
        live: root.active && root.passes >= 3
        smooth: true
        textureSize: Qt.size(root.w3, root.h3)
    }

    ShaderEffect {
        id: u1
        width: root.w2; height: root.h2
        visible: false
        property variant source: s3
        property vector2d halfpixel: Qt.vector2d(0.5 / root.w3, 0.5 / root.h3)
        property real offset: root.offset
        property real saturation: 1.0
        property real brightness: 0.0
        fragmentShader: Qt.resolvedUrl("shaders/kawase-up.frag.qsb")
    }
    ShaderEffectSource {
        id: su1
        sourceItem: root.active && root.passes >= 3 ? u1 : null
        live: root.active && root.passes >= 3
        smooth: true
        textureSize: Qt.size(root.w2, root.h2)
    }

    ShaderEffect {
        id: u2
        width: root.w1; height: root.h1
        visible: false
        property variant source: root.passes >= 3 ? su1 : s2
        property vector2d halfpixel: Qt.vector2d(0.5 / root.w2, 0.5 / root.h2)
        property real offset: root.offset
        property real saturation: 1.0
        property real brightness: 0.0
        fragmentShader: Qt.resolvedUrl("shaders/kawase-up.frag.qsb")
    }
    ShaderEffectSource {
        id: su2
        sourceItem: root.active && root.passes >= 2 ? u2 : null
        live: root.active && root.passes >= 2
        smooth: true
        textureSize: Qt.size(root.w1, root.h1)
    }

    ShaderEffect {
        id: u3
        width: root.baseW; height: root.baseH
        visible: false
        property variant source: root.passes >= 2 ? su2 : s1
        property vector2d halfpixel: Qt.vector2d(0.5 / root.w1, 0.5 / root.h1)
        property real offset: root.offset
        property real saturation: 1.0
        property real brightness: 0.0
        fragmentShader: Qt.resolvedUrl("shaders/kawase-up.frag.qsb")
    }
    ShaderEffectSource {
        id: su3
        sourceItem: root.active && root.passes >= 1 ? u3 : null
        live: root.active && root.passes >= 1
        smooth: true
        textureSize: Qt.size(root.baseW, root.baseH)
    }

    ShaderEffect {
        id: finalOut
        anchors.fill: parent
        visible: root.active
        property variant source: root.passes >= 1 ? su3 : s0
        property vector2d halfpixel: Qt.vector2d(0.5 / root.baseW, 0.5 / root.baseH)
        property real offset: root.offset
        property real saturation: root.saturation
        property real brightness: root.brightness
        fragmentShader: Qt.resolvedUrl("shaders/kawase-up.frag.qsb")
    }
}
