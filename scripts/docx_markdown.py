"""Local MinerU Flash / Basic conversion for Word and PDF."""
import base64
import io
import json
import os
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
from zipfile import BadZipFile, ZipFile

os.environ.setdefault("MINERU_MODEL_SMALL_BACKEND", "onnx")
os.environ.setdefault("MINERU_MODEL_ONNX_DEVICE", "cpu")
os.environ.setdefault("MINERU_MODEL_SOURCE", "local")
os.environ.setdefault("MINERU_TELEMETRY_DISABLED", "1")

from docvortex.content.tree import iter_image_payloads
from mineru.parser import parse
from PIL import Image
import pypdfium2 as pdfium

MAX_DOCUMENT = 20 * 1024 * 1024
OCR_WARNING = "Сверьте текст сканов с оригиналом. OCR может перепутать кириллицу, единицы измерения и знаки."


def word_markdown(path, warnings):
    with ZipFile(path) as archive:
        entries = archive.infolist()
        if ("word/document.xml" not in archive.namelist() or len(entries) > 4096
                or sum(entry.file_size for entry in entries) > 96 * 1024 * 1024):
            raise ValueError("Некорректный DOCX или превышен лимит распакованного документа.")

    def recognize_image(block):
        try:
            encoded = next(item.image_base64 for item in iter_image_payloads(block) if item.image_base64)
            raw = base64.b64decode(encoded.split(",")[-1], validate=True)
            image_path = path.with_suffix(".png")
            with Image.open(io.BytesIO(raw)) as image:
                if image.width * image.height > 24_000_000:
                    raise ValueError("Изображение слишком велико.")
                image.convert("RGB").save(image_path)
            text = parse(image_path, tier="basic", ocr_mode="ocr").markdown(image_renderer=lambda _: "").strip()
            if text:
                if OCR_WARNING not in warnings:
                    warnings.append(OCR_WARNING)
                return "\n\n" + text + "\n\n"
        except Exception as exc:
            print(f"MinerU image OCR: {type(exc).__name__}", file=sys.stderr)
        warnings.append("Не удалось извлечь текст встроенного изображения.")
        return "[Изображение не распознано]"

    return parse(path, tier="flash").markdown(image_renderer=recognize_image)


def pdf_markdown(path, warnings):
    # Keep the text layer where possible, including in mixed text/scan documents.
    runs = []
    with pdfium.PdfDocument(str(path)) as document:
        for index in range(len(document)):
            page = document[index]
            text = page.get_textpage()
            has_text = bool(text.get_text_bounded().strip())
            has_images = any(page.get_objects(filter=[pdfium.raw.FPDF_PAGEOBJ_IMAGE]))
            text.close()
            page.close()
            tier = "basic" if has_images or not has_text else "flash"
            if runs and runs[-1][0] == tier:
                runs[-1][2] = index + 1
            else:
                runs.append([tier, index + 1, index + 1])
    parts = []
    if any(tier == "basic" for tier, _, _ in runs):
        warnings.append(OCR_WARNING)
    for tier, first, last in runs:
        result = parse(path, tier=tier, ocr_mode="txt" if tier == "flash" else "auto",
                       page_range=f"{first}-{last}")
        parts.append(result.markdown())
    return "\n\n".join(parts)


def convert(data):
    if len(data) > MAX_DOCUMENT:
        raise ValueError("Размер файла превышает 20 МиБ.")
    warnings = []
    with TemporaryDirectory() as directory:
        path = Path(directory) / ("source.pdf" if data.startswith(b"%PDF-") else "source.docx")
        path.write_bytes(data)
        markdown = pdf_markdown(path, warnings) if path.suffix == ".pdf" else word_markdown(path, warnings)
    if not markdown.strip():
        raise ValueError("Не удалось извлечь текст документа.")
    return {"markdown": markdown, "warnings": warnings}


if __name__ == "__main__":
    output = os.fdopen(os.dup(1), "w", encoding="utf-8")
    os.dup2(2, 1)
    try:
        result = convert(sys.stdin.buffer.read(MAX_DOCUMENT + 1))
    except (BadZipFile, ValueError, pdfium.PdfiumError):
        result = {"error": "Не удалось прочитать DOCX или PDF."}
    json.dump(result, output, ensure_ascii=False)
    output.close()
