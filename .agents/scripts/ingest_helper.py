"""
Companion script for Ingest-Dependencies.ps1.
Ingests markdown files into the knowledge-rag vector store using knowledge-rag's Python API.

Usage:
    python ingest_helper.py <directory>          # Ingest all .md files in dir
    python ingest_helper.py --files file1.md file2.md  # Ingest specific files
"""

import sys
import os
import argparse
from pathlib import Path

# Patch fastembed to use local cached model files offline
try:
    import fastembed
    _orig_init = fastembed.TextEmbedding.__init__
    def _patched_init(self, *args, **kwargs):
        kwargs["local_files_only"] = True
        return _orig_init(self, *args, **kwargs)
    fastembed.TextEmbedding.__init__ = _patched_init
except Exception:
    pass

# Patch FastEmbedEmbeddings._embed for numpy 2.x compatibility
try:
    from mcp_server import server
    def _safe_embed(self, texts):
        if not texts:
            return []
        self._load_model()
        res = []
        for t in texts:
            vecs = list(self._model.embed([t]))
            res.append(vecs[0].tolist())
        return res
    server.FastEmbedEmbeddings._embed = _safe_embed
except Exception:
    pass

def get_ingest_func():
    try:
        from mcp_server.server import add_document
        return add_document
    except Exception as e:
        print(f"Error importing knowledge-rag server API: {e}", file=sys.stderr)
        sys.exit(1)

def ingest_single_file(add_doc_fn, file_path: Path, category: str = "development"):
    if not file_path.is_file():
        print(f"File not found: {file_path}", file=sys.stderr)
        return False

    try:
        content = file_path.read_text(encoding="utf-8")
        rel_path = file_path.name
        print(f"Ingesting: {rel_path} (category: {category}) ... ", end="", flush=True)
        res = add_doc_fn(content=content, filepath=rel_path, category=category)
        print(f"OK ({res})")
        return True
    except Exception as e:
        print(f"FAILED ({e})", file=sys.stderr)
        return False

def main():
    parser = argparse.ArgumentParser(description="Ingest markdown files into knowledge-rag")
    parser.add_argument("directory", nargs="?", help="Directory containing .md files to ingest")
    parser.add_argument("--files", nargs="+", help="Specific files to ingest")
    parser.add_argument("--category", default="development", help="Document category (default: development)")

    args = parser.parse_args()
    add_doc_fn = get_ingest_func()

    if args.files:
        success = 0
        for f in args.files:
            if ingest_single_file(add_doc_fn, Path(f), category=args.category):
                success += 1
        print(f"\nDone. Successfully ingested {success}/{len(args.files)} file(s).")
    elif args.directory:
        dir_path = Path(args.directory)
        if not dir_path.is_dir():
            print(f"Directory not found: {dir_path}", file=sys.stderr)
            sys.exit(1)
        
        md_files = list(dir_path.glob("*.md"))
        if not md_files:
            print(f"No .md files found in {dir_path}")
            return

        print(f"Found {len(md_files)} file(s) in {dir_path}")
        success = 0
        for md_file in md_files:
            if ingest_single_file(add_doc_fn, md_file, category=args.category):
                success += 1
        print(f"\nDone. Successfully ingested {success}/{len(md_files)} file(s).")
    else:
        parser.print_help()

if __name__ == "__main__":
    main()
