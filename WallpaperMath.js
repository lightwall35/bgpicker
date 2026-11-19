// Geometry and niri event reduction, shared by the renderer and offline tests.
function clamp(value, low, high) {
    return Math.max(low, Math.min(high, value));
}

function settings(value) {
    var p = value && value.parallax ? value.parallax : {};
    function number(key, fallback, low, high) {
        var value = Number(p[key]);
        return clamp(p[key] === undefined || !isFinite(value) ? fallback : value, low, high);
    }
    return {
        zoom: number("zoom", 1.12, 1, 2),
        columnSpan: Math.round(number("columnSpan", 6, 0, 100)),
        durationMs: Math.round(number("durationMs", 350, 0, 3000)),
        modeTransitionMs: Math.round(number("modeTransitionMs", 500, 0, 3000)),
        transitionMs: Math.round(number("transitionMs", 350, 0, 3000)),
        maxTextureSize: Math.round(number("maxTextureSize", 8192, 1024, 16384))
    };
}

function modeGeometry(width, height, imageWidth, imageHeight, zoom, progress, blend) {
    var normal = geometry(width, height, imageWidth, imageHeight, 1);
    var parallax = geometry(width, height, imageWidth, imageHeight, zoom);
    var t = clamp(Number(blend) || 0, 0, 1);
    function mix(a, b) { return a + (b - a) * t; }
    return {x: mix(-normal.overflowX * 0.5, -parallax.overflowX * clamp(progress, 0, 1)),
        y: mix(normal.y, parallax.y), width: mix(normal.width, parallax.width),
        height: mix(normal.height, parallax.height)};
}

function fileUrl(path) {
    return "file://" + String(path).split("/").map(encodeURIComponent).join("/");
}

function geometry(width, height, imageWidth, imageHeight, zoom) {
    var w = Math.max(1, width), h = Math.max(1, height);
    var aspect = imageWidth > 0 && imageHeight > 0 ? imageWidth / imageHeight : w / h;
    // Wide panoramas already have room to travel; ordinary images need overscan.
    var factor = aspect > (w / h) * 1.1 ? 1 : zoom;
    var canvasWidth = Math.max(w, h * aspect) * factor;
    var canvasHeight = Math.max(h, w / aspect) * factor;
    return {width: canvasWidth, height: canvasHeight,
        overflowX: Math.max(0, canvasWidth - w), y: (h - canvasHeight) / 2};
}

function initialState() {
    return {windows: [], workspaces: [], lastColumns: {}, overviewOpen: false};
}

function column(window) {
    var pos = window && window.layout && window.layout.pos_in_scrolling_layout;
    return window && !window.is_floating && pos && pos.length > 0 ? Number(pos[0]) : null;
}

function reduce(state, event) {
    var windows = state.windows, workspaces = state.workspaces;
    var overviewOpen = !!state.overviewOpen;
    if (event.OverviewOpenedOrClosed) {
        return {
            windows: windows,
            workspaces: workspaces,
            lastColumns: state.lastColumns,
            overviewOpen: !!event.OverviewOpenedOrClosed.is_open
        };
    } else if (event.WorkspacesChanged) {
        workspaces = event.WorkspacesChanged.workspaces;
    } else if (event.WorkspaceActivated) {
        var activation = event.WorkspaceActivated;
        var selected = workspaces.find(function(w) { return w.id === activation.id; });
        workspaces = workspaces.map(function(w) {
            return Object.assign({}, w, {
                is_active: selected && w.output === selected.output ? w.id === activation.id : w.is_active,
                is_focused: activation.focused ? w.id === activation.id : w.is_focused
            });
        });
    } else if (event.WorkspaceActiveWindowChanged) {
        var active = event.WorkspaceActiveWindowChanged;
        workspaces = workspaces.map(function(w) {
            return w.id === active.workspace_id ? Object.assign({}, w, {active_window_id: active.active_window_id}) : w;
        });
    } else if (event.WindowsChanged) {
        windows = event.WindowsChanged.windows;
    } else if (event.WindowOpenedOrChanged) {
        var changed = event.WindowOpenedOrChanged.window;
        windows = windows.filter(function(w) { return w.id !== changed.id; }).map(function(w) {
            return changed.is_focused ? Object.assign({}, w, {is_focused: false}) : w;
        });
        windows.push(changed);
    } else if (event.WindowClosed) {
        windows = windows.filter(function(w) { return w.id !== event.WindowClosed.id; });
    } else if (event.WindowFocusChanged) {
        windows = windows.map(function(w) {
            return Object.assign({}, w, {is_focused: w.id === event.WindowFocusChanged.id});
        });
    } else if (event.WindowLayoutsChanged) {
        windows = windows.map(function(w) {
            var change = event.WindowLayoutsChanged.changes.find(function(pair) { return pair[0] === w.id; });
            return change ? Object.assign({}, w, {layout: change[1]}) : w;
        });
    } else {
        return state;
    }
    var memory = {};
    workspaces.forEach(function(w) {
        if (state.lastColumns[w.id] !== undefined) memory[w.id] = state.lastColumns[w.id];
    });
    windows.forEach(function(w) {
        if (w.is_focused && column(w) !== null && workspaces.some(function(ws) { return ws.id === w.workspace_id; }))
            memory[w.workspace_id] = column(w);
    });
    return {windows: windows, workspaces: workspaces, lastColumns: memory, overviewOpen: overviewOpen};
}

function progress(state, output, columnSpan) {
    var workspace = state.workspaces.find(function(w) { return w.output === output && w.is_active; });
    if (!workspace) return 0.5;
    var windows = state.windows.filter(function(w) { return w.workspace_id === workspace.id; });
    var columns = [];
    windows.forEach(function(w) {
        var c = column(w);
        if (c !== null && columns.indexOf(c) < 0) columns.push(c);
    });
    columns.sort(function(a, b) { return a - b; });
    if (!columns.length) return 0.5;
    var active = windows.find(function(w) { return w.id === workspace.active_window_id; });
    var preferred = column(active);
    if (preferred === null) preferred = state.lastColumns[workspace.id];
    var nearest = columns[0];
    columns.forEach(function(c) {
        if (Math.abs(c - preferred) < Math.abs(nearest - preferred)) nearest = c;
    });
    var span = columnSpan > 1 ? columnSpan : columns.length;
    if (span <= 1) return 0.5;
    return clamp(columns.indexOf(nearest) / (span - 1), 0, 1);
}
