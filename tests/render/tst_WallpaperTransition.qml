import QtQuick
import QtTest

Item {
    id: stage
    width: 128
    height: 96
    readonly property bool hardware: GraphicsInfo.api !== GraphicsInfo.Software

    Image {
        id: before
        source: "../fixtures/red-green.png"
        visible: false
    }
    Image {
        id: after
        source: "../fixtures/blue.png"
        visible: false
    }

    ShaderEffect {
        id: effect
        anchors.fill: parent
        property var oldSource: before
        property var newSource: after
        property real transitionProgress: 0
        property int effectKind: 0
        property vector2d resolution: Qt.vector2d(width, height)
        property vector4d oldRect: Qt.vector4d(0, 0, width * 2, height)
        property vector4d newRect: Qt.vector4d(0, 0, width, height)
        property vector2d origin: Qt.vector2d(0.5, 0.5)
        property vector2d direction: Qt.vector2d(-1, 0)
        property vector2d wave: Qt.vector2d(20, 20)
        fragmentShader: "../../shaders/wallpaper-transition.frag.qsb"
    }

    TestCase {
        name: "WallpaperTransitionPixels"
        when: windowShown
        function initTestCase() {
            if (!stage.hardware)
                return;
            tryCompare(before, "status", Image.Ready);
            tryCompare(after, "status", Image.Ready);
            tryCompare(effect, "status", ShaderEffect.Compiled);
        }

        function init() {
            if (!stage.hardware)
                skip("Pixel tests need the Qt hardware scene graph.");
            effect.oldRect = Qt.vector4d(0, 0, stage.width * 2, stage.height);
            effect.newRect = Qt.vector4d(0, 0, stage.width, stage.height);
            effect.transitionProgress = 0;
        }
        function pixel(x, y) {
            wait(35);
            var result = grabImage(effect);
            return result.pixel(Math.floor(x * (result.width - 1)), Math.floor(y * (result.height - 1)));
        }
        function nearColor(value, r, g, b) {
            verify(Math.abs(value.r - r) < 0.025 && Math.abs(value.g - g) < 0.025 && Math.abs(value.b - b) < 0.025, "Unexpected pixel: " + value);
        }
        function test_endpoints_data() {
            return [0, 1, 2, 3, 4].map(function (kind) {
                return {
                    tag: "effect-" + kind,
                    kind: kind
                };
            });
        }
        function test_endpoints(data) {
            effect.effectKind = data.kind;
            effect.transitionProgress = 0;
            nearColor(pixel(0.1, 0.5), 1, 0, 0);
            effect.transitionProgress = 1;
            nearColor(pixel(0.9, 0.5), 0, 0, 1);
        }
        function test_half_fade_blends_both_images() {
            effect.effectKind = 0;
            effect.transitionProgress = 0.5;
            nearColor(pixel(0.1, 0.5), 0.5, 0, 0.5);
        }
        function test_wipe_and_wave_reveal_opposite_edges() {
            for (var kind of [1, 2]) {
                effect.effectKind = kind;
                effect.transitionProgress = 0.5;
                nearColor(pixel(0.05, 0.5), 1, 0, 0);
                nearColor(pixel(0.95, 0.5), 0, 0, 1);
            }
        }
        function test_grow_and_outer_have_opposite_masks() {
            effect.effectKind = 3;
            effect.transitionProgress = 0.5;
            nearColor(pixel(0.5, 0.5), 0, 0, 1);
            nearColor(pixel(0.02, 0.02), 1, 0, 0);
            effect.effectKind = 4;
            nearColor(pixel(0.5, 0.5), 1, 0, 0);
            nearColor(pixel(0.98, 0.02), 0, 0, 1);
        }
        function test_crop_position_is_preserved() {
            effect.effectKind = 1;
            effect.transitionProgress = 0;
            effect.oldRect = Qt.vector4d(-stage.width, 0, stage.width * 2, stage.height);
            nearColor(pixel(0.8, 0.5), 0, 1, 0);
        }
    }
}
