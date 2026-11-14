import QtQuick
import QtTest
import "../../WallpaperMath.js" as WallpaperMath

TestCase {
    name: "WallpaperMath"

    function window(id, workspace, col, focused, floating) {
        return {id: id, workspace_id: workspace, is_focused: !!focused,
            is_floating: !!floating, layout: {pos_in_scrolling_layout: col === null ? null : [col, 1]}};
    }
    function fixture() {
        var s = WallpaperMath.reduce(WallpaperMath.initialState(), {WorkspacesChanged: {workspaces: [
            {id: 1, output: "DP-1", is_active: true, active_window_id: 2},
            {id: 2, output: "DP-1", is_active: false, active_window_id: 4},
            {id: 3, output: "DP-2", is_active: true, active_window_id: 5}]}});
        return WallpaperMath.reduce(s, {WindowsChanged: {windows: [
            window(1, 1, 1), window(2, 1, 3, true), window(3, 1, 3),
            window(4, 2, 1), window(5, 3, 1)]}});
    }

    function test_panorama_covers_without_upscaling_zoom() {
        var g = WallpaperMath.geometry(1920, 1080, 7200, 2400, 1.12);
        compare(g.width, 3240);
        compare(g.height, 1080);
        compare(g.overflowX, 1320);
        compare(g.y, 0);
    }
    function test_mode_geometry_interpolates_between_center_and_parallax() {
        var centered = WallpaperMath.modeGeometry(1920, 1080, 7200, 2400, 1.12, 0, 0);
        var halfway = WallpaperMath.modeGeometry(1920, 1080, 7200, 2400, 1.12, 0, 0.5);
        var parallax = WallpaperMath.modeGeometry(1920, 1080, 7200, 2400, 1.12, 0, 1);
        compare(centered.x, -660);
        compare(parallax.x, 0);
        compare(halfway.x, -330);
        compare(centered.width, 3240);
        compare(parallax.width, 3240);
        var ordinary = WallpaperMath.modeGeometry(1920, 1080, 1920, 1080, 1.12, 1, 0);
        var enlarged = WallpaperMath.modeGeometry(1920, 1080, 1920, 1080, 1.12, 1, 1);
        verify(ordinary.width === 1920 && enlarged.width > ordinary.width);
        verify(enlarged.x < ordinary.x && enlarged.y < ordinary.y);
    }
    function test_ordinary_and_portrait_never_expose_empty_edges() {
        for (var aspect of [[1920, 1080], [1000, 2000], [0, 0]]) {
            var g = WallpaperMath.geometry(1920, 1080, aspect[0], aspect[1], 1.12);
            verify(g.width >= 1920 && g.height >= 1080);
            verify(g.overflowX > 0 && g.y < 0);
            // Both extreme crops stay inside the image.
            verify(-g.overflowX + g.width >= 1920);
        }
    }
    function test_columns_are_deduplicated_and_ranked_not_raw_indices() {
        var s = fixture();
        compare(WallpaperMath.progress(s, "DP-1", 6), 0.2);
        compare(WallpaperMath.progress(s, "DP-1", 0), 1);
        compare(WallpaperMath.progress(s, "DP-2", 6), 0);
        compare(WallpaperMath.progress(s, "unplugged", 6), 0.5);
    }
    function test_floating_focus_remembers_previous_tiled_column() {
        var s = fixture();
        s = WallpaperMath.reduce(s, {WindowOpenedOrChanged: {window: window(9, 1, null, true, true)}});
        s = WallpaperMath.reduce(s, {WorkspaceActiveWindowChanged: {workspace_id: 1, active_window_id: 9}});
        compare(WallpaperMath.progress(s, "DP-1", 6), 0.2);
        s = WallpaperMath.reduce(s, {WindowClosed: {id: 2}});
        s = WallpaperMath.reduce(s, {WindowClosed: {id: 3}});
        compare(WallpaperMath.progress(s, "DP-1", 6), 0);
    }
    function test_workspace_activation_affects_only_its_output() {
        var s = WallpaperMath.reduce(fixture(), {WorkspaceActivated: {id: 2, focused: true}});
        verify(!s.workspaces[0].is_active);
        verify(s.workspaces[1].is_active);
        verify(s.workspaces[2].is_active);
        compare(WallpaperMath.progress(s, "DP-1", 6), 0);
    }
    function test_layout_event_moves_focus_and_empty_workspace_centers() {
        var s = fixture();
        s = WallpaperMath.reduce(s, {WindowLayoutsChanged: {changes: [[2, {pos_in_scrolling_layout: [5, 1]}]]}});
        compare(WallpaperMath.progress(s, "DP-1", 6), 0.4);
        s = WallpaperMath.reduce(s, {WindowsChanged: {windows: []}});
        compare(WallpaperMath.progress(s, "DP-1", 6), 0.5);
    }
    function test_unknown_events_and_reconnect_snapshot() {
        var s = fixture();
        compare(WallpaperMath.reduce(s, {NewFutureEvent: {}}), s);
        s = WallpaperMath.reduce(s, {WorkspacesChanged: {workspaces: []}});
        compare(Object.keys(s.lastColumns).length, 0);
    }
    function test_settings_are_bounded_and_file_urls_escape_special_characters() {
        var p = WallpaperMath.settings({parallax: {zoom: 100, durationMs: -1, columnSpan: 0, maxTextureSize: "bad"}});
        compare(p.zoom, 2);
        compare(p.durationMs, 0);
        compare(p.columnSpan, 0);
        compare(p.maxTextureSize, 8192);
        compare(WallpaperMath.fileUrl("/tmp/中文 #%.png"), "file:///tmp/%E4%B8%AD%E6%96%87%20%23%25.png");
    }
}
