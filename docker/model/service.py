"""Local DOCX service. Load one model per worker and reuse it across requests."""
from threading import Lock
from zipfile import BadZipFile
from pypdfium2 import PdfiumError

from flask import Flask, jsonify, request

from docx_markdown import MAX_DOCUMENT, convert


app = Flask(__name__)
app.config["MAX_CONTENT_LENGTH"] = MAX_DOCUMENT
busy = Lock()


@app.get("/health")
def health():
    return {"ready": True}


@app.post("/convert")
def convert_document():
    if request.mimetype != "application/octet-stream":
        return jsonify(error="Ожидается DOCX или PDF."), 415
    if not busy.acquire(blocking=False):
        return jsonify(error="Конвертер занят. Повторите запрос позже."), 429
    try:
        return convert(request.get_data())
    except (BadZipFile, PdfiumError, ValueError):
        return jsonify(error="Не удалось прочитать DOCX или PDF. Проверьте содержимое и размер файла."), 422
    finally:
        busy.release()
