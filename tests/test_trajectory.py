"""Test TrajectoryRecorder Python bindings."""

import json
import os
import tempfile

import pytest


class TestTrajectoryRecorder:
    """Test PyTrajectoryRecorder functionality."""

    def test_construct(self):
        """Test basic construction."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        assert recorder is not None
        assert recorder.bar_count() == 0

    def test_run_id(self):
        """Test run_id getter and setter."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        run_id = recorder.get_run_id()
        assert run_id is not None
        assert len(run_id) > 0

        recorder.set_run_id("custom-run-id")
        assert recorder.get_run_id() == "custom-run-id"

    def test_record(self):
        """Test recording a bar."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        recorder.record(0, 1234567890, "bar 0")
        assert recorder.bar_count() == 1

    def test_record_with_action(self):
        """Test recording a bar with action."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        recorder.record(
            bar_id=0,
            ts=1234567890,
            thought="bar 0",
            action={"tool": "place_order", "args": {"symbol": "BTC-USDT", "side": "Buy"}},
            observation="order placed",
            reward=1.0,
            cum_pnl=0.5,
        )
        assert recorder.bar_count() == 1

    def test_record_multiple_bars(self):
        """Test recording multiple bars."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        for i in range(5):
            recorder.record(i, 1234567890 + i * 1000, f"bar {i}")
        assert recorder.bar_count() == 5

    def test_flush(self):
        """Test flushing to file."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        recorder.set_run_id("test-run")
        recorder.record(0, 1234567890, "bar 0")

        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            temp_path = f.name

        try:
            recorder.flush(temp_path)
            assert os.path.exists(temp_path)

            with open(temp_path, "r") as f:
                data = json.load(f)
            assert data["version"] == "0.10.0"
            assert data["run_id"] == "test-run"
            assert data["instrument"] == "BTC-USDT"
            assert data["seed"] == 42
            assert len(data["bars"]) == 1
            assert data["bars"][0]["thought"] == "bar 0"
        finally:
            if os.path.exists(temp_path):
                os.remove(temp_path)

    def test_trajectory_dict(self):
        """Test trajectory() returns correct dict."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        recorder.record(0, 1234567890, "bar 0")

        traj = recorder.trajectory()
        assert traj["version"] == "0.10.0"
        assert traj["instrument"] == "BTC-USDT"
        assert traj["seed"] == 42
        assert len(traj["bars"]) == 1
        assert traj["bars"][0]["bar_id"] == 0
        assert traj["bars"][0]["thought"] == "bar 0"

    def test_repr(self):
        """Test __repr__ method."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        repr_str = repr(recorder)
        assert "TrajectoryRecorder" in repr_str
        assert "run_id" in repr_str
        assert "bars=0" in repr_str

        recorder.record(0, 1234567890, "bar 0")
        repr_str = repr(recorder)
        assert "bars=1" in repr_str

    def test_deterministic_same_run_id(self):
        """Test that same run_id produces identical files."""
        from axon_quant.trajectory import TrajectoryRecorder

        recorder1 = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        recorder1.set_run_id("deterministic-test")
        recorder1.record(0, 1234567890, "bar 0")

        recorder2 = TrajectoryRecorder(42, "BTC-USDT", "mock", "test")
        recorder2.set_run_id("deterministic-test")
        recorder2.record(0, 1234567890, "bar 0")

        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f1:
            path1 = f1.name
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f2:
            path2 = f2.name

        try:
            recorder1.flush(path1)
            recorder2.flush(path2)

            with open(path1, "r") as f:
                content1 = f.read()
            with open(path2, "r") as f:
                content2 = f.read()

            assert content1 == content2, "same run_id should produce identical files"
        finally:
            if os.path.exists(path1):
                os.remove(path1)
            if os.path.exists(path2):
                os.remove(path2)
