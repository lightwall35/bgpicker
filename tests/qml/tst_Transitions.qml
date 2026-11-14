import QtQuick
import QtTest
import "../../WallpaperTransitions.js" as Transitions

TestCase {
    name: "Transitions"
    function test_shared_defaults_and_legacy_migration() {
        compare(Transitions.settings({}).type, "fade");
        compare(Transitions.settings({}).durationMs, 800);
        compare(Transitions.settings({parallax: {transitionMs: 350}}).durationMs, 350);
        compare(Transitions.settings({parallax: {transitionMs: 350}, transition: {durationMs: 900}}).durationMs, 900);
    }
    function test_all_choices_have_distinct_values() {
        compare(Transitions.choices.length, 7);
        var values = Transitions.choices.map(c => c.value);
        for (var i = 0; i < values.length; i++) compare(values.indexOf(values[i]), i);
        compare(Transitions.kind("wipe"), 1);
        compare(Transitions.kind("wave"), 2);
        compare(Transitions.kind("grow"), 3);
        compare(Transitions.kind("outer"), 4);
        compare(Transitions.kind("none"), 5);
    }
    function test_numeric_bounds_match_shell_preferences() {
        var p = Transitions.settings({transition: {type: "bad", durationMs: -20,
            fps: 1000, angle: "NaN", position: [-2, 5], wave: [0, 999]}});
        compare(p, {type: "fade", durationMs: 0, fps: 240, angle: 45,
            position: [0, 1], wave: [1, 500]});
    }
}
