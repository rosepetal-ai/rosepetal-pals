#!/usr/bin/env python3
"""Validates the frozen vectors against the JSON Schemas of the inventory
extension, with an independent validator.

Each schema in ``docs/extensions/inventory-v1/*.schema.json`` must be a valid
Draft 2020-12 schema, and the document of every frozen vector
(``crates/pals-core/tests/vectors/*-v1.json``) that has a schema, listed in
``DOCUMENTS``, must be valid against it. Vectors with no schema (grants,
events, the card JWS) are skipped; a schema no vector exercises, or a listed
vector that does not exist, is an error.

Prints ``N documents valid`` and exits 0, or lists every error and exits 1.
Needs the interop venv: ``jsonschema`` as pinned in ``requirements.txt``.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[2]
VECTORS = ROOT / "crates/pals-core/tests/vectors"
SCHEMAS = ROOT / "docs/extensions/inventory-v1"

# Vector file -> (schema file, JSON pointer to the document in the vector).
DOCUMENTS: dict[str, tuple[str, str]] = {
    "contact-v1.json": ("contact-record.schema.json", "/signed"),
    "announce-v1.json": ("announce-record.schema.json", "/signed"),
    "inventory-v1.json": ("inventory.schema.json", "/inventory"),
    "view-v1.json": ("inventory-view.schema.json", "/signed"),
}


def pointer(document: object, path: str) -> object:
    """The value at the RFC 6901 pointer ``path`` in ``document``."""
    for token in path.split("/")[1:]:
        token = token.replace("~1", "/").replace("~0", "~")
        document = document[int(token)] if isinstance(document, list) else document[token]
    return document


def main() -> int:
    errors: list[str] = []
    schemas = {path.name: path for path in sorted(SCHEMAS.glob("*.schema.json"))}
    validators = {}
    for name, path in schemas.items():
        schema = json.loads(path.read_text(encoding="utf-8"))
        try:
            Draft202012Validator.check_schema(schema)
        except Exception as error:  # noqa: BLE001 - every schema error is reported
            errors.append(f"{name}: not a valid Draft 2020-12 schema: {error}")
            continue
        validators[name] = Draft202012Validator(schema)

    for name in sorted(set(schemas) - {schema for schema, _ in DOCUMENTS.values()}):
        errors.append(f"{name}: no vector is validated against it")

    vectors = {path.name for path in VECTORS.glob("*-v1.json")}
    valid = 0
    for vector_name, (schema_name, document_path) in sorted(DOCUMENTS.items()):
        if vector_name not in vectors:
            errors.append(f"{vector_name}: listed, but there is no such vector")
            continue
        if schema_name not in schemas:
            errors.append(f"{vector_name}: its schema {schema_name} does not exist")
            continue
        if schema_name not in validators:
            continue
        vector = json.loads((VECTORS / vector_name).read_text(encoding="utf-8"))
        try:
            document = pointer(vector, document_path)
        except (KeyError, IndexError, ValueError, TypeError):
            errors.append(f"{vector_name}: no document at {document_path}")
            continue
        found = sorted(validators[schema_name].iter_errors(document), key=str)
        for error in found:
            where = "/".join(str(part) for part in error.absolute_path)
            errors.append(f"{vector_name} ({schema_name}) at /{where}: {error.message}")
        if not found:
            valid += 1

    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print(f"{valid} documents valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
