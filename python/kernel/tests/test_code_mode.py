from __future__ import annotations

import json
import sys

import pytest
from corpus import make_project, slide, text
from pptx import Presentation
from present_sdk import tags as T

from present_kernel.errors import RpcError
from present_kernel.runner.lint import lint_script
from present_kernel.runner.sandbox import SandboxSpec, bwrap_argv, sandbox_exec_profile

BUILD = '''
from present_sdk import Deck, cite, theme
from pptx.util import Emu

deck = Deck.open_template()
s = deck.add_slide("Title and Content", slide_id="s-code1")
s.title("Built by code")
p = s.placeholder("body").paragraph("Revenue reached 120.5", level=0)
cite(p.runs[0], "ref:r-q3rep@3f9a1c2b7e01#p11:c120-188")
s.placeholder("body").paragraph("Second point", level=1)
s2 = deck.add_slide("Title Only")
s2.title("Raw python-pptx shape")
shape = s2.raw.shapes.add_shape(1, Emu(914400), Emu(2000000), Emu(2000000), Emu(900000))
shape.text_frame.text = "custom"
shape.fill.solid()
shape.fill.fore_color.theme_color = 5
print("built", len(deck.slides), "slides")
deck.save()
'''


def code_project(tmp_path, script=BUILD):
    project = make_project(tmp_path / "p", "plain", [], {"mode": "code", "sections": [{"title": "", "slides": []}]})
    (project / "deck" / "build.py").write_text(script, encoding="utf-8")
    return project


def build(kernel_for, project, **params):
    return kernel_for(project).dispatcher.call("deck.build", {"script": "deck/build.py", "out": "exports/deck.pptx", "snapshot_id": "h-9", **params})


def test_build_runs_and_maps_every_shape(kernel_for, tmp_path):
    project = code_project(tmp_path)
    out = build(kernel_for, project)
    assert "built 2 slides" in out["stdout"]
    prs = Presentation(str(project / "exports/deck.pptx"))
    assert next(T.read_slide_tags(s.part)["present.slide"] for s in prs.slides) == "s-code1"
    ids = {e["element_id"] for e in out["element_map"]}
    assert any(i.startswith("e-raw-") for i in ids)  # the raw python-pptx shape got an id at save
    body = prs.slides[0].shapes[1]
    assert T.read_shape_tags(prs.slides[0].part, body._element)["present.cite.0"].startswith("ref:r-q3rep")
    assert T.read_custom_properties(prs)["present.mode"] == "code"
    k = kernel_for(project)
    diags = k.dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "engine": False})
    assert [d for d in diags if d["severity"] == "error" and d["rule"] != "src.dangling"] == []


def test_failing_script_reports_and_writes_nothing(kernel_for, tmp_path):
    project = code_project(tmp_path, "import sys\nprint('about to fail')\nraise ValueError('boom')\n")
    with pytest.raises(RpcError) as err:
        build(kernel_for, project)
    assert err.value.code == 2005 and "boom" in err.value.extra["stderr"] and "about to fail" in err.value.extra["stdout"]
    assert not (project / "exports/deck.pptx").exists()


def test_script_that_never_saves_is_an_error(kernel_for, tmp_path):
    with pytest.raises(RpcError) as err:
        build(kernel_for, code_project(tmp_path, "print('nothing')\n"))
    assert err.value.code == 2005 and "deck.save()" in err.value.detail


def test_timeout_kills_the_script(kernel_for, tmp_path):
    with pytest.raises(RpcError) as err:
        build(kernel_for, code_project(tmp_path, "import time\ntime.sleep(60)\n"), timeout_s=2)
    assert err.value.code == 2006


def test_environment_is_scrubbed(kernel_for, tmp_path, monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-secret")
    script = BUILD + "\nimport os\nprint('KEY', os.environ.get('ANTHROPIC_API_KEY'))\nprint('HOME', os.environ['HOME'])\n"
    out = build(kernel_for, code_project(tmp_path, script))
    assert "KEY None" in out["stdout"] and "sk-secret" not in out["stdout"]
    assert "present-build-" in out["stdout"]


def test_hardcoded_style_lint():
    src = "from pptx.dml.color import RGBColor\nrun.font.color.rgb = RGBColor(0xFF, 0, 0)\nrun.font.name = 'Comic Sans MS'\nx.font.name = '+mn-lt'\n"
    diags = lint_script(src)
    assert [d["rule"] for d in diags] == ["code.hardcoded-style"] * 2
    assert ":2:" in diags[0]["message"] and "Comic Sans MS" in diags[1]["message"]
    assert lint_script("def broken(:\n")[0]["rule"] == "build"


def test_build_reports_lint_and_sandbox_kind(kernel_for, tmp_path):
    script = BUILD.replace("shape.fill.fore_color.theme_color = 5", "from pptx.dml.color import RGBColor\nshape.fill.fore_color.rgb = RGBColor(255, 0, 0)")
    out = build(kernel_for, code_project(tmp_path, script))
    rules = [w["rule"] for w in out["warnings"]]
    assert "code.hardcoded-style" in rules
    assert out["sandbox"] in ("limits", "bwrap", "sandbox-exec", "appcontainer")
    if out["sandbox"] == "limits":
        assert any("unsandboxed" in w["message"] for w in out["warnings"])


GENERATED = '''
from pptx.util import Emu
slide.title("Generated slide")
x, y, cx, cy = slide.free_box()
for i in range(3):
    slide.shape({"shape": "ellipse", "fill": "accent%d" % (i + 1)}, (x + i * cx // 3, y, cx // 4, cy // 3))
'''


def gen_project(tmp_path, script=GENERATED):
    docs = [slide(1, "Title and Content", "Normal slide", placeholders={"body": text("e-b1", "A point")}),
            {"id": "s-gen1", "kind": "generated", "layout": "Title Only", "placeholders": {}, "elements": [], "generated": {"script": script}}]  # fmt: skip
    return make_project(tmp_path / "p", "plain", docs)


def test_generated_slide_renders_inside_the_runner(kernel_for, tmp_path):
    project = gen_project(tmp_path)
    k = kernel_for(project)
    out = k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    prs = Presentation(str(project / "exports/deck.pptx"))
    assert len(prs.slides) == 2 and prs.slides[1].shapes.title.text == "Generated slide"
    assert len([e for e in out["element_map"] if e["slide_id"] == "s-gen1" and not e["element_id"].startswith(("e-ftr", "e-sldNum"))]) == 4
    single = k.dispatcher.call("slide.generate", {"slide_id": "s-gen1"})
    assert len(Presentation(str(project / single["out"])).slides) == 1


def test_generated_slide_failure_is_a_build_error(kernel_for, tmp_path):
    project = gen_project(tmp_path, "raise RuntimeError('bad slide')\n")
    with pytest.raises(RpcError) as err:
        kernel_for(project).dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    assert err.value.code == 2005 and "bad slide" in err.value.extra["stderr"]


@pytest.mark.parametrize("template", ["corp", "plain", "wide-dense"])
def test_eject_verifies_for_the_corpus(kernel_for, tmp_path, template):
    project = make_project(tmp_path / "p", template)
    out = kernel_for(project).dispatcher.call("deck.eject", {"out_dir": "deck"})
    assert out["verified"], out.get("diff")
    source = (project / "deck/build.py").read_text(encoding="utf-8")
    assert "def slide_02_margin_recovered_to_18_on" in source and "deck.save()" in source
    compile(source, "build.py", "exec")


def test_eject_inlines_generated_slides(kernel_for, tmp_path):
    project = gen_project(tmp_path)
    out = kernel_for(project).dispatcher.call("deck.eject", {})
    assert out["verified"], out.get("diff")
    assert "generated slide script (inlined)" in (project / "deck/build.py").read_text(encoding="utf-8")


def test_profiles_and_argv_shapes(tmp_path):
    home = tmp_path / "home"
    (home / "proj" / "deck").mkdir(parents=True)
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    spec = SandboxSpec(cwd=scratch, scratch=scratch, read_only=[home / "proj" / "deck"])
    profile = sandbox_exec_profile(spec, home)
    def q(path):  # the profile's string quoting: backslashes doubled (Windows paths in this test)
        return '"' + str(path).replace("\\", "\\\\") + '"'

    assert "(deny network*)" in profile and f"(deny file-read* (subpath {q(home)}))" in profile
    assert f"(allow file-read* (subpath {q(home / 'proj' / 'deck')}))" in profile
    assert f"(allow file-read-metadata (literal {q(home / 'proj')}))" in profile
    assert f"(allow file-read* file-write* (subpath {q(scratch)}))" in profile
    argv = bwrap_argv(spec, ["python", "x"], home)
    assert "--unshare-net" in argv and argv[-2:] == ["python", "x"]
    assert ["--bind", str(scratch), str(scratch)] == argv[argv.index("--bind"):argv.index("--bind") + 3]


def test_hello_reports_sandbox(kernel_for, tmp_path):
    hello = kernel_for(tmp_path).dispatcher.call("kernel.hello", {"core_version": "0", "protocol_version": 1})
    assert hello["sandbox"]["kind"] in ("limits", "bwrap", "sandbox-exec", "appcontainer") and hello["sandbox"]["available"]


@pytest.mark.skipif(sys.platform == "win32", reason="skill scripts are covered by the sandbox job on Windows")
def test_skill_script_runs_and_returns_files(kernel_for, tmp_path):
    project = make_project(tmp_path / "p", "plain", [])
    skill = tmp_path / "skills" / "demo"
    (skill / "scripts").mkdir(parents=True)
    (skill / "scripts" / "make.py").write_text("import sys, json\njson.dump({'args': sys.argv[1:]}, open('result.json', 'w'))\nprint('ok')\n")
    out = kernel_for(project).dispatcher.call("skill.run_script", {"script_abs": str(skill / "scripts" / "make.py"), "skill_dir_abs": str(skill), "args": ["a", "b"]})
    assert out["exit_code"] == 0 and "ok" in out["stdout"]
    assert json.loads((project / out["files"][0]).read_text(encoding="utf-8")) == {"args": ["a", "b"]}


# ---- isolation (PROTOCOL 9, BUILD-SPEC S1): only meaningful where a native sandbox exists --------

ESCAPES = {
    "network": "import socket\ns = socket.create_connection(('1.1.1.1', 443), timeout=5)\nprint('ESCAPED')\n",
    "read_home": "import os, pathlib\np = pathlib.Path(os.environ['PRESENT_TEST_SECRET'])\nprint('ESCAPED', p.read_text())\n",
    "write_outside": "import os, pathlib\npathlib.Path(os.environ['PRESENT_TEST_TARGET']).write_text('x')\nprint('ESCAPED')\n",
    "write_deck": "import os, pathlib\n(pathlib.Path(os.environ['PRESENT_PROJECT']) / 'deck' / 'deck.json').write_text('{}')\nprint('ESCAPED')\n",
}


@pytest.mark.parametrize("attempt", sorted(ESCAPES))
def test_native_sandbox_blocks_escapes(kernel_for, tmp_path, attempt, monkeypatch):
    from pathlib import Path

    project = code_project(tmp_path)
    k = kernel_for(project)
    kind = k.dispatcher.call("kernel.hello", {"core_version": "0", "protocol_version": 1})["sandbox"]["kind"]
    if kind == "limits":
        pytest.skip("no native sandbox on this machine; isolation is not enforced in the limits fallback")
    secret = Path.home() / ".present-sandbox-test-secret"
    secret.write_text("top secret")
    target = Path.home() / ".present-sandbox-test-target"
    target.unlink(missing_ok=True)
    before = (project / "deck" / "deck.json").read_text(encoding="utf-8")
    # The escape scripts read their targets from the environment, which the kernel scrubs; bake them in.
    script = ESCAPES[attempt].replace("os.environ['PRESENT_TEST_SECRET']", repr(str(secret))).replace(
        "os.environ['PRESENT_TEST_TARGET']", repr(str(target)))  # fmt: skip
    (project / "deck" / "build.py").write_text(script, encoding="utf-8")
    try:
        with pytest.raises(RpcError) as err:
            build(kernel_for, project)
        # Under bwrap the home directory is a private, discarded tmpfs: a write there "succeeds"
        # inside the sandbox and never reaches the host, so the host file is the evidence.
        if not (attempt == "write_outside" and kind == "bwrap"):
            assert "ESCAPED" not in err.value.extra.get("stdout", "")
        assert not target.exists()
        assert (project / "deck" / "deck.json").read_text(encoding="utf-8") == before
    finally:
        secret.unlink(missing_ok=True)
        target.unlink(missing_ok=True)
