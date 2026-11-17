from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest
from corpus import TEMPLATES
from present_sdk.fonts import FontIndex
from present_sdk.template import describe_template

GOLDEN = Path(__file__).parent / "golden"


def _stable(desc: dict) -> dict:
    out = dict(desc)
    out.pop("fonts_missing")  # depends on the machine's fonts
    return out


@pytest.mark.parametrize("name", ["plain", "corp", "wide-dense"])
def test_inspector_matches_golden(kernel_for, tmp_path, name, request):
    src = TEMPLATES[name]
    (tmp_path / "template").mkdir()
    shutil.copy(src, tmp_path / "template" / src.name)
    side = src.parent / "template.toml"
    if side.is_file():
        shutil.copy(side, tmp_path / "template" / "template.toml")
    desc = kernel_for(tmp_path).dispatcher.call("template.inspect", {"path": f"template/{src.name}"})
    again = kernel_for(tmp_path).dispatcher.call("template.inspect", {"path": f"template/{src.name}"})
    assert desc == again
    path = GOLDEN / f"template-{name}.json"
    text = json.dumps(_stable(desc), indent=1, sort_keys=True)
    if request.config.getoption("--update-golden") or not path.exists():
        path.write_text(text, encoding="utf-8")
    assert json.loads(text) == json.loads(path.read_text(encoding="utf-8"))
    for master in desc["masters"]:
        for layout in master["layouts"]:
            keys = [p["key"] for p in layout["placeholders"]]
            assert len(keys) == len(set(keys)), layout["name"]


def test_sidecar_descriptions_and_avoid(kernel_for, tmp_path):
    src = TEMPLATES["corp"]
    (tmp_path / "template").mkdir()
    shutil.copy(src, tmp_path / "template" / src.name)
    shutil.copy(src.parent / "template.toml", tmp_path / "template" / "template.toml")
    k = kernel_for(tmp_path)
    desc = k.dispatcher.call("template.inspect", {"path": "template/corp.potx"})
    layouts = {lay["name"]: lay for lay in desc["masters"][0]["layouts"]}
    assert layouts["Layout 7"]["description"] == "Big number with caption"
    assert layouts["Blank"]["usable"] is False
    side = k.dispatcher.call("template.sidecar", {"path": "template/corp.potx"})
    assert side["rules"]["mandatory"] == ["Title Slide", "Disclaimer"]


def test_fonts_missing_reports_absent_font(tmp_path):
    empty = tmp_path / "nofonts"
    empty.mkdir()
    desc = describe_template(TEMPLATES["corp"], fonts=FontIndex([empty]))
    assert desc["fonts_missing"] == ["Carlito", "DejaVu Serif"]
    vendored = FontIndex([Path(__file__).resolve().parents[3] / "vendor" / "fonts"])
    assert describe_template(TEMPLATES["corp"], fonts=vendored)["fonts_missing"] == []


def test_path_escape_is_rejected(kernel_for, tmp_path):
    from present_kernel.errors import RpcError

    with pytest.raises(RpcError) as err:
        kernel_for(tmp_path).dispatcher.call("template.inspect", {"path": "../outside.pptx"})
    assert err.value.code == 2001
