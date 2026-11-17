import QtQuick
import QtTest
import "../../CarouselMedia.js" as Media

TestCase {
    name: "CarouselMedia"
    function test_video_cards_apply_original_not_thumbnail() {
        var entries = Media.videos([{fileName: "视频 one.mp4", fileUrl: "file:///video/%E8%A7%86%E9%A2%91%20one.mp4",
            thumbnailUrl: "file:///cache/one.jpg", originalPath: "/video/视频 one.mp4"}]);
        compare(entries[0].fileUrl, "file:///cache/one.jpg");
        compare(entries[0].originalPath, "/video/视频 one.mp4");
        var fallback = Media.videos([{fileName: "two.mp4", fileUrl: "file:///video/two%20words.mp4"}]);
        compare(fallback[0].originalPath, "/video/two words.mp4");
    }
    function test_search_selection_uses_filtered_indices() {
        var entries = [{fileName: "Apple.png"}, {fileName: "Bee.png"}, {fileName: "Blue.png"}];
        var filtered = Media.filter(entries, "b");
        compare(filtered.length, 2);
        compare(Media.selectedIndex(filtered, "Blue.png", ""), 1);
        compare(Media.selectedIndex(filtered, "Apple.png", ""), 0);
        compare(Media.selectedIndex([], "", ""), -1);
        compare(Media.filter(entries, "").length, 3);
    }
    function test_page_direction_and_single_row_at_rest() {
        var downStart = Media.pageGeometry(0, -1, 180);
        var downMiddle = Media.pageGeometry(0.5, -1, 180);
        var downEnd = Media.pageGeometry(1, -1, 180);
        compare(downStart.outgoingY, 0);
        compare(downStart.incomingY, 180);
        compare(downMiddle.outgoingY, -90);
        compare(downMiddle.incomingY, 90);
        compare(downEnd.incomingY, 0);
        compare(downEnd.outgoingOpacity, 0);
        compare(downEnd.incomingOpacity, 1);
        var up = Media.pageGeometry(0.5, 1, 180);
        compare(up.outgoingY, 90);
        compare(up.incomingY, -90);
    }
}
