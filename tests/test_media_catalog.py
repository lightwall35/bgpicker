import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

MODULE = Path(__file__).resolve().parents[1] / 'media_catalog.py'
spec = importlib.util.spec_from_file_location('media_catalog', MODULE)
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)


class MediaCatalogTests(unittest.TestCase):
    def test_sorted_video_cards_keep_original_and_encoded_preview_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            for name in ['B.MP4', 'a.webm', '雪 #100%.mov', 'ignore.png', 'notes.txt']:
                (directory / name).write_bytes(b'file')
            (directory / 'directory.mkv').mkdir()
            preview = directory / '预览 #1.jpg'
            with patch.object(catalog, 'prepare_thumbnail', return_value=str(preview)) as thumbnails:
                entries = catalog.build_catalog(directory)
            self.assertEqual([entry['fileName'] for entry in entries], ['a.webm', 'B.MP4', '雪 #100%.mov'])
            self.assertEqual(thumbnails.call_count, 3)
            card = entries[-1]
            self.assertEqual(card['originalPath'], str(directory / '雪 #100%.mov'))
            self.assertEqual(card['fileUrl'], (directory / '雪 #100%.mov').as_uri())
            self.assertEqual(card['thumbnailUrl'], preview.as_uri())
            self.assertIn('%23', card['fileUrl'])
            self.assertIn('%25', card['fileUrl'])
            self.assertNotIn('thumbnailError', card)

    def test_thumbnail_failure_does_not_hide_video_or_stop_other_cards(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            for name in ['broken.mp4', 'good.mkv']:
                (directory / name).write_bytes(b'video')
            def prepare(source):
                if source.name == 'broken.mp4':
                    raise subprocess.TimeoutExpired('ffmpeg', 60)
                return directory / 'good.jpg'
            with patch.object(catalog, 'prepare_thumbnail', side_effect=prepare):
                entries = catalog.build_catalog(directory)
            self.assertEqual(len(entries), 2)
            self.assertEqual(entries[0]['thumbnailUrl'], '')
            self.assertTrue(entries[0]['thumbnailError'])
            self.assertEqual(entries[1]['thumbnailUrl'], (directory / 'good.jpg').as_uri())

    def test_missing_folder_is_empty_but_file_instead_of_folder_is_error(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            self.assertEqual(catalog.build_catalog(directory / 'missing'), [])
            source = directory / 'file'; source.write_text('not a folder')
            with self.assertRaisesRegex(ValueError, 'not a directory'):
                catalog.build_catalog(source)

    def test_cli_stdout_remains_json_when_thumbnail_has_error(self):
        entry = {'fileName': 'bad.mp4', 'fileUrl': 'file:///bad.mp4',
                 'thumbnailUrl': '', 'originalPath': '/bad.mp4',
                 'thumbnailError': 'decode failed'}
        output, errors = io.StringIO(), io.StringIO()
        with patch.object(catalog, 'build_catalog', return_value=[entry]), \
             contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            result = catalog.main([])
        self.assertEqual(result, 0)
        self.assertEqual(json.loads(output.getvalue()), [entry])
        self.assertIn('decode failed', errors.getvalue())

    def test_image_catalog_and_bundle_cli_include_both_media_sets(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            img_dir = root / 'images'
            vid_dir = root / 'videos'
            img_dir.mkdir(); vid_dir.mkdir()
            (img_dir / 'b.png').write_bytes(b'png')
            (img_dir / 'A.jpg').write_bytes(b'jpg')
            (img_dir / 'ignore.mp4').write_bytes(b'mp4')
            (vid_dir / 'clip.mp4').write_bytes(b'mp4')
            img_thumb = root / 'img_thumb.jpg'
            vid_thumb = root / 'vid_thumb.jpg'
            output, errors = io.StringIO(), io.StringIO()
            with patch.object(catalog, 'prepare_image_thumbnail', return_value=str(img_thumb)) as ithumbs, \
                 patch.object(catalog, 'prepare_thumbnail', return_value=str(vid_thumb)) as vthumbs, \
                 contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                result = catalog.main(['--bundle', str(img_dir), str(vid_dir)])
            self.assertEqual(result, 0)
            self.assertEqual(ithumbs.call_count, 2)
            self.assertEqual(vthumbs.call_count, 1)
            payload = json.loads(output.getvalue())
            self.assertEqual([e['fileName'] for e in payload['images']], ['A.jpg', 'b.png'])
            self.assertEqual([e['fileName'] for e in payload['videos']], ['clip.mp4'])
            self.assertEqual(payload['images'][0]['thumbnailUrl'], img_thumb.as_uri())
            self.assertEqual(payload['videos'][0]['thumbnailUrl'], vid_thumb.as_uri())
            self.assertEqual(errors.getvalue(), '')


if __name__ == '__main__':
    unittest.main()
