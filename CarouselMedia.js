// Catalog and page geometry shared with offline QML tests.
function videos(entries) {
    return (entries || []).map(function(entry) {
        var source = entry.originalPath || decodeURIComponent(String(entry.fileUrl || "").replace(/^file:\/\//, ""));
        return {fileName: entry.fileName, fileUrl: entry.thumbnailUrl || entry.fileUrl || "", originalPath: source};
    });
}

function filter(entries, text) {
    var needle = String(text || "").toLowerCase();
    return needle ? entries.filter(e => e.fileName.toLowerCase().indexOf(needle) !== -1) : entries;
}

function selectedIndex(entries, preferredName, currentPath) {
    var name = preferredName || String(currentPath || "").split('/').pop();
    for (var i = 0; i < entries.length; i++) {
        if (entries[i].fileName === name) return i;
    }
    return entries.length ? 0 : -1;
}

function pageGeometry(progress, direction, travel) {
    var t = Math.max(0, Math.min(1, progress));
    return {outgoingY: direction * travel * t, incomingY: -direction * travel * (1 - t),
        outgoingOpacity: 1 - t, incomingOpacity: t};
}
