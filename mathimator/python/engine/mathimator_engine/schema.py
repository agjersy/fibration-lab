"""Argument schemas and their validator.

Each operation declares its arguments with a small JSON-Schema dialect. ``describe_api``
serves the schemas as they are written here, so every client (the SDK, the agent's tool
description, the chip generator) sees exactly what the engine enforces.

Standard keywords: ``type`` (object, array, string, number, integer, boolean), ``properties``,
``required``, ``items``, ``minItems``, ``maxItems``, ``enum``, ``minimum``, ``maximum``,
``maxLength``, ``anyOf``, ``description``. Objects reject unknown properties.

Engine types (``type`` values that are not JSON Schema's):

* ``handle``: a handle reference; ``kinds`` lists the accepted handle kinds.
* ``scalar``: a finite number, math text such as ``"pi/2"``, or an ``expr`` handle.
* ``interval``: two scalars ``[lo, hi]``.
"""

from __future__ import annotations

import math
from typing import Any

from .errors import CallError
from .handles import is_ref


def handle(*kinds: str, description: str = "") -> dict[str, Any]:
    out: dict[str, Any] = {"type": "handle", "kinds": list(kinds)}
    if description:
        out["description"] = description
    return out


def scalar(description: str = "") -> dict[str, Any]:
    return {"type": "scalar", "description": description} if description else {"type": "scalar"}


def interval(description: str = "") -> dict[str, Any]:
    return {"type": "interval", "description": description} if description else {"type": "interval"}


def string(description: str = "", max_length: int = 4000, enum: list[str] | None = None) -> dict[str, Any]:
    out: dict[str, Any] = {"type": "string", "maxLength": max_length}
    if enum:
        out["enum"] = enum
    if description:
        out["description"] = description
    return out


def number(description: str = "", minimum: float | None = None, maximum: float | None = None) -> dict[str, Any]:
    out: dict[str, Any] = {"type": "number"}
    if minimum is not None:
        out["minimum"] = minimum
    if maximum is not None:
        out["maximum"] = maximum
    if description:
        out["description"] = description
    return out


def integer(description: str = "", minimum: int | None = None, maximum: int | None = None) -> dict[str, Any]:
    out = number(description, minimum, maximum)
    out["type"] = "integer"
    return out


STYLE = {
    "type": "object",
    "description": "Visual style; every key optional",
    "properties": {
        "color": {"type": "string", "maxLength": 32},
        "width": {"type": "number", "minimum": 0.1, "maximum": 20},
        "dash": {"type": "string", "enum": ["solid", "dashed", "dotted"]},
        "alpha": {"type": "number", "minimum": 0, "maximum": 1},
        "label": {"type": "string", "maxLength": 200},
        "fill": {"type": "boolean"},
    },
}

OBJECT_ID = {"type": "string", "maxLength": 64, "description": "Scene object id"}


def _fail(path: str, message: str) -> CallError:
    where = f"argument {path}" if path else "arguments"
    return CallError("bad_args", f"{where}: {message}")


def _is_number(value: Any) -> bool:
    return isinstance(value, int | float) and not isinstance(value, bool)


def validate(schema: dict[str, Any], value: Any, path: str = "") -> None:
    if "anyOf" in schema:
        errors = []
        for option in schema["anyOf"]:
            try:
                validate(option, value, path)
                return
            except CallError as exc:
                errors.append(exc.detail)
        raise _fail(path, "does not match any accepted form (" + "; ".join(errors) + ")")
    kind = schema.get("type")
    if "enum" in schema and value not in schema["enum"]:
        raise _fail(path, f"must be one of {', '.join(map(str, schema['enum']))}")
    if kind == "object":
        if not isinstance(value, dict):
            raise _fail(path, "must be an object")
        props = schema.get("properties", {})
        for name in schema.get("required", []):
            if name not in value:
                raise _fail(f"{path}.{name}" if path else name, "is required")
        for name, item in value.items():
            if name not in props:
                if "additionalProperties" in schema and schema["additionalProperties"] is not False:
                    validate(schema["additionalProperties"], item, f"{path}.{name}" if path else name)
                    continue
                raise _fail(f"{path}.{name}" if path else name, "is not a known argument")
            validate(props[name], item, f"{path}.{name}" if path else name)
    elif kind == "array":
        if not isinstance(value, list):
            raise _fail(path, "must be a list")
        if len(value) < schema.get("minItems", 0):
            raise _fail(path, f"needs at least {schema['minItems']} items")
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            raise _fail(path, f"takes at most {schema['maxItems']} items")
        if "items" in schema:
            for i, item in enumerate(value):
                validate(schema["items"], item, f"{path}[{i}]")
    elif kind == "string":
        if not isinstance(value, str):
            raise _fail(path, "must be a string")
        if len(value) > schema.get("maxLength", 4000):
            raise _fail(path, f"is longer than {schema.get('maxLength', 4000)} characters")
    elif kind in ("number", "integer"):
        if not _is_number(value) or (isinstance(value, float) and not math.isfinite(value)):
            raise _fail(path, "must be a finite number")
        if kind == "integer" and not (isinstance(value, int) or float(value).is_integer()):
            raise _fail(path, "must be a whole number")
        if "minimum" in schema and value < schema["minimum"]:
            raise _fail(path, f"must be at least {schema['minimum']}")
        if "maximum" in schema and value > schema["maximum"]:
            raise _fail(path, f"must be at most {schema['maximum']}")
    elif kind == "boolean":
        if not isinstance(value, bool):
            raise _fail(path, "must be true or false")
    elif kind == "handle":
        if not is_ref(value):
            raise _fail(path, 'must be a handle reference {"$h": id}')
    elif kind == "scalar":
        ok = (_is_number(value) and math.isfinite(value)) or isinstance(value, str) or is_ref(value)
        if not ok:
            raise _fail(path, "must be a number, math text, or an expression handle")
        if isinstance(value, str) and len(value) > 400:
            raise _fail(path, "is too long for a scalar")
    elif kind == "interval":
        if not isinstance(value, list) or len(value) != 2:
            raise _fail(path, "must be a pair [lo, hi]")
        for i, item in enumerate(value):
            validate({"type": "scalar"}, item, f"{path}[{i}]")
    elif kind is not None:
        raise _fail(path, f"has an unknown schema type {kind!r}")
