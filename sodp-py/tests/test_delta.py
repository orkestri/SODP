"""Unit tests for the delta application module."""

from sodp.delta import apply_ops, _parse_path


# ── Path parsing ────────────────────────────────────────────────────────────────

def test_parse_root():
    assert _parse_path("/") == []


def test_parse_single():
    assert _parse_path("/name") == ["name"]


def test_parse_nested():
    assert _parse_path("/position/x") == ["position", "x"]


def test_parse_deep():
    assert _parse_path("/a/b/c/d") == ["a", "b", "c", "d"]


# ── Root operations ─────────────────────────────────────────────────────────────

def test_add_root():
    result = apply_ops(None, [{"op": "ADD", "path": "/", "value": {"x": 1}}])
    assert result == {"x": 1}


def test_update_root():
    result = apply_ops({"x": 1}, [{"op": "UPDATE", "path": "/", "value": {"x": 2}}])
    assert result == {"x": 2}


def test_remove_root():
    result = apply_ops({"x": 1}, [{"op": "REMOVE", "path": "/"}])
    assert result is None


# ── Field operations ────────────────────────────────────────────────────────────

def test_add_field():
    state = {"name": "Alice"}
    result = apply_ops(state, [{"op": "ADD", "path": "/score", "value": 42}])
    assert result == {"name": "Alice", "score": 42}
    # Original should not be mutated
    assert "score" not in state


def test_update_field():
    state = {"name": "Alice", "health": 100}
    result = apply_ops(state, [{"op": "UPDATE", "path": "/health", "value": 80}])
    assert result == {"name": "Alice", "health": 80}


def test_remove_field():
    state = {"name": "Alice", "temp": True}
    result = apply_ops(state, [{"op": "REMOVE", "path": "/temp"}])
    assert result == {"name": "Alice"}


def test_remove_nonexistent_field():
    state = {"name": "Alice"}
    result = apply_ops(state, [{"op": "REMOVE", "path": "/missing"}])
    assert result == {"name": "Alice"}


# ── Nested operations ──────────────────────────────────────────────────────────

def test_update_nested():
    state = {"position": {"x": 0, "y": 0}}
    result = apply_ops(state, [{"op": "UPDATE", "path": "/position/x", "value": 5}])
    assert result == {"position": {"x": 5, "y": 0}}


def test_add_nested_materializes_intermediates():
    state = {}
    result = apply_ops(state, [{"op": "ADD", "path": "/a/b", "value": 1}])
    assert result == {"a": {"b": 1}}


def test_remove_nested():
    state = {"position": {"x": 1, "y": 2, "z": 3}}
    result = apply_ops(state, [{"op": "REMOVE", "path": "/position/z"}])
    assert result == {"position": {"x": 1, "y": 2}}


# ── Multiple operations ────────────────────────────────────────────────────────

def test_multiple_ops_in_sequence():
    state = {"score": 0}
    ops = [
        {"op": "UPDATE", "path": "/score", "value": 10},
        {"op": "ADD", "path": "/combo", "value": 3},
    ]
    result = apply_ops(state, ops)
    assert result == {"score": 10, "combo": 3}


def test_empty_ops():
    state = {"x": 1}
    result = apply_ops(state, [])
    assert result == {"x": 1}


# ── Immutability ────────────────────────────────────────────────────────────────

def test_does_not_mutate_original():
    state = {"a": {"b": 1}}
    result = apply_ops(state, [{"op": "UPDATE", "path": "/a/b", "value": 2}])
    assert state == {"a": {"b": 1}}
    assert result == {"a": {"b": 2}}


def test_does_not_mutate_nested_original():
    inner = {"x": 1, "y": 2}
    state = {"pos": inner}
    apply_ops(state, [{"op": "UPDATE", "path": "/pos/x", "value": 99}])
    assert inner == {"x": 1, "y": 2}
