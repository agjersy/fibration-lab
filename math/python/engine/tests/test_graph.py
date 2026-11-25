"""The scene as a dependency graph (ARCHITECTURE 5.6)."""

from fibration_math_engine.engine import Engine
from fibration_math_engine.sample import sample

VIEW = {"x": [-10, 10], "y": [-6, 6]}


def scene() -> Engine:
    e = Engine()

    def call(fn, **args):
        result = e.call(fn, args)
        assert result["status"] == "ok", result
        return result

    call("param.declare", name="a", value=1.0, min=-3, max=3)
    call("param.declare", name="b", value=1.0, min=0.1, max=5)
    call("plot.explicit", expr="a*sin(x)", var="x", id="wave")
    call("plot.explicit", expr="tan(b*x)", var="x", domain=[-5, 5], id="tan")
    call("plot.explicit", expr="x**2/10", var="x", id="fixed")
    call("plot.tangent", of="wave", at=1, id="tangent")
    call("scene.highlight", target="tan", id="hl")
    call("note.label", text="here", at="wave", id="label")
    return e


def test_edges_come_from_content():
    e = scene()
    graph = e.scene.graph()
    assert graph["objects"]["wave"] == {"kind": "explicit", "origin": None, "params": ["a"], "objects": []}
    assert graph["objects"]["fixed"]["params"] == []
    assert graph["objects"]["tangent"]["params"] == ["a"]
    assert graph["objects"]["tangent"]["objects"] == ["wave"]
    assert graph["objects"]["hl"] == {"kind": "highlight", "origin": None, "params": [], "objects": ["tan"]}
    assert graph["objects"]["label"]["objects"] == ["wave"]
    # Dependents follow references: the highlight on `tan` moves with b, the label on `wave` with a.
    assert graph["params"] == {"a": ["wave", "tangent", "label"], "b": ["tan", "hl"]}


def test_state_lists_each_objects_dependencies():
    e = scene()
    by_id = {o["id"]: o for o in e.state()["objects"] + e.state()["notes"]}
    assert by_id["wave"]["deps"] == {"params": ["a"], "objects": []}
    assert by_id["hl"]["deps"] == {"params": [], "objects": ["tan"]}


def test_a_parameter_change_reports_only_what_it_affects():
    e = scene()
    before = e.scene.hash()
    result = e.call("param.set", {"values": {"a": 2.0}})
    assert result["diff"]["params"]["changed"] == {"a": {"value": 2.0, "min": -3.0, "max": 3.0}}
    assert result["diff"]["params"]["affected"] == ["wave", "tangent", "label"]
    assert "objects" not in result["diff"]  # the objects themselves are unchanged: they hold `a`, not its value
    assert e.scene.hash() != before
    # Setting it back gives the scene it was: the hash is of content, not of history.
    e.call("param.set", {"values": {"a": 1.0}})
    assert e.scene.hash() == before


def test_declaring_a_parameter_affects_nothing_yet():
    e = Engine()
    result = e.call("param.declare", {"name": "k", "value": 1.0, "min": 0, "max": 2})
    assert result["diff"]["params"]["affected"] == []


def test_earlier_revisions_are_untouched_by_later_changes():
    e = scene()
    revision = e.scene.revision
    e.call("param.set", {"values": {"a": 2.5}})
    e.call("scene.style", {"target": "wave", "style": {"color": "red"}})
    restored = e.restore(revision)
    assert e.scene.params["a"].value == 1.0
    assert e.scene.objects["wave"].style == {}
    assert restored["diff"]["params"]["affected"] == ["wave", "tangent", "label"]


def test_samples_are_reused_unless_something_they_depend_on_moved():
    e = scene()
    first = {name: sample(e, name, VIEW) for name in ("wave", "tan", "fixed")}
    assert not any(r["stats"].get("cached") for r in first.values())

    e.call("param.set", {"values": {"a": 2.0}})
    again = {name: sample(e, name, VIEW) for name in ("wave", "tan", "fixed")}
    assert not again["wave"]["stats"].get("cached")  # depends on a
    assert again["tan"]["stats"]["cached"] and again["fixed"]["stats"]["cached"]
    for name in ("tan", "fixed"):
        assert again[name]["segments"] == first[name]["segments"]
        assert again[name]["status"] == first[name]["status"]
    assert again["wave"]["segments"] != first["wave"]["segments"]

    # An override is a parameter value like any other.
    assert sample(e, "wave", VIEW, params={"a": 1.0})["segments"] == first["wave"]["segments"]
    # Another view is another sample.
    assert not sample(e, "fixed", {"x": [-5, 5], "y": [-6, 6]})["stats"].get("cached")


def test_a_cached_sample_is_what_a_fresh_engine_computes():
    e = scene()
    sample(e, "tan", VIEW)
    e.call("param.set", {"values": {"a": 2.0}})
    cached = sample(e, "tan", VIEW)
    assert cached["stats"]["cached"]

    fresh = scene()
    fresh.call("param.set", {"values": {"a": 2.0}})
    computed = sample(fresh, "tan", VIEW)
    assert cached["segments"] == computed["segments"] and cached["markers"] == computed["markers"]


def test_restyling_does_not_reuse_a_sample_for_other_content():
    e = scene()
    sample(e, "wave", VIEW)
    e.call("plot.explicit", {"expr": "a*cos(x)", "var": "x", "id": "wave"})
    assert not sample(e, "wave", VIEW)["stats"].get("cached")


def test_many_objects_cost_about_the_same_each():
    """A call's cost must not grow with the number of objects already in the scene."""
    import time

    e = Engine()

    def add(n: int) -> float:
        started = time.perf_counter()
        for i in range(n):
            result = e.call("plot.vector", {"origin": [i, 0], "vector": [1, "1/2"]})
            assert result["status"] == "ok"
        return (time.perf_counter() - started) / n

    first = add(100)
    add(600)
    late = add(100)
    assert late < first * 4 + 0.002, (first, late)
