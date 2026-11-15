"""Project configuration: ``<root>/.analyst/project.toml`` with the defaults of PROTOCOL.md 2.2."""

from __future__ import annotations

import logging
import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

DEFAULT_SCRIPTS = ["scripts/**/*.py"]
DEFAULT_LIBRARY = "analysis"
DEFAULT_OUT_DIR = "out"
DEFAULT_DATA_DIRS = ["data"]
DEFAULT_TIMEOUT_S = 600
DEFAULT_ADDRESS_SPACE_GB = 0.0  # 0 means no RLIMIT_AS
DEFAULT_OPEN_FILES = 4096

log = logging.getLogger("analyst_kernel.config")


class ConfigError(ValueError):
    """A configuration value the kernel refuses to start with (exit code 2)."""

DEFAULT_MAX_HELPER_LINES = 15
DEFAULT_NEAR_DUPLICATE_THRESHOLD = 0.80
DEFAULT_MIN_TOKENS_FOR_SIMILARITY = 30
DEFAULT_MATH_REQUIRED: list[str] = []
DEFAULT_NUMERICAL_MODULES = [
    "numpy",
    "scipy",
    "pandas",
    "sklearn",
    "jax",
    "torch",
    "xarray",
    "statsmodels",
]
DEFAULT_SCRIPT_ALLOWED_FUNCTIONS = ["main", "build_parser", "parse_args"]

DEFAULT_POLICY: dict[str, Any] = {
    "edits": "review",
    "runs": "auto",
    "packages": "ask",
    "shell": "ask",
    "network": "deny",
    "git": "ask",
    "protected": [],
    "max_steps": 40,
}


@dataclass
class ProjectConfig:
    scripts: list[str] = field(default_factory=lambda: list(DEFAULT_SCRIPTS))
    library: str = DEFAULT_LIBRARY
    out_dir: str = DEFAULT_OUT_DIR
    data_dirs: list[str] = field(default_factory=lambda: list(DEFAULT_DATA_DIRS))
    timeout_s: float = DEFAULT_TIMEOUT_S

    max_helper_lines: int = DEFAULT_MAX_HELPER_LINES
    near_duplicate_threshold: float = DEFAULT_NEAR_DUPLICATE_THRESHOLD
    min_tokens_for_similarity: int = DEFAULT_MIN_TOKENS_FOR_SIMILARITY
    math_required: list[str] = field(default_factory=lambda: list(DEFAULT_MATH_REQUIRED))
    numerical_modules: list[str] = field(default_factory=lambda: list(DEFAULT_NUMERICAL_MODULES))
    script_allowed_functions: list[str] = field(
        default_factory=lambda: list(DEFAULT_SCRIPT_ALLOWED_FUNCTIONS)
    )
    severity: dict[str, str] = field(default_factory=dict)

    address_space_gb: float = DEFAULT_ADDRESS_SPACE_GB
    open_files: int = DEFAULT_OPEN_FILES

    policy: dict[str, Any] = field(default_factory=lambda: dict(DEFAULT_POLICY))
    targets: dict[str, dict[str, Any]] = field(default_factory=dict)

    raw: dict[str, Any] = field(default_factory=dict)

    def rlimit_as(self) -> int | None:
        """``RLIMIT_AS`` in bytes, or None when unlimited (the default)."""
        return int(self.address_space_gb * 1024**3) if self.address_space_gb > 0 else None

    def to_json(self) -> dict[str, Any]:
        return {
            "project": {
                "scripts": self.scripts,
                "library": self.library,
                "out_dir": self.out_dir,
                "data_dirs": self.data_dirs,
                "timeout_s": self.timeout_s,
            },
            "contract": {
                "max_helper_lines": self.max_helper_lines,
                "near_duplicate_threshold": self.near_duplicate_threshold,
                "min_tokens_for_similarity": self.min_tokens_for_similarity,
                "math_required": self.math_required,
                "numerical_modules": self.numerical_modules,
                "script_allowed_functions": self.script_allowed_functions,
                "severity": self.severity,
            },
            "limits": {"address_space_gb": self.address_space_gb, "open_files": self.open_files},
            "policy": self.policy,
            "targets": self.targets,
        }


def _list_of_str(value: Any, default: list[str]) -> list[str]:
    if isinstance(value, list) and all(isinstance(v, str) for v in value):
        return list(value)
    return list(default)


def _out_dir_text(value: Any) -> str:
    """The configured ``out_dir`` with trailing slashes removed; validation happens at startup."""
    text = str(value).strip()
    stripped = text.rstrip("/")
    return stripped or (text if text.startswith("/") else DEFAULT_OUT_DIR)


def validate_out_dir(root: Path, out_dir: str) -> str:
    """Return ``out_dir`` normalized, or raise ConfigError unless it is a relative path inside root.

    The output directory is created by the kernel and made writable in the sandbox, so a
    value such as ``../x``, ``/abs``, ``~/x``, ``.``, or a symlink that leaves the repo is refused.
    It may not be or contain the source areas ``.git`` and ``.analyst`` either.
    """
    text = str(out_dir).strip().rstrip("/")
    if not text:
        raise ConfigError("out_dir is empty")
    p = Path(text)
    if p.is_absolute() or text.startswith("~") or "\\" in text:
        raise ConfigError(f"out_dir {out_dir!r} must be a relative path inside the repo root")
    parts = p.parts
    if not parts or any(part in ("..", ".") for part in parts):
        raise ConfigError(f"out_dir {out_dir!r} must not contain '.' or '..' segments")
    if parts[0] in (".git", ".analyst", ".venv"):
        raise ConfigError(f"out_dir {out_dir!r} must not be inside {parts[0]}/")
    root_r = Path(os.path.realpath(root))
    real = Path(os.path.realpath(root_r / p))
    if real == root_r or root_r not in real.parents:
        raise ConfigError(f"out_dir {out_dir!r} resolves outside the repo root")
    return p.as_posix()


def project_data_dirs(root: Path, entries: list[str]) -> tuple[list[Path], list[str]]:
    """Resolve ``data_dirs`` from the repo's own config: only directories inside the root count.

    Returns the accepted absolute paths and one warning per ignored entry. Directories
    outside the repo are granted only through the kernel's ``--data-dir`` argument, which
    comes from the user's global configuration and not from the repository.
    """
    root_r = Path(os.path.realpath(root))
    accepted: list[Path] = []
    warnings: list[str] = []
    for entry in entries:
        text = str(entry)
        if not text or text.startswith("~") or Path(text).is_absolute():
            candidate = None
        else:
            candidate = Path(os.path.realpath(root_r / text))
        if candidate is None or (candidate != root_r and root_r not in candidate.parents):
            warnings.append(
                f"data_dirs entry {text!r} in project.toml is outside the repo root and was ignored; "
                "grant external data directories in the target's data_dirs (global config)"
            )
            continue
        accepted.append(root_r / text)
    return accepted, warnings


def config_from_dict(data: dict[str, Any]) -> ProjectConfig:
    project = data.get("project", {}) if isinstance(data.get("project"), dict) else {}
    contract = data.get("contract", {}) if isinstance(data.get("contract"), dict) else {}
    severity_raw = contract.get("severity", {})
    severity = {
        str(k): str(v)
        for k, v in (severity_raw.items() if isinstance(severity_raw, dict) else [])
        if str(v) in ("error", "warn")
    }
    policy = dict(DEFAULT_POLICY)
    if isinstance(data.get("policy"), dict):
        policy.update(data["policy"])
    targets = data.get("targets", {}) if isinstance(data.get("targets"), dict) else {}
    limits = data.get("limits", {}) if isinstance(data.get("limits"), dict) else {}

    def num(value: Any, default: float) -> float:
        return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else default

    def positive(value: Any, default: float) -> float:
        v = num(value, default)
        return v if v > 0 else default

    return ProjectConfig(
        scripts=_list_of_str(project.get("scripts"), DEFAULT_SCRIPTS),
        library=str(project.get("library", DEFAULT_LIBRARY)),
        out_dir=_out_dir_text(project.get("out_dir", DEFAULT_OUT_DIR)),
        data_dirs=_list_of_str(project.get("data_dirs"), DEFAULT_DATA_DIRS),
        timeout_s=positive(project.get("timeout_s"), DEFAULT_TIMEOUT_S),
        max_helper_lines=int(num(contract.get("max_helper_lines"), DEFAULT_MAX_HELPER_LINES)),
        near_duplicate_threshold=num(
            contract.get("near_duplicate_threshold"), DEFAULT_NEAR_DUPLICATE_THRESHOLD
        ),
        min_tokens_for_similarity=int(
            num(contract.get("min_tokens_for_similarity"), DEFAULT_MIN_TOKENS_FOR_SIMILARITY)
        ),
        math_required=_list_of_str(contract.get("math_required"), DEFAULT_MATH_REQUIRED),
        numerical_modules=_list_of_str(contract.get("numerical_modules"), DEFAULT_NUMERICAL_MODULES),
        script_allowed_functions=_list_of_str(
            contract.get("script_allowed_functions"), DEFAULT_SCRIPT_ALLOWED_FUNCTIONS
        ),
        severity=severity,
        address_space_gb=max(0.0, num(limits.get("address_space_gb"), DEFAULT_ADDRESS_SPACE_GB)),
        open_files=int(positive(limits.get("open_files"), DEFAULT_OPEN_FILES)),
        policy=policy,
        targets={str(k): dict(v) for k, v in targets.items() if isinstance(v, dict)},
        raw=data,
    )


def load_config(root: Path) -> ProjectConfig:
    """Load ``<root>/.analyst/project.toml``; a missing file means all defaults."""
    path = Path(root) / ".analyst" / "project.toml"
    if not path.is_file():
        return ProjectConfig()
    with path.open("rb") as fh:
        try:
            data = tomllib.load(fh)
        except tomllib.TOMLDecodeError:
            return ProjectConfig()
    return config_from_dict(data)
