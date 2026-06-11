"""Checkpoint 管理与容错训练器。

设计：
- **本地模式**：使用 JSON 文件存储 checkpoint 元数据
- **mock trainer**：trainer.algo.train() 返回合成 metrics
- **自动清理**：保留最近 N 个 checkpoint，删除旧的
"""

from __future__ import annotations

import json
import logging
import shutil
import time
from pathlib import Path
from typing import Any

from .types import CheckpointConfig

logger = logging.getLogger(__name__)


class CheckpointManager:
    """Checkpoint 管理器。"""

    def __init__(self, config: CheckpointConfig):
        self.config = config
        self.checkpoint_dir = Path(config.checkpoint_dir)
        self.checkpoint_dir.mkdir(parents=True, exist_ok=True)

    def save_checkpoint(
        self,
        algo: Any,
        iteration: int,
        metrics: dict | None = None,
    ) -> str:
        """保存 checkpoint（mock 模式下仅写元数据 JSON）。"""
        timestamp = int(time.time() * 1000)
        ckpt_name = f"checkpoint_iter{iteration}_{timestamp}"
        meta_path = self.checkpoint_dir / f"{ckpt_name}.meta.json"

        metadata = {
            "iteration": iteration,
            "timestamp": timestamp,
            "metrics": metrics or {},
            "checkpoint_path": str(meta_path),
        }
        with open(meta_path, "w", encoding="utf-8") as f:
            json.dump(metadata, f, indent=2, default=str)

        if algo is not None and hasattr(algo, "save"):
            try:
                algo.save(self.checkpoint_dir / ckpt_name)
            except Exception as e:  # noqa: BLE001
                logger.warning("algo.save failed: %s", e)

        self._cleanup_old_checkpoints()
        logger.info("Checkpoint saved: %s", meta_path)
        return str(meta_path)

    def find_latest_checkpoint(self) -> str | None:
        """查找最新的 checkpoint。"""
        meta_files = sorted(
            self.checkpoint_dir.glob("checkpoint_*.meta.json"),
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        if not meta_files:
            return None
        return str(meta_files[0])

    def restore_checkpoint(
        self, algo: Any = None, checkpoint_path: str | None = None
    ) -> dict:
        """恢复 checkpoint。"""
        if checkpoint_path is None:
            checkpoint_path = self.find_latest_checkpoint()
        if checkpoint_path is None:
            logger.warning("No checkpoint found, starting fresh")
            return {}
        meta_path = Path(checkpoint_path)
        if not meta_path.exists():
            return {}
        with open(meta_path, "r", encoding="utf-8") as f:
            meta = json.load(f)
        if algo is not None and hasattr(algo, "restore"):
            try:
                algo.restore(meta.get("checkpoint_path", ""))
            except Exception as e:  # noqa: BLE001
                logger.warning("algo.restore failed: %s", e)
        logger.info("Restored from: %s", checkpoint_path)
        return meta

    def _cleanup_old_checkpoints(self) -> None:
        """删除超过 keep_checkpoints_num 的旧 checkpoint。"""
        meta_files = sorted(
            self.checkpoint_dir.glob("checkpoint_*.meta.json"),
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        for old_meta in meta_files[self.config.keep_checkpoints_num :]:
            old_meta.unlink(missing_ok=True)
            # 同时删除关联的 checkpoint 目录
            ckpt_name = old_meta.name.replace(".meta.json", "")
            ckpt_dir = self.checkpoint_dir / ckpt_name
            if ckpt_dir.is_dir():
                shutil.rmtree(ckpt_dir, ignore_errors=True)
            logger.info("Removed old checkpoint: %s", old_meta)


class FaultTolerantTrainer:
    """容错训练器。"""

    def __init__(self, trainer: Any, checkpoint_config: CheckpointConfig):
        self.trainer = trainer
        self.ckpt_manager = CheckpointManager(checkpoint_config)
        self.checkpoint_config = checkpoint_config
        self.start_iteration = 0
        self._retry_count = 0

    def train_with_recovery(
        self,
        num_iterations: int,
        checkpoint_interval: int = 10,
    ) -> dict:
        """带故障恢复的训练。"""
        # 尝试恢复
        algo = getattr(self.trainer, "algorithm", None) or getattr(
            self.trainer, "algo", None
        )
        metadata = self.ckpt_manager.restore_checkpoint(algo=algo)
        if metadata:
            self.start_iteration = metadata.get("iteration", 0) + 1
            logger.info("Resuming from iteration %d", self.start_iteration)

        results = []
        for i in range(self.start_iteration, num_iterations):
            try:
                # 优先调用 train(num_iterations=1) 接口
                if hasattr(self.trainer, "train"):
                    result = self.trainer.train(num_iterations=1, checkpoint_interval=1)
                else:
                    # 无 train 接口时使用 mock 合成 result
                    result = {
                        "env_runners": {"episode_reward_mean": 1.0 + 0.01 * i},
                        "iteration": i + 1,
                    }
                # train() 通常返回包含 results 列表的 dict；适配 FaultTolerantTrainer
                if isinstance(result, dict) and "results" in result and isinstance(
                    result["results"], list
                ):
                    # 取最后一个 iteration 的 result
                    inner = result["results"][-1] if result["results"] else result
                else:
                    inner = result
                results.append(inner)
            except Exception as e:  # noqa: BLE001
                logger.error("Worker failed at iter %d: %s", i, e)
                self._retry_count += 1
                if self._retry_count > self.checkpoint_config.max_retries:
                    raise
                continue

            # 定期 checkpoint
            if (i + 1) % checkpoint_interval == 0:
                metrics = {
                    "episode_reward_mean": inner.get("env_runners", {}).get(
                        "episode_reward_mean", 0.0
                    )
                }
                self.ckpt_manager.save_checkpoint(algo, i + 1, metrics)

        return {
            "iterations": len(results),
            "start_iteration": self.start_iteration,
            "final_reward": (
                results[-1].get("env_runners", {}).get("episode_reward_mean", 0.0)
                if results
                else 0.0
            ),
            "results": results,
        }
