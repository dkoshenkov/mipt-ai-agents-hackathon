"""Run inside the MinerU image with /app on PYTHONPATH."""
import io
import unittest
from unittest.mock import patch

from docx import Document
from PIL import Image

import docx_markdown


class DocumentConversion(unittest.TestCase):
    def test_table_and_embedded_scan_keep_their_order(self):
        doc = Document()
        table = doc.add_table(rows=2, cols=2)
        for row, values in zip(table.rows, (("Параметр", "Значение"), ("Масса", "≤ 0,05 кг"))):
            for cell, value in zip(row.cells, values):
                cell.text = value
        image = io.BytesIO()
        Image.new("RGB", (60, 50), "white").save(image, format="PNG")
        doc.add_picture(image)
        doc.add_paragraph("Конец")
        data = io.BytesIO()
        doc.save(data)

        native_parse = docx_markdown.parse
        def parse(path, **options):
            if path.suffix == ".docx":
                return native_parse(path, **options)
            self.assertEqual(options["tier"], "basic")
            with Image.open(path) as extracted:
                self.assertEqual(extracted.size, (60, 50))
            return unittest.mock.Mock(markdown=lambda **kwargs: "Текст скана")

        with patch("docx_markdown.parse", side_effect=parse):
            result = docx_markdown.convert(data.getvalue())
        md = result["markdown"]
        self.assertEqual(result["warnings"], [docx_markdown.OCR_WARNING])
        self.assertIn("| Масса | ≤ 0,05 кг |", md)
        self.assertLess(md.index("0,05"), md.index("Текст скана"))
        self.assertLess(md.index("Текст скана"), md.index("Конец"))


if __name__ == "__main__":
    unittest.main()
