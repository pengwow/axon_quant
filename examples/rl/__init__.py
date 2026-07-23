"""axon_quant RL 训练示例(0.9.0 D1.3)。

每个 example 演示一个端到端训练路径(CartPole 烟雾 / spot 单 leg /
spot+perp 套利 demo)。完整跑需要 ray + torch + stable-baselines3 等
重型依赖(在 `axon-quant[rl]` extra 中);demo 脚本本身是 standalone 的,
按 `__main__` 跑时,缺依赖会报清晰错误。
"""
