FROM python:3.11-slim-bookworm
WORKDIR /app
ENV PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1 MINERU_HOME=/models \
    MINERU_MODEL_SMALL_BACKEND=onnx MINERU_MODEL_ONNX_DEVICE=cpu \
    MINERU_MODEL_SOURCE=local MINERU_TELEMETRY_DISABLED=1
RUN apt-get update && apt-get install -y --no-install-recommends libgomp1 libglib2.0-0 libgl1 && rm -rf /var/lib/apt/lists/*
COPY scripts/docx-requirements.txt .
RUN pip install --no-cache-dir -r docx-requirements.txt 'Flask>=3.1,<4' 'gunicorn>=23,<27'
COPY scripts/docx_markdown.py docker/model/service.py ./
CMD ["sh","-ec","mineru-kit models verify --tier basic --small-backend onnx || mineru-kit models download --tier basic --small-backend onnx --source huggingface; mineru-kit models verify --tier basic --small-backend onnx; exec gunicorn --bind=0.0.0.0:8000 --workers=1 --threads=2 --timeout=660 service:app"]
