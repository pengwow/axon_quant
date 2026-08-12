"""SFT 训练脚本骨架（0.11.0 E11）

基于 HuggingFace TRL 的 SFTTrainer。
~80 行核心逻辑，不实际跑训练，验证 import + 参数解析。

用法:
    python -m axon_quant.training.sft.train --data train.jsonl --model Qwen/Qwen2.5-1.5B --output_dir ./sft_out
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="SFT Training (TRL)")
    parser.add_argument("--data", type=str, required=True, help="JSONL 训练数据路径")
    parser.add_argument("--model", type=str, default="Qwen/Qwen2.5-1.5B", help="基座模型")
    parser.add_argument("--output_dir", type=str, default="./sft_output", help="输出目录")
    parser.add_argument("--epochs", type=int, default=3, help="训练轮数")
    parser.add_argument("--batch_size", type=int, default=4, help="batch size")
    parser.add_argument("--lr", type=float, default=2e-5, help="学习率")
    parser.add_argument("--max_seq_len", type=int, default=512, help="最大序列长度")
    parser.add_argument("--dry_run", action="store_true", help="只验证参数，不实际训练")
    return parser.parse_args()


def main():
    args = parse_args()

    # 验证数据文件存在
    data_path = Path(args.data)
    if not data_path.exists():
        print(f"ERROR: data file not found: {data_path}", file=sys.stderr)
        sys.exit(1)

    # 统计样本数
    with open(data_path) as f:
        n_samples = sum(1 for line in f if line.strip())
    print(f"Data: {data_path} ({n_samples} samples)")
    print(f"Model: {args.model}")
    print(f"Output: {args.output_dir}")
    print(f"Epochs: {args.epochs}, BS: {args.batch_size}, LR: {args.lr}")

    if args.dry_run:
        print("DRY RUN: skipping actual training")
        return

    # ─── 实际训练（需要 trl + transformers + datasets）───
    try:
        from datasets import load_dataset
        from transformers import AutoModelForCausalLM, AutoTokenizer
        from trl import SFTConfig, SFTTrainer
    except ImportError as e:
        print(f"ERROR: missing dependency: {e}", file=sys.stderr)
        print("Install: pip install trl transformers datasets", file=sys.stderr)
        sys.exit(1)

    # 加载数据
    dataset = load_dataset("json", data_files=str(data_path), split="train")
    print(f"Loaded dataset: {len(dataset)} samples")

    # 加载模型
    tokenizer = AutoTokenizer.from_pretrained(args.model, trust_remote_code=True)
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    model = AutoModelForCausalLM.from_pretrained(args.model, trust_remote_code=True)

    # SFT 配置
    sft_config = SFTConfig(
        output_dir=args.output_dir,
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        learning_rate=args.lr,
        max_seq_length=args.max_seq_len,
        logging_steps=10,
        save_strategy="epoch",
        dataset_text_field=None,  # 使用 messages 格式
    )

    # 训练
    trainer = SFTTrainer(
        model=model,
        args=sft_config,
        train_dataset=dataset,
        processing_class=tokenizer,
    )

    print("Starting training...")
    trainer.train()
    trainer.save_model(args.output_dir)
    print(f"Training complete. Model saved to {args.output_dir}")


if __name__ == "__main__":
    main()
