"""Regression tests for the November 2026 review: K3, K8 (figure capture) and C1-C5 (contract checker)."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest
from conftest import Notes

from analyst_kernel.errors import PATH_OUTSIDE_ROOT, RpcError
from analyst_kernel.kernel import Kernel
from analyst_kernel.runs import _ArtifactScanner, valid_sidecar

KERNEL_DIR = Path(__file__).resolve().parent.parent


def run_mpl(tmp_path: Path, body: str, name: str = "plot.py") -> tuple[subprocess.CompletedProcess, Path]:
    out = tmp_path / "outdir"
    out.mkdir(exist_ok=True)
    env = dict(os.environ)
    env.update(
        MPLBACKEND="module://analyst_mpl", PYTHONPATH=str(KERNEL_DIR), MPLCONFIGDIR=str(tmp_path / "mplcfg"),
        ANALYST_OUT_DIR=str(out), ANALYST_RUN_ID="20260101-000000-abcdef", ANALYST_ROOT=str(tmp_path),
    )
    script = tmp_path / name
    script.write_text("import os, json, sys\nimport matplotlib.pyplot as plt\nOUT = os.environ['ANALYST_OUT_DIR']\n" + body)
    proc = subprocess.run([sys.executable, str(script)], env=env, cwd=str(tmp_path), capture_output=True, text=True)
    return proc, out


# ---- K3: sidecars ------------------------------------------------------------------------


def test_k3_sidecar_never_overwrites_user_data(tmp_path: Path):
    proc, out = run_mpl(
        tmp_path,
        "json.dump({'chi2': 1.5}, open(os.path.join(OUT, 'results.json'), 'w'))\n"
        "json.dump({'mine': True}, open(os.path.join(OUT, 'keep.png.meta.json'), 'w'))\n"
        "fig, ax = plt.subplots()\nax.plot([0, 1])\nax.set_title('Results')\n"
        "fig.savefig(os.path.join(OUT, 'results.png'))\n"
        "fig.savefig(os.path.join(OUT, 'results.png'))\n"  # a second save replaces our own sidecar
        "fig.savefig(os.path.join(OUT, 'keep.png'))\n"
        "fig.savefig(os.path.join(OUT, 'photo.jpg')) if False else None\n"
        "fig.savefig(os.path.join(OUT, 'vector.pdf'))\n"
        "fig.savefig(os.path.join(OUT, 'raw.eps'))\n"
        "plt.close(fig)\n",
    )
    assert proc.returncode == 0, proc.stderr
    assert json.loads((out / "results.json").read_text()) == {"chi2": 1.5}, "the user's results.json was overwritten"
    side = json.loads((out / "results.png.meta.json").read_text())
    assert valid_sidecar(side) and side["title"] == "Results"
    assert json.loads((out / "keep.png.meta.json").read_text()) == {"mine": True}, "a file that is not a sidecar was replaced"
    assert (out / "vector.pdf.meta.json").exists()
    assert not (out / "raw.eps.meta.json").exists() and not (out / "raw.json").exists()
    assert not list(out.glob("*.tmp"))


def test_k3_scanner_uses_new_sidecar_names_and_validates_shape(tmp_path: Path):
    proc, out = run_mpl(
        tmp_path,
        "json.dump({'chi2': 1.5}, open(os.path.join(OUT, 'results.json'), 'w'))\n"
        "json.dump({'not': 'a sidecar'}, open(os.path.join(OUT, 'odd.png.meta.json'), 'w'))\n"
        "json.dump([1], open(os.path.join(OUT, 'orphan.svg.meta.json'), 'w'))\n"
        "fig, ax = plt.subplots()\nax.set_xlabel('x')\n"
        "fig.savefig(os.path.join(OUT, 'results.png'))\nfig.savefig(os.path.join(OUT, 'odd.png'))\nplt.close(fig)\n",
    )
    assert proc.returncode == 0, proc.stderr
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    sc = _ArtifactScanner(tmp_path, run_dir, out)
    live = {}
    for _ in range(5):
        live.update({a["path"].rsplit("/", 1)[-1]: a for a in sc.scan()})
    assert live["results.json"]["kind"] == "json", "a data file next to a figure with the same stem is an artifact"
    assert live["results.png"]["sidecar"]["axes"] == [{"title": None, "xlabel": "x", "ylabel": None}]
    assert "sidecar" not in live["odd.png"], "a malformed sidecar is not attached"
    assert not any(name.endswith(".meta.json") for name in live)
    final = {a["path"].rsplit("/", 1)[-1]: a for a in sc.scan(final=True)}
    # files under a sidecar name that are not sidecars are the user's JSON and are listed at the end
    assert set(final) == {"odd.png.meta.json", "orphan.svg.meta.json"} and final["odd.png.meta.json"]["kind"] == "json"
    assert not valid_sidecar({"title": "T"}) and not valid_sidecar([])


# ---- K8 and figure numbering ---------------------------------------------------------------


def test_k8_pause_and_nonblocking_show_keep_the_figure_and_save_the_last_frame(tmp_path: Path):
    proc, out = run_mpl(
        tmp_path,
        "fig, ax = plt.subplots()\n(line,) = ax.plot([0, 1], [0, 0])\n"
        "for i in range(1, 4):\n"
        "    line.set_ydata([0, i])\n    ax.set_xlabel('frame %d' % i)\n"
        "    plt.pause(0.01)\n"
        "    assert plt.get_fignums() == [1], 'pause() closed the figure'\n"
        "plt.show(block=False)\nassert plt.get_fignums() == [1]\n"
        "fig.show()\nassert plt.get_fignums() == [1]\n"
        "ax.set_xlabel('final')\n",  # changed after the last save: the exit hook stores the final state
    )
    assert proc.returncode == 0, proc.stderr
    names = sorted(p.name for p in (out / "figures").iterdir())
    assert names == ["fig-1.png", "fig-1.png.meta.json", "fig-1.svg", "fig-1.svg.meta.json"], names
    assert json.loads((out / "figures" / "fig-1.png.meta.json").read_text())["axes"][0]["xlabel"] == "final"
    assert "final" in (out / "figures" / "fig-1.svg").read_text()


def test_figures_are_numbered_across_show_calls_and_saved_at_exit(tmp_path: Path):
    proc, out = run_mpl(
        tmp_path,
        "for _ in range(2):\n    plt.figure(); plt.plot([0, 1])\n    plt.show()\n    assert plt.get_fignums() == []\n"
        "plt.figure(); plt.plot([1, 0]); plt.title('Named')\nplt.show()\n"
        "plt.figure(); plt.plot([1, 0])\nplt.show()\n"
        "fig, ax = plt.subplots()\nax.plot([2, 3])\nax.set_title('Left open')\n"  # never shown: saved at exit
        "fig2, ax2 = plt.subplots()\nax2.plot([2, 3])\nfig2.savefig(os.path.join(OUT, 'explicit.png'))\n"  # only savefig: not duplicated
        "plt.figure()\n",  # empty figure: ignored
    )
    assert proc.returncode == 0, proc.stderr
    pngs = sorted(p.name for p in (out / "figures").glob("*.png"))
    assert pngs == ["fig-1.png", "fig-2.png", "fig-3.png", "left-open.png", "named.png"], pngs
    assert (out / "explicit.png").exists() and (out / "explicit.png.meta.json").exists()


def test_hint_when_the_backend_is_switched_away(tmp_path: Path):
    proc, _ = run_mpl(
        tmp_path,
        "import matplotlib\nplt.figure(); plt.plot([0, 1])\nmatplotlib.use('Agg')\nplt.figure(); plt.plot([0, 1])\nplt.show()\n",
    )
    assert proc.returncode == 0, proc.stderr
    assert "switched the Matplotlib backend" in proc.stderr and "matplotlib.use" in proc.stderr
    quiet, _ = run_mpl(tmp_path, "plt.figure(); plt.plot([0, 1])\nplt.show()\n", name="quiet.py")
    assert quiet.returncode == 0 and quiet.stderr == ""


def test_mpl_module_avoids_newer_python_only_names():
    src = (KERNEL_DIR / "analyst_mpl" / "__init__.py").read_text()
    assert "import UTC" not in src and "datetime.UTC" not in src and "match " not in src
    assert src.startswith('"""') and "from __future__ import annotations" in src


def test_run_reports_figures_with_new_sidecars(kernel: Kernel, repo: Path, notes: Notes):
    run_id = kernel.call("run.start", {"script": "scripts/fit_decay.py"})["run_id"]
    notes.wait_for("run.done", run_id)
    arts = {a["artifact"]["path"].rsplit("/", 1)[-1]: a["artifact"] for a in notes.of("run.artifact", run_id)}
    assert set(arts) == {"decay-fit.png", "decay-fit.svg", "fit.json"}
    assert arts["decay-fit.png"]["sidecar"]["title"] == "Decay fit" and arts["decay-fit.svg"]["sidecar"]["title"] == "Decay fit"
    listed = {e["path"].rsplit("/", 1)[-1] for e in kernel.call("out.list", {"run_id": run_id})["entries"]}
    assert "decay-fit.png.meta.json" in listed


# ---- contract checker ----------------------------------------------------------------------


def diags(kernel, **params):
    return kernel.call("contract.check", params)["diagnostics"]


HEAD = "import argparse\n\ndef main():\n    p = argparse.ArgumentParser(description='d')\n    p.add_argument('--out', default='out', help='h')\n    args = p.parse_args()\n    out_dir = args.out\n"
TAIL = "\nif __name__ == '__main__':\n    main()\n"


def test_c1_fstrings_are_not_absolute_paths(kernel, repo: Path):
    (repo / "scripts" / "fs.py").write_text(
        HEAD
        + "    a = f'{out_dir}/spectrum.png'\n"  # line 8: fine
        + "    b = f'{out_dir}/{args.out}/x.csv'\n"  # line 9: fine
        + "    c = f'/data/{args.out}/x.csv'\n"  # line 10: starts with an absolute path
        + "    d = f'~/{args.out}'\n"  # line 11
        + "    e = f'{args.out:/>8}'\n"  # line 12: format spec
        + "    g = '/etc/hosts'\n"  # line 13: plain literal, still flagged
        + "    return a, b, c, d, e, g\n"
        + TAIL
    )
    found = sorted(d["line"] for d in diags(kernel, paths=["scripts/fs.py"]) if d["rule"] == "cli.no-absolute-paths")
    assert found == [10, 11, 13]


def test_c2_contract_functions_are_not_duplicates(kernel, repo: Path):
    parser = (
        "def build_parser():\n"
        "    parser = argparse.ArgumentParser(description='Summarize a run')\n"
        "    parser.add_argument('--input', default='data/sample.csv', help='input file')\n"
        "    parser.add_argument('--seed', type=int, default=0, help='random seed')\n"
        "    parser.add_argument('--out', default='out', help='output directory')\n"
        "    return parser\n\n"
        "def main(argv=None):\n"
        "    args = build_parser().parse_args(argv)\n"
        "    rows = count_rows(args.input)\n"
        "    print(rows, args.seed, args.out)\n"
        "    return 0\n"
    )
    for name in ("one", "two"):
        (repo / "scripts" / f"{name}.py").write_text("import argparse\nfrom analysis.io import count_rows\n\n" + parser + TAIL)
    ds = diags(kernel, paths=["scripts/one.py", "scripts/two.py"])
    assert [d for d in ds if d["rule"].startswith("lib.")] == [], "build_parser()/main() are required by the contract"
    pairs = kernel.call("contract.similar")["pairs"]
    assert not any(p["a"]["qualname"] in ("build_parser", "main") for p in pairs)
    # a helper with the same body in two scripts is still a duplicate
    helper = "def _fmt(a, b):\n    x = a + 1\n    y = x * b\n    return y\n\n"
    for name in ("one", "two"):
        (repo / "scripts" / f"{name}.py").write_text("import argparse\nfrom analysis.io import count_rows\n\n" + helper + parser + TAIL)
    assert [d["rule"] for d in diags(kernel, paths=["scripts/one.py"]) if d["rule"].startswith("lib.")] == ["lib.duplicate"]


def test_c3_ordinary_plotting_code_is_clean(kernel, repo: Path):
    (repo / "scripts" / "plot.py").write_text(
        "import argparse\nimport logging\nfrom pathlib import Path\nimport matplotlib.pyplot as plt\n"
        "log = logging.getLogger(__name__)\n\n"
        "def main():\n"
        "    p = argparse.ArgumentParser(description='d')\n"
        "    p.add_argument('--out', default='out', help='h')\n"
        "    args = p.parse_args()\n"
        "    fig, ax = plt.subplots(figsize=(8, 5))\n"  # keyword tuple
        "    ax.plot(args.out, alpha=0.4, lw=1.5, label='data/model')\n"
        "    ax.set_xlabel('time / s')\n"
        "    ax.set_ylabel('flux (erg/cm^2/s)')\n"
        "    ax.set_title('signal/noise ratio v1.2')\n"
        "    ax.legend('a/b.c')\n"
        "    log.info('wrote spectra/summary.csv to %s', args.out)\n"
        "    logging.warning('missing calib/dark.fits')\n"
        "    print('see docs/readme.md')\n"
        "    fig.savefig(Path(args.out) / 'spectrum.png', dpi=150)\n"
        "    ax.annotate('peak', (3, 4))\n"
        "    ax.text(7, 9, 'n/a')\n"
        "    fig.suptitle('and/or')\n"
        "    return 0\n" + TAIL
    )
    ds = [d for d in diags(kernel, paths=["scripts/plot.py"]) if d["rule"] == "cli.option-for-every-input"]
    assert ds == [], [d["message"] for d in ds]


def test_c3_real_inputs_are_still_flagged(kernel, repo: Path):
    (repo / "scripts" / "inputs.py").write_text(
        "import argparse\nfrom pathlib import Path\nimport numpy as np\nfrom analysis.io import load_runs\n\n"
        "def main():\n"
        "    p = argparse.ArgumentParser(description='d')\n"
        "    args = p.parse_args()\n"
        "    a = open('data/x.csv')\n"  # 9: open() is no longer exempt
        "    b = Path('data/raw/run1.npy')\n"  # 10
        "    c = load_runs('runs.csv')\n"  # 11: data extension, first positional
        "    d = np.linspace(0, 12.5, 300)\n"  # 12: two numeric positionals
        "    e = load_runs(args, 'summary.json')\n"  # later bare name: fine
        "    f = load_runs('flux / counts')\n"  # prose with a slash: fine
        "    g = load_runs('no_extension/dir')\n"  # no file extension: fine
        "    h = np.zeros(shape=64)\n"  # keyword number: fine
        "    return a, b, c, d, e, f, g, h\n" + TAIL
    )
    ds = [d for d in diags(kernel, paths=["scripts/inputs.py"]) if d["rule"] == "cli.option-for-every-input"]
    found = sorted((d["line"], d["message"].split(" passed")[0]) for d in ds)
    assert found == [
        (9, "path literal 'data/x.csv'"),
        (10, "path literal 'data/raw/run1.npy'"),
        (11, "path literal 'runs.csv'"),
        (12, "numeric literal 12.5"),
        (12, "numeric literal 300"),
    ]
    assert "open()" in ds[0]["message"]


def test_c4_listed_files_are_compared_with_the_whole_project(kernel, repo: Path):
    (repo / "scripts" / "copycat.py").write_text(
        "import argparse\nimport csv\nfrom pathlib import Path\n\n"
        "def _read(path):\n"
        "    ts = []\n    ys = []\n"
        "    with Path(path).open(newline='') as fh:\n"
        "        reader = csv.DictReader(fh)\n"
        "        for row in reader:\n"
        "            ts.append(float(row['t']))\n"
        "            ys.append(float(row['y']))\n"
        "    return ts, ys\n\n"
        "def main():\n    args = argparse.ArgumentParser(description='d').parse_args()\n    return _read(args)\n" + TAIL
    )
    ds = diags(kernel, paths=["scripts/copycat.py"])
    dup = [d for d in ds if d["rule"] == "lib.duplicate"]
    # checking only this file used to compare it with nothing and miss the library function it copies
    assert {(d["path"], d["related"][0]["path"]) for d in dup} == {
        ("scripts/copycat.py", "analysis/io.py"), ("scripts/copycat.py", "scripts/bad_script.py"),
    }
    assert all(d["path"] == "scripts/copycat.py" for d in ds if d["rule"] in ("lib.duplicate", "lib.near-duplicate"))
    # pairs between two unlisted files are not reported
    assert not any(d["path"] == "scripts/bad_script.py" and d["rule"] == "lib.duplicate" for d in ds)
    # from the library side the diagnostic is anchored on the listed file
    lib = [d for d in diags(kernel, paths=["analysis/io.py"]) if d["rule"] == "lib.duplicate"]
    assert {d["path"] for d in lib} == {"analysis/io.py"} and len(lib) == 2
    # the full check reports each pair once
    full = [d for d in diags(kernel) if d["rule"] == "lib.duplicate"]
    assert len(full) == 3


def test_c5_near_duplicate_search_is_fast_and_cached(kernel, repo: Path):
    body = "".join(f"    v{j} = (a + {j}) * b - c / (d + {j + 1})\n    if v{j} > c:\n        a = a + v{j}\n" for j in range(12))
    lines = ["import numpy as np\n"]
    for i in range(240):
        # same shape, different names and one extra statement each: no exact duplicates, all similar
        extra = "".join(f"    a = a - {k}\n" for k in range(i % 7))
        lines.append(f"def f{i}(a, b, c, d):\n    '''Doc.'''\n{body}{extra}    return a * {i}\n\n")
    (repo / "analysis" / "many.py").write_text("".join(lines))
    t0 = time.monotonic()
    pairs = kernel.call("contract.similar")["pairs"]
    first = time.monotonic() - t0
    near = [p for p in pairs if p["kind"] == "near-duplicate"]
    assert len(near) > 10000
    assert first < 20, f"near-duplicate search took {first:.1f}s"
    t0 = time.monotonic()
    again = kernel.call("contract.similar")["pairs"]
    second = time.monotonic() - t0
    assert again == pairs and second < 5, f"repeated search took {second:.1f}s"
    # a file check compares one file with the rest and is quick as well
    t0 = time.monotonic()
    diags(kernel, paths=["scripts/fit_decay.py"])
    assert time.monotonic() - t0 < 5


def test_similarity_cache_notices_edits(kernel, repo: Path):
    target = repo / "scripts" / "bad_script.py"
    assert any(p["kind"] == "duplicate" for p in kernel.call("contract.similar")["pairs"])
    text = target.read_text().replace('ts.append(float(row["t"]))\n            ys.append(float(row["y"]))', 'ys.append(float(row["y"]))')
    time.sleep(0.01)
    target.write_text(text)
    assert not any(p["kind"] == "duplicate" for p in kernel.call("contract.similar")["pairs"])


# ---- globs and directories ------------------------------------------------------------------


@pytest.mark.parametrize("glob", ["../*.py", "/etc/*.py", "scripts/../../*.py", "~/x/*.py"])
def test_globs_outside_root_are_rejected(kernel, repo: Path, glob: str):
    (repo.parent / "outside.py").write_text("print(1)\n")
    with pytest.raises(RpcError) as exc:
        kernel.call("scripts.index", {"globs": [glob]})
    assert exc.value.code == PATH_OUTSIDE_ROOT


def test_contract_check_globs_from_config_outside_root(repo: Path, tmp_path: Path):
    (repo / ".analyst" / "project.toml").write_text('[project]\nscripts = ["../*.py"]\n')
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits")
    try:
        for method in ("contract.check", "scripts.index", "contract.similar"):
            with pytest.raises(RpcError) as exc:
                k.call(method)
            assert exc.value.code == PATH_OUTSIDE_ROOT, method
        with pytest.raises(RpcError) as exc:
            k.call("contract.check", {"paths": ["../outside.py"]})
        assert exc.value.code == PATH_OUTSIDE_ROOT
    finally:
        k.close()


def test_checks_never_descend_into_venv_out_or_git(repo: Path, tmp_path: Path):
    bad = "import numpy as np\n\ndef leaked(x):\n    return np.sum(x)\n"
    for d in (".venv/lib/site", "out/fit/run1", ".git/hooks", "scripts/__pycache__", "scripts/.venv"):
        (repo / d).mkdir(parents=True, exist_ok=True)
        (repo / d / "leak.py").write_text(bad)
    (repo / "scripts" / "sub").mkdir()
    (repo / "scripts" / "sub" / "kept.py").write_text(bad)
    (repo / ".analyst" / "project.toml").write_text('[project]\nscripts = ["**/*.py"]\nlibrary = "analysis"\n')
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits")
    try:
        for params in ({}, {"paths": ["."]}, {"paths": ["scripts", "out", ".venv", ".git"]}):
            paths = {d["path"] for d in k.call("contract.check", params)["diagnostics"]}
            assert "scripts/sub/kept.py" in paths, params
            assert not any("leak.py" in p for p in paths), (params, paths)
        indexed = {s["path"] for s in k.call("scripts.index")["scripts"]}
        assert "scripts/sub/kept.py" in indexed and not any("leak.py" in p for p in indexed)
        assert not any("leak" in p["a"]["path"] + p["b"]["path"] for p in k.call("contract.similar")["pairs"])
    finally:
        k.close()
