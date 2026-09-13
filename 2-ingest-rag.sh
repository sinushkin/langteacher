#!/usr/bin/env bash
set -euo pipefail

# Пример: ./3-ingest-rag.sh path/to/book.pdf --book "Book Name" --language de --level A2 --lesson "Kapitel 3"
VENV="/home/user/data/python-envs/langteacher"
cd "$(dirname "$0")"

source "$VENV/bin/activate"
python3 rag_engine.py ingest "$@"
