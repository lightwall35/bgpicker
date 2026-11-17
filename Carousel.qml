import QtQuick
import Qt.labs.folderlistmodel
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import QtQuick.Effects
import "CarouselMedia.js" as Media

Item {
    id: root

    // ── Shell integration interface ───────────────────────────────────────────
    // Wrapper provides these; Carousel is fully shell-agnostic.

    property string defaultWallpaperFolder: ""
    property string currentWallpaperPath: ""   // full path; used to highlight current item on open
    property var extraDirectories: []           // other directories to pre-scan/cache (Noctalia multi-monitor)
    property string wlrNamespace: "plugins:wallpaperCarousel"
    property var cfg: ({})                      // raw settings object from the host shell
    property var getFocusedScreen: () => null   // () => Screen | null
    property bool hasWallpaperConfigured: true
    property string shellSettingsHint: "Open your shell settings → Wallpaper to configure a wallpaper directory."
    property string wallpaperMode: ""
    property bool wallpaperModeBusy: false
    property bool wallpaperModeAvailable: true
    property string wallpaperTransition: "fade"
    property var wallpaperTransitionChoices: []
    property string wallpaperError: ""
    property var imageCatalog: []
    property bool useImageCatalog: false
    property var videoCatalog: []
    property string currentVideoPath: ""
    property string videoFolder: ""
    property bool videoPage: false
    property bool _constructed: false
    property real _wheelAccum: 0
    property bool _pageCapturePending: false
    property bool _pageAnimating: false
    property real _pageProgress: 1
    property int _pageDirection: -1
    property string _modelPage: "images"
    property var _pageGrab: null
    property var _pageSelections: ({images: "", videos: ""})
    readonly property int activeEntryCount: videoPage ? videoCatalog.length : (useImageCatalog ? imageCatalog.length : folderModel.count)
    readonly property string activeCurrentPath: videoPage ? currentVideoPath : currentWallpaperPath
    readonly property var activeEntries: videoPage ? Media.videos(videoCatalog) : (useImageCatalog ? Media.videos(imageCatalog) : (_folderCache[wallpaperFolder] || []))
    readonly property var _pageGeometry: Media.pageGeometry(_pageProgress, _pageDirection, 180)
    property bool _modelSyncPending: false
    property int _pageGeneration: 0
    onImageCatalogChanged: {
        if (!videoPage) _syncActiveModel();
        root._rebuildCacheEntries();
    }
    onVideoCatalogChanged: {
        if (videoPage) _syncActiveModel();
        root._rebuildCacheEntries();
    }
    onVideoPageChanged: {
        if (!_constructed) return;
        carousel.initialFocusSet = false;
        searchInput.text = "";
        carousel.searchVisible = false;
        // This is the intentional incoming row swap. External catalog refreshes
        // defer during the animation; this swap must happen before it starts.
        _syncActiveModel(true);
    }
    onCurrentVideoPathChanged: {
        if (videoPage) root._currentCacheIndex = root._findCurrentIndex();
        root._rebuildCacheEntries();
    }

    readonly property bool isOpen: overlay.visible
    signal opened()
    signal catalogRefreshRequested()
    signal wallpaperPicked(string fullPath, string screenName)
    signal wallpaperModeToggleRequested()
    signal wallpaperTransitionSelected(string type)

    readonly property var overlayScreen: overlay.screen  // wrappers can bind against this

    // ── Derived settings with defaults ────────────────────────────────────────
    readonly property string wallpaperFolder: {
        const ov = (cfg.wallpaperDirectory ?? "").trim();
        return ov || defaultWallpaperFolder;
    }

    readonly property string _carouselMode: cfg.carouselMode ?? "wrap"
    readonly property bool _isInfinite: _carouselMode === "infinite"
    readonly property bool _wrapsIndex: _carouselMode !== "standard"
    on_CarouselModeChanged: if (_initialSyncDone) Qt.callLater(_syncStableModel)

    readonly property var _currentView: (_isInfinite && !carousel.searchActive) ? pathView : listView
    readonly property string wallpaperFolderUrl: "file://" + wallpaperFolder

    // ── Folder cache ──────────────────────────────────────────────────────────
    property var _folderCache: ({})
    property bool _initialSyncDone: false
    property int _currentCacheIndex: -1
    property var _imageCacheEntries: []
    property var _videoCacheEntries: []

    readonly property var _nameFilters: [
        "*.jpg", "*.jpeg", "*.png", "*.webp", "*.gif",
        "*.bmp", "*.jxl", "*.avif", "*.heif", "*.exr"
    ]

    onCurrentWallpaperPathChanged: {
        if (!videoPage && folderModel.status === FolderListModel.Ready && folderModel.count > 0)
            root._currentCacheIndex = root._findCurrentIndex();
        if (folderModel.status === FolderListModel.Ready && folderModel.count > 0)
            root._rebuildCacheEntries();
    }

    onWallpaperFolderUrlChanged: {
        modelSyncTimer.stop();
        root._initialSyncDone = false;
        if (!root.videoPage) root._syncActiveModel();
    }

    ListModel { id: stableModel }
    ListModel { id: filteredModel }

    Timer {
        id: modelSyncTimer
        interval: 1500
        onTriggered: root._syncStableModel()
    }

    FolderListModel {
        id: folderModel
        folder: root.wallpaperFolderUrl
        nameFilters: root._nameFilters
        showDirs: false
        sortField: FolderListModel.Name

        onStatusChanged: {
            if (status === FolderListModel.Ready && !root._initialSyncDone) {
                if (!root.useImageCatalog) root._syncStableModel();
                root._initialSyncDone = true;
            }
        }
        onCountChanged: {
            if (root._initialSyncDone) {
                if (root.useImageCatalog) root.catalogRefreshRequested();
                else modelSyncTimer.restart();
            }
        }
    }

    FolderListModel {
        id: videoFolderModel
        folder: root.videoFolder !== "" ? "file://" + root.videoFolder : ""
        nameFilters: ["*.mp4", "*.mkv", "*.webm", "*.mov"]
        showDirs: false
        property bool _initialVideoSyncDone: false
        onStatusChanged: if (status === FolderListModel.Ready) _initialVideoSyncDone = true
        onCountChanged: if (_initialVideoSyncDone) root.catalogRefreshRequested()
    }

    function _readFolderModel(model) {
        var entries = [];
        for (var i = 0; i < model.count; i++)
            entries.push({
                fileName: model.get(i, "fileName"),
                fileUrl: model.get(i, "fileUrl").toString(),
                originalPath: model.get(i, "filePath")
            });
        return entries;
    }

    function _entriesForCurrentPage() {
        // Read the source properties directly: a property-changed handler can
        // run before QML has invalidated another binding derived from it.
        return root.videoPage ? Media.videos(root.videoCatalog)
            : (root.useImageCatalog ? Media.videos(root.imageCatalog) : (root._folderCache[root.wallpaperFolder] || []));
    }

    function _findCurrentIndex() {
        const name = ((root.videoPage ? root.currentVideoPath : root.currentWallpaperPath) || "").split('/').pop();
        const entries = root._entriesForCurrentPage();
        for (let i = 0; i < entries.length; i++) {
            if (entries[i].fileName === name) return i;
        }
        return 0;
    }

    function _setCacheList(key, urls) {
        const current = root[key];
        if (current.length === urls.length && current.every((u, i) => u === urls[i])) return;
        root[key] = urls;
    }

    function _cacheWindowUrls(entries, currentPath) {
        if (!entries || entries.length === 0) return [];
        const name = (currentPath || "").split('/').pop();
        let center = 0;
        for (let i = 0; i < entries.length; i++) {
            if (entries[i].fileName === name) {
                center = i;
                break;
            }
        }
        const radius = Math.floor(((root.cfg.cacheSize !== undefined) ? parseInt(root.cfg.cacheSize) : 200) / 2);
        return entries.slice(Math.max(0, center - radius), center + radius + 1).map(e => e.fileUrl);
    }

    function _rebuildCacheEntries() {
        const images = root.useImageCatalog ? Media.videos(root.imageCatalog) : (root._folderCache[root.wallpaperFolder] || []);
        root._setCacheList("_imageCacheEntries", _cacheWindowUrls(images, root.currentWallpaperPath));
        root._setCacheList("_videoCacheEntries", _cacheWindowUrls(Media.videos(root.videoCatalog), root.currentVideoPath));
    }

    function _populateStableModel(entries) {
        const activeView = root._currentView;
        const savedFile = activeView.currentItem ? activeView.currentItem.fileName : "";

        stableModel.clear();
        for (let i = 0; i < entries.length; i++)
            stableModel.append(entries[i]);

        if (root._isInfinite && entries.length > 0) {
            const viewWidth = pathView.width > 0 ? pathView.width : 2560;
            const minCount = Math.ceil(viewWidth / carousel.itemWidth) + 6;
            const baseCount = entries.length;
            const targetCount = baseCount * Math.ceil(minCount / baseCount);
            while (stableModel.count < targetCount) {
                for (let i = 0; i < baseCount && stableModel.count < targetCount; i++)
                    stableModel.append(entries[i]);
            }
        }

        let restoreIndex = entries.length ? 0 : -1;
        if (savedFile) {
            for (let i = 0; i < stableModel.count; i++) {
                if (stableModel.get(i).fileName === savedFile) {
                    restoreIndex = i;
                    break;
                }
            }
        }
        activeView.currentIndex = restoreIndex;
        // ListView batches model changes; flush them before reading count or
        // restoring a page's selected item, especially on a shorter new page.
        listView.forceLayout();
    }

    function _syncStableModel() {
        const entries = _readFolderModel(folderModel);
        const cache = Object.assign({}, root._folderCache);
        cache[root.wallpaperFolder] = entries;
        root._folderCache = cache;
        if (!root.videoPage) _syncActiveModel();
        else root._rebuildCacheEntries();
    }

    Component.onCompleted: {
        root._constructed = true;
        root._syncActiveModel();
    }

    function _syncActiveModel(force) {
        if (!root._constructed) return;
        if (!force && (root._pageAnimating || root._pageCapturePending)) {
            root._modelSyncPending = true;
            return;
        }
        root._modelSyncPending = false;
        const pageKey = root.videoPage ? "videos" : "images";
        const selected = root._currentView.currentItem;
        if (root._modelPage === pageKey && carousel.initialFocusSet && selected)
            root._pageSelections[pageKey] = selected.fileName;
        root._modelPage = pageKey;
        _populateStableModel(root._entriesForCurrentPage());
        carousel.initialFocusSet = false;
        root._currentCacheIndex = root._findCurrentIndex();
        root._rebuildCacheEntries();
        _applySearch(true);
        carousel.tryFocus();
    }

    function _flushPendingSync() {
        if (root._modelSyncPending && !root._pageAnimating && !root._pageCapturePending)
            root._syncActiveModel();
    }

    function _applySearch(preserveSelection) {
        const selectedFile = preserveSelection && listView.currentItem ? listView.currentItem.fileName : "";
        const entries = Media.filter(root._entriesForCurrentPage(), carousel._searchText);
        filteredModel.clear();
        for (let i = 0; i < entries.length; i++)
            filteredModel.append(entries[i]);
        if (filteredModel.count > 0 && carousel.searchActive) {
            const index = Math.max(0, Media.selectedIndex(entries, selectedFile, ""));
            listView.currentIndex = index;
            listView.positionViewAtIndex(index, ListView.Center);
        }
    }

    function requestMediaPage(page) {
        const wantVideo = page === "video";
        if (wantVideo === root.videoPage || root._pageAnimating || root._pageCapturePending
                || root.wallpaperModeBusy || !overlay.visible || pageCooldown.running) return false;
        root._pageCapturePending = true;
        const generation = ++root._pageGeneration;
        root._pageDirection = wantVideo ? -1 : 1;
        const current = root._currentView.currentItem;
        root._pageSelections[root.videoPage ? "videos" : "images"] = current ? current.fileName : "";
        const captured = carousel.grabToImage(result => {
            if (generation !== root._pageGeneration) return;
            root._pageCapturePending = false;
            if (!overlay.visible) { root._flushPendingSync(); return; }
            root._pageGrab = result;
            outgoingRow.source = result.url;
            root._pageProgress = 0;
            root._pageAnimating = true;
            root.videoPage = wantVideo;
            Qt.callLater(() => {
                root._currentView.forceActiveFocus();
                pageAnimation.restart();
            });
        });
        if (!captured) root._pageCapturePending = false;
        return captured;
    }

    Timer { id: pageCooldown; interval: 180 }
    NumberAnimation {
        id: pageAnimation
        target: root
        property: "_pageProgress"
        from: 0
        to: 1
        duration: 460
        easing.type: Easing.BezierSpline
        easing.bezierCurve: [0.2, 0, 0.18, 1, 1, 1]
        onFinished: {
            root._pageAnimating = false;
            outgoingRow.source = "";
            root._pageGrab = null;
            root._flushPendingSync();
            pageCooldown.restart();
            Qt.callLater(() => root._currentView.forceActiveFocus());
        }
    }

    // ── Public API ────────────────────────────────────────────────────────────
    function toggle() {
        if (overlay.visible) close(); else open();
    }

    function open() {
        root._pageSelections = {images: "", videos: ""};
        carousel.initialFocusSet = false;
        const focusedScreen = root.getFocusedScreen();
        if (focusedScreen)
            overlay.screen = focusedScreen;
        overlay.visible = true;
        searchInput.text = "";
        carousel.searchVisible = false;
        carousel.tryFocus();
        Qt.callLater(() => root._currentView.forceActiveFocus());
        root.opened();
    }

    function close() {
        overlay.visible = false;
        root._pageGeneration++;
        root._pageCapturePending = false;
        pageAnimation.stop();
        root._pageAnimating = false;
        root._pageProgress = 1;
        outgoingRow.source = "";
        root._pageGrab = null;
        root._flushPendingSync();
    }

    function cycle(direction: int): string {
        const v = root._currentView;
        if (!overlay.visible) {
            open();
            return "opened:" + v.currentIndex;
        }
        if (direction > 0) v.incrementCurrentIndex();
        else v.decrementCurrentIndex();
        return "index:" + v.currentIndex;
    }

    // ── IPC (Quickshell — available on both DMS and Noctalia) ─────────────────
    IpcHandler {
        target: "wallpaperCarousel"
        function toggle(): string   { root.toggle(); return overlay.visible ? "opened" : "closed"; }
        function open(): string     { if (!overlay.visible) root.open(); return "opened"; }
        function close(): string    { if (overlay.visible) root.close(); return "closed"; }
        function cycleNext(): string     { return root.cycle(+1); }
        function cyclePrevious(): string { return root.cycle(-1); }
        function showVideos(): string { return root.requestMediaPage("video") ? "switching" : "unchanged"; }
        function showImages(): string { return root.requestMediaPage("static") ? "switching" : "unchanged"; }
        function applySelection(): string {
            if (root._pageAnimating || root._pageCapturePending || root.wallpaperModeBusy || carousel.confirmingIndex >= 0)
                return "busy";
            if (!overlay.visible || !root._currentView.currentItem) return "unavailable";
            root._currentView.currentItem.pickWallpaper();
            return "applied";
        }
        function status(): string {
            return JSON.stringify({visible: overlay.visible, page: root.videoPage ? "video" : "static",
                changingPage: root._pageAnimating || root._pageCapturePending,
                count: root.activeEntryCount, index: root._currentView.currentIndex,
                selectedPath: root._currentView.currentItem ? root._currentView.currentItem.originalPath : ""});
        }
    }

    // ── Fullscreen overlay window ─────────────────────────────────────────────
    PanelWindow {
        id: overlay
        visible: false
        color: "transparent"

        WlrLayershell.namespace: root.wlrNamespace
        WlrLayershell.layer: WlrLayershell.Overlay
        WlrLayershell.exclusiveZone: -1
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive

        anchors { top: true; left: true; right: true; bottom: true }

        Rectangle {
            anchors.fill: parent
            color: "#CC000000"
            opacity: overlay.visible ? carousel.overlayOpacity / 100 : 0
            Behavior on opacity { NumberAnimation { duration: 150 } }
        }

        MouseArea {
            anchors.fill: parent
            enabled: overlay.visible && carousel.confirmingIndex < 0
            onClicked: root.close()
        }

        WheelHandler {
            target: null
            acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
            orientation: Qt.Vertical
            onWheel: event => {
                const amount = event.angleDelta.y || event.pixelDelta.y;
                if (root._pageAnimating || root._pageCapturePending || pageCooldown.running) root._wheelAccum = 0;
                else {
                    if (amount * root._wheelAccum < 0) root._wheelAccum = 0;
                    root._wheelAccum += amount;
                    if (Math.abs(root._wheelAccum) >= 40) {
                        root.requestMediaPage(root._wheelAccum < 0 ? "video" : "static");
                        root._wheelAccum = 0;
                    }
                }
                event.accepted = true;
            }
        }

        // ── Carousel ──────────────────────────────────────────────────────────
        Item {
            id: carousel
            anchors.fill: parent
            opacity: overlay.visible ? (root._pageAnimating ? root._pageGeometry.incomingOpacity : 1) : 0
            transform: Translate { y: root._pageAnimating ? root._pageGeometry.incomingY : 0 }
            enabled: !root._pageAnimating && !root._pageCapturePending

            property bool initialFocusSet: false
            property string _searchText: ""
            readonly property bool searchActive: _searchText.length > 0
            property bool searchVisible: false

            function tryFocus() {
                if (initialFocusSet) return;

                const model = carousel.searchActive ? filteredModel : stableModel;
                const entries = [];
                for (let i = 0; i < model.count; i++) entries.push({fileName: model.get(i).fileName});
                const targetIndex = Math.max(0, Media.selectedIndex(entries,
                    root._pageSelections[root.videoPage ? "videos" : "images"],
                    root.videoPage ? root.currentVideoPath : root.currentWallpaperPath));

                const v = root._currentView;
                if (v.count > 0) {
                    const safeIndex = Math.min(targetIndex, v.count - 1);
                    v.currentIndex = safeIndex;
                    if (!root._isInfinite)
                        v.positionViewAtIndex(safeIndex, ListView.Center);
                    initialFocusSet = true;
                }

                carousel.heldIndex = -1;
                holdTimer.start();
            }

            readonly property int itemWidth:    parseInt(root.cfg.itemWidth)  || 300
            readonly property int itemHeight:   parseInt(root.cfg.itemHeight) || 420
            readonly property size thumbnailSourceSize: Qt.size(1280, Math.ceil(carousel.itemHeight * 1.6))
            readonly property int borderWidth:  (root.cfg.borderWidth  !== undefined) ? parseInt(root.cfg.borderWidth)  : 3
            readonly property int overlayOpacity: (root.cfg.overlayOpacity !== undefined) ? parseInt(root.cfg.overlayOpacity) : 80
            readonly property int cornerRadius: (root.cfg.cornerRadius !== undefined) ? parseInt(root.cfg.cornerRadius) : 0
            readonly property bool enableRounding: cornerRadius > 0
            readonly property real skewFactor: -0.35
            readonly property int _baseWallpaperCount: root.activeEntryCount

            readonly property real selectedScale: {
                let val = (root.cfg.selectedScale !== undefined) ? parseFloat(root.cfg.selectedScale) : 108;
                return val > 10 ? val / 100.0 : val;
            }
            readonly property bool expandSelected: !!(root.cfg.expandSelected === true || root.cfg.expandSelected === "true")
            readonly property real expandMultiplier: ((root.cfg.expandMultiplier !== undefined) ? parseInt(root.cfg.expandMultiplier) : 120) / 100.0
            readonly property bool enableHoldExpand: !!(root.cfg.enableHoldExpand === true || root.cfg.enableHoldExpand === "true")
            readonly property real holdExpandRatio: ((root.cfg.holdExpandRatio !== undefined) ? parseFloat(root.cfg.holdExpandRatio) : 35.0) / 100.0
            readonly property int holdDelay: (root.cfg.holdDelay !== undefined) ? parseInt(root.cfg.holdDelay) : 1500

            property int heldIndex: -1

            Timer {
                id: holdTimer
                interval: carousel.holdDelay
                onTriggered: {
                    if (carousel.enableHoldExpand)
                        carousel.heldIndex = root._currentView.currentIndex;
                }
            }

            property int confirmingIndex: -1
            property string _pendingPickPath: ""
            property string _pendingPickScreen: ""

            function confirmPick(idx, path) {
                confirmingIndex = idx;
                _pendingPickPath = path || "";
                _pendingPickScreen = overlay.screen ? overlay.screen.name : "";
                confirmTimer.start();
            }

            Timer {
                id: confirmTimer
                interval: 300
                onTriggered: {
                    carousel.confirmingIndex = -1;
                    const pickedPath = carousel._pendingPickPath;
                    const pickedScreen = carousel._pendingPickScreen;
                    carousel._pendingPickPath = "";
                    carousel._pendingPickScreen = "";
                    root.close();
                    if (pickedPath)
                        root.wallpaperPicked(pickedPath, pickedScreen);
                }
            }

            // ── Shared delegate ───────────────────────────────────────────────
            Component {
                id: carouselDelegate

                Item {
                    id: delegateRoot
                    width: carousel.itemWidth
                    height: carousel.itemHeight
                    anchors.verticalCenter: parent ? parent.verticalCenter : undefined

                    required property int index
                    required property string fileName
                    required property string fileUrl
                    required property string originalPath

                    readonly property bool _effectivelyInfinite: root._isInfinite && !carousel.searchActive

                    readonly property bool isCurrent: _effectivelyInfinite ? PathView.isCurrentItem : ListView.isCurrentItem

                    readonly property int distFromCenter: {
                        if (_effectivelyInfinite) {
                            const n = stableModel.count;
                            if (n <= 1) return 0;
                            const d = Math.abs(index - pathView.currentIndex);
                            return Math.min(d, n - d);
                        }
                        return Math.abs(index - listView.currentIndex);
                    }

                    readonly property real falloff: 1.0 / (1.0 + distFromCenter * distFromCenter)

                    readonly property real _dupeFade: {
                        if (!_effectivelyInfinite) return 1.0;
                        const base = carousel._baseWallpaperCount;
                        if (base <= 0 || base >= stableModel.count) return 1.0;
                        const n = stableModel.count;
                        const cur = pathView.currentIndex;
                        const wpOffset = ((index % base) - (cur % base) + base) % base;
                        const leftCount  = Math.floor(base / 2);
                        const rightCount = Math.floor((base - 1) / 2);
                        let target;
                        if (wpOffset === 0)
                            target = cur;
                        else if (wpOffset <= rightCount)
                            target = (cur + wpOffset) % n;
                        else if (base - wpOffset <= leftCount)
                            target = (cur - (base - wpOffset) + n) % n;
                        else
                            return 0.0;
                        return index === target ? 1.0 : 0.0;
                    }

                    z: carousel.confirmingIndex === index ? 100 : isCurrent ? 10 : Math.max(1, 10 - distFromCenter)

                    function pickWallpaper() {
                        if (carousel.confirmingIndex >= 0 || root._pageAnimating || root.wallpaperModeBusy) return;
                        carousel.confirmPick(index, originalPath);
                    }

                    MouseArea {
                        id: delegateMouseArea
                        x: carousel.skewFactor * carousel.itemHeight / 2
                        width: parent.width
                        height: parent.height
                        hoverEnabled: true
                        onClicked: delegateRoot.pickWallpaper()
                    }

                    readonly property real extraSpace:     carousel.itemWidth * carousel.expandMultiplier - carousel.itemWidth
                    readonly property real heldExtraSpace: carousel.width * carousel.holdExpandRatio - carousel.itemWidth

                    readonly property real visualXOffset: {
                        if (!carousel.expandSelected && carousel.heldIndex < 0) return 0;
                        if (carousel.heldIndex === index) return 0;
                        if (distFromCenter === 0) return 0;
                        const space = (carousel.heldIndex >= 0) ? heldExtraSpace : extraSpace;
                        let signedDist = 0;
                        if (_effectivelyInfinite) {
                            let d = index - pathView.currentIndex;
                            const h = stableModel.count / 2;
                            if (d > h) d -= stableModel.count;
                            else if (d < -h) d += stableModel.count;
                            signedDist = d;
                        } else {
                            signedDist = index - listView.currentIndex;
                        }
                        return signedDist > 0 ? (space / 2) : -(space / 2);
                    }

                    Item {
                        anchors.verticalCenter: parent.verticalCenter
                        anchors.horizontalCenter: parent.horizontalCenter
                        anchors.horizontalCenterOffset: visualXOffset

                        readonly property bool isConfirmed: carousel.confirmingIndex === delegateRoot.index
                        readonly property bool isHeld: carousel.heldIndex === delegateRoot.index

                        width:  isHeld ? (carousel.width  * carousel.holdExpandRatio) : ((carousel.expandSelected && isCurrent) ? (carousel.itemWidth * carousel.expandMultiplier) : carousel.itemWidth)
                        height: isHeld ? (carousel.height * carousel.holdExpandRatio) : parent.height

                        Behavior on anchors.horizontalCenterOffset { NumberAnimation { duration: 400; easing.type: Easing.InOutCubic } }
                        Behavior on width  { NumberAnimation { duration: 400; easing.type: Easing.InOutCubic } }
                        Behavior on height { NumberAnimation { duration: 400; easing.type: Easing.InOutCubic } }

                        readonly property bool isOtherConfirming: carousel.confirmingIndex >= 0 && !isConfirmed
                        readonly property bool isHovered: delegateMouseArea.containsMouse && carousel.confirmingIndex < 0

                        readonly property real baseScale:  0.75
                        readonly property real scaleRange: carousel.selectedScale - 0.75

                        scale:   isConfirmed ? 1.6 : isOtherConfirming ? (baseScale + scaleRange * delegateRoot.falloff) * 0.8 : isHovered ? baseScale + (scaleRange + 0.20) * delegateRoot.falloff : baseScale + scaleRange * delegateRoot.falloff
                        opacity: (isConfirmed ? 0.0 : isOtherConfirming ? 0.0 : isHovered ? 1.0 : 0.1 + 0.9 * delegateRoot.falloff) * delegateRoot._dupeFade
                        layer.enabled: overlay.visible && opacity < 1 && opacity > 0 && !isConfirmed

                        Behavior on scale   { NumberAnimation { duration: 300; easing.type: Easing.OutBack } }
                        Behavior on opacity { NumberAnimation { duration: 300 } }

                        transform: Matrix4x4 {
                            property real s: carousel.skewFactor
                            matrix: Qt.matrix4x4(1, s, 0, 0,  0, 1, 0, 0,  0, 0, 1, 0,  0, 0, 0, 1)
                        }

                        Item {
                            anchors.fill: parent

                            Rectangle {
                                id: cornerMask
                                width: parent.width; height: parent.height
                                radius: carousel.cornerRadius
                                visible: false
                                layer.enabled: overlay.visible && carousel.enableRounding
                            }

                            layer.enabled: overlay.visible && carousel.enableRounding
                            layer.effect: MultiEffect {
                                maskEnabled: true
                                maskSource: cornerMask
                                maskSpreadAtMin: 1.0
                                maskThresholdMin: 0.5
                            }

                            Rectangle {
                                anchors.fill: parent
                                radius: carousel.cornerRadius
                                color: "#33FFFFFF"
                                visible: innerImage.status === Image.Ready
                            }

                            Item {
                                anchors.fill: parent
                                anchors.margins: carousel.borderWidth
                                visible: innerImage.status === Image.Ready

                                Rectangle { anchors.fill: parent; color: "black" }
                                clip: true

                                Image {
                                    id: innerImage
                                    anchors.centerIn: parent
                                    anchors.horizontalCenterOffset: -50
                                    width: parent.width + (parent.height * Math.abs(carousel.skewFactor)) + 50
                                    height: parent.height
                                    fillMode: Image.PreserveAspectCrop
                                    source: delegateRoot.fileUrl
                                    sourceSize: carousel.thumbnailSourceSize
                                    asynchronous: true

                                    transform: Matrix4x4 {
                                        property real s: -carousel.skewFactor
                                        matrix: Qt.matrix4x4(1, s, 0, 0,  0, 1, 0, 0,  0, 0, 1, 0,  0, 0, 0, 1)
                                    }
                                }

                                // High-res overlay — loads asynchronously when held, cross-fades in over the thumbnail
                                Image {
                                    id: hiResImage
                                    anchors.centerIn: parent
                                    anchors.horizontalCenterOffset: -50
                                    width: parent.width + (parent.height * Math.abs(carousel.skewFactor)) + 50
                                    height: parent.height
                                    fillMode: Image.PreserveAspectCrop
                                    cache: false
                                    source: (overlay.visible && carousel.heldIndex === delegateRoot.index)
                                        ? (!root.videoPage && delegateRoot.originalPath !== ""
                                            ? "file://" + delegateRoot.originalPath.split("/").map(encodeURIComponent).join("/")
                                            : delegateRoot.fileUrl)
                                        : ""
                                    sourceSize: Qt.size(
                                        Math.max(carousel.thumbnailSourceSize.width, Math.ceil(carousel.width * carousel.holdExpandRatio * 2)),
                                        Math.max(carousel.thumbnailSourceSize.height, Math.ceil(carousel.height * carousel.holdExpandRatio * 2))
                                    )
                                    asynchronous: true
                                    opacity: (status === Image.Ready && carousel.heldIndex === delegateRoot.index) ? 1.0 : 0.0
                                    visible: opacity > 0

                                    Behavior on opacity { NumberAnimation { duration: 300; easing.type: Easing.InOutQuad } }

                                    transform: Matrix4x4 {
                                        property real s: -carousel.skewFactor
                                        matrix: Qt.matrix4x4(1, s, 0, 0,  0, 1, 0, 0,  0, 0, 1, 0,  0, 0, 0, 1)
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── PathView — looping ────────────────────────────────────────────
            PathView {
                id: pathView
                anchors.fill: parent
                visible: root._isInfinite && !carousel.searchActive

                model: (root._isInfinite && !carousel.searchActive) ? stableModel : null
                delegate: carouselDelegate

                pathItemCount: Math.max(1, Math.min(stableModel.count, Math.ceil(width / carousel.itemWidth) + 4))
                cacheItemCount: 4

                preferredHighlightBegin: 0.5
                preferredHighlightEnd: 0.5
                highlightRangeMode: PathView.StrictlyEnforceRange

                highlightMoveDuration: carousel.initialFocusSet ? 150 : 0
                movementDirection: PathView.Shortest

                focus: root._isInfinite && !carousel.searchVisible && overlay.visible

                Keys.onPressed: event => {
                    if (carousel.confirmingIndex >= 0) { event.accepted = true; return; }
                    if (event.key === Qt.Key_Escape) {
                        root.close(); event.accepted = true;
                    } else if (event.key === Qt.Key_F && (event.modifiers & Qt.ControlModifier)) {
                        carousel.searchVisible = true;
                        searchInput.forceActiveFocus();
                        event.accepted = true;
                    } else if (event.key === Qt.Key_Left || event.key === Qt.Key_H) {
                        decrementCurrentIndex(); event.accepted = true;
                    } else if (event.key === Qt.Key_Right || event.key === Qt.Key_L) {
                        incrementCurrentIndex(); event.accepted = true;
                    } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                        if (currentItem) currentItem.pickWallpaper();
                        event.accepted = true;
                    }
                }

                onCountChanged: carousel.tryFocus()
                onCurrentIndexChanged: {
                    carousel.heldIndex = -1;
                    holdTimer.restart();
                }

                readonly property real _pathLen: pathItemCount * carousel.itemWidth
                readonly property real _pathX0:  (width - _pathLen) / 2
                path: Path {
                    startX: pathView._pathX0
                    startY: pathView.height / 2 - carousel.itemHeight / 2
                    PathLine {
                        x: pathView._pathX0 + pathView._pathLen
                        y: pathView.height / 2 - carousel.itemHeight / 2
                    }
                }
            }

            // ── ListView — wrap / standard ────────────────────────────────────
            ListView {
                id: listView
                anchors.fill: parent
                visible: !root._isInfinite || carousel.searchActive

                model: carousel.searchActive ? filteredModel : (root._isInfinite ? null : stableModel)
                delegate: carouselDelegate

                spacing: 0
                orientation: ListView.Horizontal
                clip: false
                cacheBuffer: overlay.visible ? 1200 : 0

                highlightRangeMode: ListView.StrictlyEnforceRange
                preferredHighlightBegin: (width / 2) - (carousel.itemWidth / 2)
                preferredHighlightEnd:   (width / 2) + (carousel.itemWidth / 2)

                highlightMoveDuration: carousel.initialFocusSet ? 150 : 0

                focus: !root._isInfinite && !carousel.searchVisible && overlay.visible

                Keys.onPressed: event => {
                    if (carousel.confirmingIndex >= 0) { event.accepted = true; return; }
                    if (event.key === Qt.Key_Escape) {
                        root.close(); event.accepted = true;
                    } else if (event.key === Qt.Key_F && (event.modifiers & Qt.ControlModifier)) {
                        carousel.searchVisible = true;
                        searchInput.forceActiveFocus();
                        event.accepted = true;
                    } else if (event.key === Qt.Key_Left || event.key === Qt.Key_H) {
                        if (currentIndex > 0) decrementCurrentIndex();
                        else if (root._wrapsIndex) currentIndex = count - 1;
                        event.accepted = true;
                    } else if (event.key === Qt.Key_Right || event.key === Qt.Key_L) {
                        if (currentIndex < count - 1) incrementCurrentIndex();
                        else if (root._wrapsIndex) currentIndex = 0;
                        event.accepted = true;
                    } else if (event.key === Qt.Key_Home) {
                        currentIndex = 0; event.accepted = true;
                    } else if (event.key === Qt.Key_End) {
                        currentIndex = count - 1; event.accepted = true;
                    } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                        if (currentItem) currentItem.pickWallpaper();
                        event.accepted = true;
                    }
                }

                onCountChanged: carousel.tryFocus()
                onCurrentIndexChanged: {
                    carousel.heldIndex = -1;
                    holdTimer.restart();
                }
            }
        }

        Image {
            id: outgoingRow
            anchors.fill: parent
            visible: root._pageAnimating
            opacity: root._pageGeometry.outgoingOpacity
            transform: Translate { y: root._pageGeometry.outgoingY }
            fillMode: Image.Stretch
            z: 100
        }

        // Shared skewed controls; stack the two groups on narrow displays.
        Item {
            id: wallpaperControls
            visible: root.wallpaperMode !== ""
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            anchors.bottomMargin: Math.max(32, Math.min(88, overlay.height * 0.08 - 16))
            width: Math.min(overlay.width - 64, modeSelector.width + 40 + transitionSelector.width)
            readonly property bool stacked: width < modeSelector.width + 40 + transitionSelector.width
            height: stacked ? modeSelector.height + 18 + transitionSelector.height
                : Math.max(modeSelector.height, transitionSelector.height)
            z: 210

            SkewSelector {
                id: modeSelector
                x: wallpaperControls.stacked ? (parent.width - width) / 2 : 0
                choices: [{value: "awww", label: "普通", icon: "frame"},
                    {value: "parallax", label: "视差", icon: "layers"}]
                value: root.wallpaperMode
                skew: carousel.skewFactor
                busy: root.wallpaperModeBusy
                available: root.wallpaperModeAvailable
                onChosen: root.wallpaperModeToggleRequested()
            }

            SkewSelector {
                id: transitionSelector
                visible: root.wallpaperTransitionChoices.length > 0
                x: wallpaperControls.stacked ? (parent.width - width) / 2 : modeSelector.width + 40
                y: wallpaperControls.stacked ? modeSelector.height + 18 : 0
                choices: root.wallpaperTransitionChoices
                value: root.wallpaperTransition
                buttonWidth: Math.min(80, (overlay.width - 96 - 6 * 8) / 7)
                gap: 8
                skew: carousel.skewFactor
                busy: root.wallpaperModeBusy
                onChosen: type => root.wallpaperTransitionSelected(type)
            }
        }

        Text {
            visible: root.wallpaperError !== ""
            anchors.bottom: parent.bottom
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottomMargin: 36
            width: Math.min(parent.width - 48, 800)
            text: root.wallpaperError
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
            color: "#FFB4AB"
            font.pixelSize: 14
            z: 210
        }

        // ── Search bar ────────────────────────────────────────────────────────
        Item {
            id: searchBar
            visible: carousel.searchVisible
            anchors.top: parent.top
            anchors.topMargin: 24
            anchors.horizontalCenter: parent.horizontalCenter
            width: searchBox.width
            height: searchBox.height
            z: 200

            MouseArea {
                anchors.fill: parent
            }

            Rectangle {
                id: searchBox
                width: 360
                height: 40
                radius: 8
                color: "#BB000000"
                border.color: searchInput.activeFocus ? "#AAFFFFFF" : "#44FFFFFF"
                border.width: 1

                Behavior on border.color { ColorAnimation { duration: 150 } }

                Text {
                    anchors.fill: parent
                    anchors.leftMargin: 12
                    anchors.rightMargin: 12
                    verticalAlignment: Text.AlignVCenter
                    text: "Search wallpapers..."
                    color: "#55FFFFFF"
                    font.pixelSize: 15
                    visible: searchInput.text.length === 0
                }

                TextInput {
                    id: searchInput
                    anchors.fill: parent
                    anchors.leftMargin: 12
                    anchors.rightMargin: 12
                    verticalAlignment: TextInput.AlignVCenter
                    color: "white"
                    font.pixelSize: 15
                    clip: true

                    onTextChanged: {
                        carousel._searchText = text;
                        root._applySearch();
                    }

                    Keys.onPressed: event => {
                        if (event.key === Qt.Key_Escape) {
                            if (text.length > 0) {
                                text = "";
                            } else {
                                carousel.searchVisible = false;
                                root._currentView.forceActiveFocus();
                            }
                            event.accepted = true;
                        } else if (event.key === Qt.Key_Left) {
                            root._currentView.decrementCurrentIndex();
                            event.accepted = true;
                        } else if (event.key === Qt.Key_Right) {
                            root._currentView.incrementCurrentIndex();
                            event.accepted = true;
                        } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                            const v = root._currentView;
                            if (v.currentItem) v.currentItem.pickWallpaper();
                            event.accepted = true;
                        }
                    }
                }
            }
        }

        // ── Invalid override directory ─────────────────────────────────────────
        Column {
            anchors.centerIn: parent
            spacing: 12
            visible: !root.videoPage && overlay.visible && (root.cfg.wallpaperDirectory ?? "").trim() && folderModel.status !== FolderListModel.Ready

            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: "Directory not found"
                font.pixelSize: 24; font.bold: true; color: "white"
            }
            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: "The configured directory '" + (root.cfg.wallpaperDirectory ?? "") + "' does not exist.\nCheck the path in Wallpaper Carousel settings."
                font.pixelSize: 14; color: "#BBBBBB"; horizontalAlignment: Text.AlignHCenter; lineHeight: 1.4
            }
            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: "Press Escape to close"
                font.pixelSize: 12; color: "#888888"
            }
        }

        // ── Empty folder ──────────────────────────────────────────────────────
        Column {
            anchors.centerIn: parent
            spacing: 12
            visible: overlay.visible && root.activeEntryCount === 0 && (root.videoPage || folderModel.status === FolderListModel.Ready)

            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: root.videoPage ? "动态壁纸文件夹为空" : root.hasWallpaperConfigured ? "No images found in wallpaper folder" : "No wallpaper configured"
                font.pixelSize: 24; font.bold: true; color: "white"
            }
            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: root.videoPage ? "将视频放入 " + root.videoFolder : root.hasWallpaperConfigured
                    ? "The folder '" + root.wallpaperFolder + "' is empty.\nAdd images or choose a different wallpaper directory."
                    : root.shellSettingsHint
                font.pixelSize: 14; color: "#BBBBBB"; horizontalAlignment: Text.AlignHCenter; lineHeight: 1.4
            }
            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: "Press Escape to close"
                font.pixelSize: 12; color: "#888888"
            }
        }
    }

    // ── Pre-cache window ──────────────────────────────────────────────────────
    PanelWindow {
        id: cacheWindow
        visible: true
        color: "transparent"
        implicitWidth: 1; implicitHeight: 1

        WlrLayershell.namespace: root.wlrNamespace + ":precache"
        WlrLayershell.layer: WlrLayershell.Background
        WlrLayershell.exclusiveZone: 0
        anchors { top: true; left: true }

        Item {
            width: 1; height: 1
            clip: true

            Repeater {
                model: root._imageCacheEntries
                Image {
                    width: carousel.itemWidth; height: carousel.itemHeight
                    asynchronous: true
                    source: modelData
                    sourceSize: carousel.thumbnailSourceSize
                    fillMode: Image.PreserveAspectCrop
                }
            }

            Repeater {
                model: root._videoCacheEntries
                Image {
                    width: carousel.itemWidth; height: carousel.itemHeight
                    asynchronous: true
                    source: modelData
                    sourceSize: carousel.thumbnailSourceSize
                    fillMode: Image.PreserveAspectCrop
                }
            }

            // Pre-scan and cache other monitor directories (provided by wrapper)
            Repeater {
                model: root.extraDirectories
                Item {
                    property string dir: modelData
                    FolderListModel {
                        id: extraFolderModel
                        folder: "file://" + dir
                        nameFilters: root._nameFilters
                        showDirs: false
                        sortField: FolderListModel.Name
                        onStatusChanged: {
                            if (status === FolderListModel.Ready)
                                root._folderCache[dir] = root._readFolderModel(extraFolderModel);
                        }
                        onCountChanged: {
                            if (status === FolderListModel.Ready)
                                root._folderCache[dir] = root._readFolderModel(extraFolderModel);
                        }
                    }
                    Repeater {
                        model: extraFolderModel
                        Image {
                            width: carousel.itemWidth; height: carousel.itemHeight
                            asynchronous: true
                            source: fileUrl
                            sourceSize: carousel.thumbnailSourceSize
                            fillMode: Image.PreserveAspectCrop
                        }
                    }
                }
            }
        }
    }
}
