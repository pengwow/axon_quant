# 场景四 — 传统策略迁移与自定义策略

> **完整可运行示例**: [`examples/16_traditional_strategy/traditional_strategy_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/16_traditional_strategy/traditional_strategy_demo.py)
> 覆盖动量策略、均值回归、趋势跟踪三种经典策略的信号生成与回测对比。

本文档展示如何在 AXON 框架中实现和运行传统量化策略（不依赖 RL），以及如何将传统信号与 AI 模块结合，构建混合模式交易系统。

---

## 1. 纯 Python 策略（不依赖 RL）

AXON 的交易环境 (`TradingEnv`) 和回测引擎 (`BacktestEngine`) 完全支持纯规则策略。你可以像使用普通 Python 类一样实现策略，无需了解 RL 的任何细节。

### 1.1 SimpleMomentumStrategy 完整代码

```python
"""
SimpleMomentumStrategy - 纯 Python 动量策略示例

本策略不依赖任何 RL 模块，直接基于价格数据计算动量信号，
通过 AXON 的 BacktestEngine 进行回测验证。
"""

from dataclasses import dataclass
from typing import List, Optional, Dict
from decimal import Decimal
import numpy as np

from axon_quant import (
    BacktestEngine,
    MarketBar,
    Order,
    OrderType,
    Side,
)


@dataclass
class Signal:
    """策略信号结构体。"""
    action: str           # "buy" / "sell" / "hold"
    confidence: float     # 0.0 ~ 1.0
    target_position: float  # 目标仓位比例 0.0 ~ 1.0
    reason: str           # 信号原因说明


class SimpleMomentumStrategy:
    """
    简单动量策略：基于双均线交叉 + 成交量确认。
    
    策略逻辑:
    1. 计算短期均线 (MA5) 和长期均线 (MA20)
    2. 当 MA5 上穿 MA20 且成交量放大 1.5 倍时，产生买入信号
    3. 当 MA5 下穿 MA20 且成交量放大 1.5 倍时，产生卖出信号
    4. 其他情况持仓不动
    
    参数:
        short_window: 短期均线窗口，默认 5
        long_window: 长期均线窗口，默认 20
        volume_threshold: 成交量放大阈值，默认 1.5
        position_size: 每次交易仓位比例，默认 0.5 (50%)
    """
    
    def __init__(
        self,
        short_window: int = 5,
        long_window: int = 20,
        volume_threshold: float = 1.5,
        position_size: float = 0.5,
    ):
        self.short_window = short_window
        self.long_window = long_window
        self.volume_threshold = volume_threshold
        self.position_size = position_size
        
        # 状态跟踪
        self.price_history: List[float] = []
        self.volume_history: List[float] = []
        self.current_position: float = 0.0  # 当前仓位比例 -1.0 ~ 1.0
        self.signals: List[Dict] = []
    
    def on_bar(self, bar: MarketBar) -> Optional[Order]:
        """
        每根 K 线到达时调用，返回交易订单或 None。
        
        Args:
            bar: 当前 K 线数据，包含 open/high/low/close/volume
            
        Returns:
            Order 对象（有信号时）或 None（无信号时）
        """
        # 记录历史数据
        self.price_history.append(bar.close)
        self.volume_history.append(bar.volume)
        
        # 数据不足时不交易
        if len(self.price_history) < self.long_window:
            return None
        
        # 保持窗口长度，防止内存无限增长
        if len(self.price_history) > self.long_window * 2:
            self.price_history = self.price_history[-self.long_window * 2:]
            self.volume_history = self.volume_history[-self.long_window * 2:]
        
        # 计算均线
        ma_short = np.mean(self.price_history[-self.short_window:])
        ma_long = np.mean(self.price_history[-self.long_window:])
        
        # 计算成交量均值
        avg_volume = np.mean(self.volume_history[-self.long_window:])
        volume_ratio = bar.volume / avg_volume if avg_volume > 0 else 1.0
        
        # 计算上一期的均线（用于判断交叉）
        if len(self.price_history) >= self.long_window + 1:
            prev_ma_short = np.mean(self.price_history[-(self.short_window + 1):-1])
            prev_ma_long = np.mean(self.price_history[-(self.long_window + 1):-1])
        else:
            prev_ma_short = ma_short
            prev_ma_long = ma_long
        
        # 判断均线交叉
        golden_cross = prev_ma_short <= prev_ma_long and ma_short > ma_long
        death_cross = prev_ma_short >= prev_ma_long and ma_short < ma_long
        
        # 成交量确认
        volume_confirmed = volume_ratio >= self.volume_threshold
        
        # 生成信号
        order = None
        if golden_cross and volume_confirmed and self.current_position <= 0:
            # 金叉 + 放量：买入
            signal = Signal(
                action="buy",
                confidence=min(volume_ratio / 2.0, 1.0),
                target_position=self.position_size,
                reason=f"MA{self.short_window} 上穿 MA{self.long_window}, "
                       f"成交量放大 {volume_ratio:.2f} 倍",
            )
            order = self._create_order(signal, bar)
            self.current_position = self.position_size
            
        elif death_cross and volume_confirmed and self.current_position >= 0:
            # 死叉 + 放量：卖出
            signal = Signal(
                action="sell",
                confidence=min(volume_ratio / 2.0, 1.0),
                target_position=-self.position_size,
                reason=f"MA{self.short_window} 下穿 MA{self.long_window}, "
                       f"成交量放大 {volume_ratio:.2f} 倍",
            )
            order = self._create_order(signal, bar)
            self.current_position = -self.position_size
        
        # 记录信号日志
        if order:
            self.signals.append({
                "timestamp": bar.timestamp,
                "action": signal.action,
                "confidence": signal.confidence,
                "reason": signal.reason,
                "price": bar.close,
            })
        
        return order
    
    def _create_order(self, signal: Signal, bar: MarketBar) -> Order:
        """根据信号创建订单对象。"""
        side = Side.Buy if signal.action == "buy" else Side.Sell
        
        # 计算订单数量（简化：按目标仓位比例计算）
        notional = 10000.0 * abs(signal.target_position)  # 假设本金 10000 USDT
        quantity = Decimal(str(notional / bar.close))
        
        return Order(
            # 0.6.0 Python `axon_quant.oms.Order` 字段集:`(symbol, side, order_type, quantity, price, idempotency_key=None)`
            symbol="BTCUSDT",
            side=side,
            order_type=OrderType.Market,
            quantity=quantity,
            price=Decimal("0"),  # 市价单 price 传 0
            idempotency_key=f"momentum-{signal.confidence:.2f}",
        )
    
    def get_stats(self) -> Dict:
        """获取策略统计信息。"""
        buy_signals = [s for s in self.signals if s["action"] == "buy"]
        sell_signals = [s for s in self.signals if s["action"] == "sell"]
        
        return {
            "total_signals": len(self.signals),
            "buy_signals": len(buy_signals),
            "sell_signals": len(sell_signals),
            "avg_confidence": np.mean([s["confidence"] for s in self.signals]) if self.signals else 0.0,
        }


# ==================== 回测执行 ====================

def run_backtest():
    """
    使用 AXON BacktestEngine 对 SimpleMomentumStrategy 进行回测。
    """
    # 准备历史 K 线数据（实际使用时应从 axon-data 加载）
    bars = load_historical_bars("BTCUSDT", "1h", start="2024-01-01", end="2024-06-01")
    
    # 创建策略实例
    strategy = SimpleMomentumStrategy(
        short_window=5,
        long_window=20,
        volume_threshold=1.5,
        position_size=0.5,
    )
    
    # 创建回测引擎
    engine = BacktestEngine(
        initial_capital=10000.0,
        transaction_cost=0.001,  # 10 bps
        slippage=0.0005,         # 5 bps
    )
    
    # 运行回测
    for bar in bars:
        order = strategy.on_bar(bar)
        if order:
            engine.submit_order(order)
        engine.on_bar(bar)
    
    # 输出结果
    stats = strategy.get_stats()
    print(f"信号总数: {stats['total_signals']}")
    print(f"买入信号: {stats['buy_signals']}")
    print(f"卖出信号: {stats['sell_signals']}")
    print(f"平均置信度: {stats['avg_confidence']:.2%}")
    print(f"最终净值: {engine.portfolio_value:.2f} USDT")
    print(f"收益率: {(engine.portfolio_value / 10000.0 - 1) * 100:.2f}%")
```

---

## 2. AI 辅助策略：传统信号触发 LLM 复核

混合模式的核心思想是：传统策略负责快速发现机会，LLM 负责深度验证。这样既保留了传统策略的低延迟，又获得了 AI 的语义理解能力。

### 2.1 混合模式架构

```text
┌─────────────────────────────────────────────────────────────┐
│                    HybridStrategy                           │
│                                                             │
│  ┌─────────────────┐      ┌─────────────────────────────┐  │
│  │  Traditional    │      │  LLM Verification           │  │
│  │  Signal Layer   │ ───► │  (News / Sentiment / Macro)│  │
│  │                 │      │                             │  │
│  │  - MA Cross     │      │  - "BTC 突破 2 月新高，      │  │
│  │  - Bollinger    │      │    但美联储即将加息，        │  │
│  │  - RSI Diverge  │      │    是否应跟进？"             │  │
│  └─────────────────┘      └─────────────────────────────┘  │
│           │                           │                     │
│           └───────────┬───────────────┘                     │
│                       ▼                                     │
│              ┌─────────────────┐                           │
│              │  Decision Gate  │                           │
│              │  (信号 + LLM)   │                           │
│              └─────────────────┘                           │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 混合策略完整代码

```python
"""
HybridStrategy - 传统信号 + LLM 复核的混合策略

工作流程:
1. 传统技术指标产生初始信号
2. LLM 分析当前市场环境（新闻、情绪、宏观）
3. 只有当传统信号和 LLM 意见一致时才执行交易
4. LLM 同时生成交易理由，存入审计日志
"""

import asyncio
from typing import Optional, List
from decimal import Decimal

from axon_quant import (
    MarketBar, Order, OrderType, Side,
    LLMBackend, Message, ToolDefinition,
    DecisionRecorder, DecisionRecord,
)


class HybridStrategy:
    """
    混合策略：传统动量信号 + LLM 市场复核。
    
    参数:
        llm_backend: LLM 后端实例 (OpenAI / DeepSeek / 本地模型)
        strategy: 底层传统策略实例
        consensus_threshold: 共识阈值，LLM 置信度需超过此值才执行
        max_llm_latency_ms: LLM 最大等待时间，超时则仅按传统信号执行
    """
    
    def __init__(
        self,
        llm_backend: LLMBackend,
        strategy: SimpleMomentumStrategy,
        consensus_threshold: float = 0.7,
        max_llm_latency_ms: float = 2000.0,
    ):
        self.llm = llm_backend
        self.strategy = strategy
        self.consensus_threshold = consensus_threshold
        self.max_llm_latency_ms = max_llm_latency_ms
        self.recorder = None  # 可选：决策记录器
        
        # 统计
        self.traditional_signals = 0
        self.llm_approved = 0
        self.llm_rejected = 0
        self.llm_timeouts = 0
    
    async def on_bar(self, bar: MarketBar) -> Optional[Order]:
        """
        每根 K 线到达时的完整决策流程。
        
        步骤:
        1. 传统策略产生信号
        2. 构造 LLM 提示词（包含价格数据 + 信号信息）
        3. 异步调用 LLM 进行复核
        4. 综合决策：传统信号 + LLM 意见
        5. 记录决策理由
        """
        # 步骤 1: 传统策略信号
        raw_order = self.strategy.on_bar(bar)
        if raw_order is None:
            return None
        
        self.traditional_signals += 1
        
        # 提取信号信息
        signal_info = self.strategy.signals[-1]
        
        # 步骤 2: 构造 LLM 提示词
        prompt = self._build_prompt(bar, signal_info)
        
        # 步骤 3: 异步调用 LLM（带超时）
        try:
            llm_response = await asyncio.wait_for(
                self._query_llm(prompt),
                timeout=self.max_llm_latency_ms / 1000.0,
            )
            llm_approved = llm_response.get("approve", False)
            llm_confidence = llm_response.get("confidence", 0.0)
            llm_reason = llm_response.get("reason", "")
            
        except asyncio.TimeoutError:
            # LLM 超时：保守策略，仅按传统信号执行（但降低仓位）
            self.llm_timeouts += 1
            print(f"[警告] LLM 复核超时，降低仓位至 30% 执行")
            raw_order.quantity = raw_order.quantity * Decimal("0.3")
            self._record_decision(bar, signal_info, "timeout", 0.0, "LLM 超时，保守执行")
            return raw_order
        
        # 步骤 4: 综合决策
        if llm_approved and llm_confidence >= self.consensus_threshold:
            # 双重确认通过：全额执行
            self.llm_approved += 1
            print(f"[通过] 传统信号 + LLM 一致 (置信度 {llm_confidence:.1%})")
            self._record_decision(bar, signal_info, "approved", llm_confidence, llm_reason)
            return raw_order
        else:
            # LLM 反对或置信度不足：放弃交易
            self.llm_rejected += 1
            print(f"[放弃] LLM 反对 ({llm_reason}), 不执行交易")
            self._record_decision(bar, signal_info, "rejected", llm_confidence, llm_reason)
            return None
    
    def _build_prompt(self, bar: MarketBar, signal: dict) -> str:
        """
        构造 LLM 复核提示词。
        
        包含信息:
        - 当前价格和技术指标
        - 传统策略的信号和理由
        - 要求 LLM 从宏观/新闻角度评估
        """
        prices = self.strategy.price_history[-20:]
        price_change_24h = (prices[-1] - prices[-24]) / prices[-24] * 100 if len(prices) >= 24 else 0
        
        prompt = f"""你是一位资深加密货币交易员，请对以下交易信号进行复核评估。

## 市场数据
- 交易对: BTCUSDT
- 当前价格: {bar.close:.2f} USDT
- 24h 涨跌: {price_change_24h:+.2f}%
- 成交量: {bar.volume:.2f}

## 技术信号
- 信号类型: {signal['action'].upper()}
- 信号置信度: {signal['confidence']:.1%}
- 信号理由: {signal['reason']}

## 你的任务
1. 从宏观经济、市场情绪、新闻事件角度分析当前是否适合执行该交易
2. 给出明确的通过/拒绝建议
3. 提供你的分析理由（100 字以内）

请以 JSON 格式回复:
{{
    "approve": true/false,
    "confidence": 0.0-1.0,
    "reason": "你的分析理由"
}}
"""
        return prompt
    
    async def _query_llm(self, prompt: str) -> dict:
        """
        调用 LLM 后端获取复核意见。
        
        实际实现中，这里会调用 axon-llm 的 complete() 或 complete_with_tools()。
        """
        messages = [
            Message(role="system", content="你是一位专业的加密货币交易分析师。"),
            Message(role="user", content=prompt),
        ]
        
        response = await self.llm.complete(messages)
        
        # 解析 LLM 返回的 JSON
        import json
        try:
            result = json.loads(response.content)
            return result
        except json.JSONDecodeError:
            # 如果 LLM 没有返回合法 JSON，保守处理为拒绝
            return {"approve": False, "confidence": 0.0, "reason": "LLM 返回格式错误"}
    
    def _record_decision(self, bar: MarketBar, signal: dict, outcome: str, llm_conf: float, reason: str):
        """记录决策到审计系统（可选）。"""
        if self.recorder:
            self.recorder.record_async(DecisionRecord(
                decision_id=f"hybrid_{bar.timestamp.isoformat()}",
                timestamp=bar.timestamp.timestamp(),
                observation={"price": bar.close, "volume": bar.volume},
                action=signal,
                model_version="hybrid_v1",
                metadata={
                    "outcome": outcome,
                    "llm_confidence": llm_conf,
                    "llm_reason": reason,
                },
            ))
    
    def get_stats(self) -> dict:
        """获取混合策略统计。"""
        return {
            "traditional_signals": self.traditional_signals,
            "llm_approved": self.llm_approved,
            "llm_rejected": self.llm_rejected,
            "llm_timeouts": self.llm_timeouts,
            "approval_rate": self.llm_approved / max(self.traditional_signals, 1),
        }


# ==================== 使用示例 ====================

async def run_hybrid_strategy():
    """运行混合策略示例。"""
    from axon_quant import OpenAIBackend
    
    # 创建 LLM 后端
    llm = OpenAIBackend(
        api_key="YOUR_API_KEY",
        model="deepseek-chat",
        base_url="https://api.deepseek.com",
    )
    
    # 创建传统策略
    traditional = SimpleMomentumStrategy(
        short_window=5,
        long_window=20,
        volume_threshold=1.5,
        position_size=0.5,
    )
    
    # 创建混合策略
    hybrid = HybridStrategy(
        llm_backend=llm,
        strategy=traditional,
        consensus_threshold=0.7,
        max_llm_latency_ms=2000,
    )
    
    # 模拟 K 线流
    bars = load_historical_bars("BTCUSDT", "1h")
    
    for bar in bars:
        order = await hybrid.on_bar(bar)
        if order:
            print(f"执行订单: {order.side} {order.quantity} @ {bar.close}")
    
    # 输出统计
    stats = hybrid.get_stats()
    print(f"\n混合策略统计:")
    print(f"  传统信号数: {stats['traditional_signals']}")
    print(f"  LLM 通过: {stats['llm_approved']}")
    print(f"  LLM 拒绝: {stats['llm_rejected']}")
    print(f"  LLM 超时: {stats['llm_timeouts']}")
    print(f"  通过率: {stats['approval_rate']:.1%}")
```

---

## 3. 业内常见策略模式

以下展示三种经典策略模式在 AXON 中的完整实现，每种都包含与 AI 模块的集成方式。

### 3.1 趋势跟踪：均线突破 + LLM 新闻验证

```python
"""
TrendFollowingStrategy - 趋势跟踪策略

核心逻辑:
- 入场: 价格突破 N 周期高点 + 均线多头排列
- 出场: 价格跌破 N 周期低点 或 均线空头排列
- LLM 增强: 突破时查询相关新闻，验证突破有效性
"""

import numpy as np
from typing import Optional, List
from decimal import Decimal

from axon_quant import (
    MarketBar, Order, OrderType, Side,
)


class TrendFollowingStrategy:
    """
    趋势跟踪策略：均线突破 + LLM 新闻验证。
    
    参数:
        lookback: 突破回看周期，默认 20
        ma_fast: 快速均线周期，默认 10
        ma_slow: 慢速均线周期，默认 30
        atr_period: ATR 周期（用于止损），默认 14
        risk_per_trade: 每笔交易风险比例，默认 1%
    """
    
    def __init__(
        self,
        lookback: int = 20,
        ma_fast: int = 10,
        ma_slow: int = 30,
        atr_period: int = 14,
        risk_per_trade: float = 0.01,
    ):
        self.lookback = lookback
        self.ma_fast = ma_fast
        self.ma_slow = ma_slow
        self.atr_period = atr_period
        self.risk_per_trade = risk_per_trade
        
        self.prices: List[float] = []
        self.highs: List[float] = []
        self.lows: List[float] = []
        self.position: Optional[dict] = None
    
    def on_bar(self, bar: MarketBar) -> Optional[Order]:
        """主决策逻辑。"""
        self.prices.append(bar.close)
        self.highs.append(bar.high)
        self.lows.append(bar.low)
        
        if len(self.prices) < self.ma_slow:
            return None
        
        # 计算指标
        ma_fast = np.mean(self.prices[-self.ma_fast:])
        ma_slow = np.mean(self.prices[-self.ma_slow:])
        atr = self._calculate_atr()
        
        # 计算突破水平
        highest_high = max(self.highs[-self.lookback:])
        lowest_low = min(self.lows[-self.lookback:])
        
        order = None
        
        # 多头入场: 突破高点 + 均线多头排列
        if bar.close > highest_high and ma_fast > ma_slow and self.position is None:
            stop_loss = bar.close - 2.0 * atr
            take_profit = bar.close + 3.0 * atr  # 1:1.5 盈亏比
            
            order = self._create_order(
                side=Side.Buy,
                price=bar.close,
                stop_loss=stop_loss,
                take_profit=take_profit,
                reason=f"突破 {self.lookback} 周期高点 {highest_high:.2f}, "
                       f"MA{self.ma_fast}({ma_fast:.2f}) > MA{self.ma_slow}({ma_slow:.2f})",
            )
            self.position = {"side": "long", "entry": bar.close, "sl": stop_loss, "tp": take_profit}
        
        # 空头入场: 跌破低点 + 均线空头排列
        elif bar.close < lowest_low and ma_fast < ma_slow and self.position is None:
            stop_loss = bar.close + 2.0 * atr
            take_profit = bar.close - 3.0 * atr
            
            order = self._create_order(
                side=Side.Sell,
                price=bar.close,
                stop_loss=stop_loss,
                take_profit=take_profit,
                reason=f"跌破 {self.lookback} 周期低点 {lowest_low:.2f}, "
                       f"MA{self.ma_fast}({ma_fast:.2f}) < MA{self.ma_slow}({ma_slow:.2f})",
            )
            self.position = {"side": "short", "entry": bar.close, "sl": stop_loss, "tp": take_profit}
        
        # 出场逻辑
        elif self.position:
            if self.position["side"] == "long":
                if bar.close <= self.position["sl"] or bar.close >= self.position["tp"]:
                    order = self._create_order(
                        side=Side.Sell,
                        price=bar.close,
                        reason="多头止损/止盈",
                    )
                    self.position = None
            else:
                if bar.close >= self.position["sl"] or bar.close <= self.position["tp"]:
                    order = self._create_order(
                        side=Side.Buy,
                        price=bar.close,
                        reason="空头止损/止盈",
                    )
                    self.position = None
        
        return order
    
    def _calculate_atr(self) -> float:
        """计算平均真实波幅 (ATR)。"""
        if len(self.prices) < self.atr_period + 1:
            return 0.0
        
        tr_values = []
        for i in range(-self.atr_period, 0):
            high = self.highs[i]
            low = self.lows[i]
            prev_close = self.prices[i - 1]
            tr = max(high - low, abs(high - prev_close), abs(low - prev_close))
            tr_values.append(tr)
        
        return np.mean(tr_values)
    
    def _create_order(
        self,
        side: Side,
        price: float,
        stop_loss: float = 0.0,
        take_profit: float = 0.0,
        reason: str = "",
    ) -> Order:
        """创建订单。"""
        # 根据风险计算仓位大小
        # 简化: 固定名义金额
        notional = 1000.0
        quantity = Decimal(str(notional / price))
        
        return Order(
            symbol="BTCUSDT",
            side=side,
            order_type=OrderType.Market,
            quantity=quantity,
            price=Decimal("0"),
            idempotency_key=f"trend-{reason[:16]}",
        )


# ==================== LLM 新闻验证集成 ====================

async def verify_with_llm(strategy: TrendFollowingStrategy, llm_backend, bar: MarketBar) -> bool:
    """
    在趋势突破时，使用 LLM 验证新闻面是否支持该方向。
    
    返回 True 表示 LLM 支持该交易方向，False 表示反对或不确定。
    """
    recent_high = max(strategy.highs[-strategy.lookback:])
    recent_low = min(strategy.lows[-strategy.lookback:])
    
    is_breakout_up = bar.close > recent_high
    direction = "上涨" if is_breakout_up else "下跌"
    
    prompt = f"""BTC 刚刚{'突破' if is_breakup_up else '跌破'}了 {strategy.lookback} 周期的{'高点' if is_breakout_up else '低点'}，
价格 {bar.close:.2f} USDT。

请分析:
1. 最近 24 小时是否有重大新闻支持 BTC {direction}?
2. 宏观经济环境（美联储政策、美元指数等）是否支持该方向?
3. 市场情绪（恐惧/贪婪指数、资金费率）如何?

如果基本面支持该方向，回复 "APPROVE"，否则回复 "REJECT"。
简要说明理由（50 字以内）。"""
    
    messages = [Message(role="user", content=prompt)]
    response = await llm_backend.complete(messages)
    
    content = response.content.upper()
    approved = "APPROVE" in content and "REJECT" not in content
    
    print(f"[LLM 验证] {'通过' if approved else '拒绝'} - {response.content[:100]}")
    return approved
```

### 3.2 均值回归：布林带 + RL 仓位优化

```python
"""
MeanReversionStrategy - 均值回归策略

核心逻辑:
- 入场: 价格触及布林带上/下轨 + RSI 超买/超卖
- 仓位: 不固定，使用 RL 模型根据市场状态动态优化仓位大小
- 出场: 价格回归中轨 或 止损
"""

import numpy as np
from typing import Optional, Tuple
from decimal import Decimal

from axon_quant import (
    MarketBar, Order, OrderType, Side,
    TradingEnv, Observation, Action,
)


class MeanReversionStrategy:
    """
    均值回归策略：布林带 + RL 仓位优化。
    
    参数:
        bb_period: 布林带周期，默认 20
        bb_std: 布林带标准差倍数，默认 2.0
        rsi_period: RSI 周期，默认 14
        rsi_overbought: RSI 超买阈值，默认 70
        rsi_oversold: RSI 超卖阈值，默认 30
    """
    
    def __init__(
        self,
        bb_period: int = 20,
        bb_std: float = 2.0,
        rsi_period: int = 14,
        rsi_overbought: float = 70.0,
        rsi_oversold: float = 30.0,
    ):
        self.bb_period = bb_period
        self.bb_std = bb_std
        self.rsi_period = rsi_period
        self.rsi_overbought = rsi_overbought
        self.rsi_oversold = rsi_oversold
        
        self.prices: list[float] = []
        self.position: Optional[str] = None
        
        # RL 仓位优化器（可选）
        self.rl_position_optimizer = None
    
    def on_bar(self, bar: MarketBar) -> Optional[Order]:
        """主决策逻辑。"""
        self.prices.append(bar.close)
        
        if len(self.prices) < self.bb_period:
            return None
        
        # 计算布林带
        middle, upper, lower = self._calculate_bollinger()
        
        # 计算 RSI
        rsi = self._calculate_rsi()
        
        # 计算价格相对于布林带的位置
        bb_position = (bar.close - lower) / (upper - lower) if upper != lower else 0.5
        
        order = None
        
        # 空头信号: 触及上轨 + RSI 超买
        if bb_position >= 0.95 and rsi >= self.rsi_overbought and self.position is None:
            position_size = self._optimize_position_size(bar, "short")
            order = self._create_order(
                side=Side.Sell,
                quantity=position_size,
                reason=f"触及布林带上轨 {upper:.2f}, RSI={rsi:.1f} 超买, "
                       f"仓位={position_size:.2%}",
            )
            self.position = "short"
        
        # 多头信号: 触及下轨 + RSI 超卖
        elif bb_position <= 0.05 and rsi <= self.rsi_oversold and self.position is None:
            position_size = self._optimize_position_size(bar, "long")
            order = self._create_order(
                side=Side.Buy,
                quantity=position_size,
                reason=f"触及布林带下轨 {lower:.2f}, RSI={rsi:.1f} 超卖, "
                       f"仓位={position_size:.2%}",
            )
            self.position = "long"
        
        # 出场: 价格回归中轨附近
        elif self.position:
            if abs(bar.close - middle) / middle < 0.005:
                order = self._create_order(
                    side=Side.Sell if self.position == "long" else Side.Buy,
                    quantity=0.0,  # 平仓
                    reason=f"价格回归中轨 {middle:.2f}, 平仓",
                )
                self.position = None
        
        return order
    
    def _calculate_bollinger(self) -> Tuple[float, float, float]:
        """计算布林带三条线。"""
        window = self.prices[-self.bb_period:]
        middle = np.mean(window)
        std = np.std(window)
        upper = middle + self.bb_std * std
        lower = middle - self.bb_std * std
        return middle, upper, lower
    
    def _calculate_rsi(self) -> float:
        """计算 RSI 指标。"""
        if len(self.prices) < self.rsi_period + 1:
            return 50.0
        
        deltas = np.diff(self.prices[-(self.rsi_period + 1):])
        gains = np.where(deltas > 0, deltas, 0)
        losses = np.where(deltas < 0, -deltas, 0)
        
        avg_gain = np.mean(gains)
        avg_loss = np.mean(losses)
        
        if avg_loss == 0:
            return 100.0
        
        rs = avg_gain / avg_loss
        rsi = 100.0 - (100.0 / (1.0 + rs))
        return rsi
    
    def _optimize_position_size(self, bar: MarketBar, direction: str) -> Decimal:
        """
        使用 RL 模型优化仓位大小。
        
        如果没有 RL 模型，回退到固定仓位。
        """
        if self.rl_position_optimizer is None:
            # 默认固定仓位 30%
            return Decimal("0.3")
        
        # 构造观测
        obs = Observation(
            market_features=[
                bar.close, bar.volume, self._calculate_rsi(),
                *self.prices[-5:],  # 最近 5 个价格
            ],
            technical_indicators=[],
            portfolio_state=PortfolioState(),
            time_features=[],
        )
        
        # RL 模型输出动作（仓位比例 0.0 ~ 1.0）
        action = self.rl_position_optimizer.predict(obs)
        position_ratio = abs(action.target_position)
        
        # 限制最大仓位
        position_ratio = min(position_ratio, 0.5)
        
        return Decimal(str(position_ratio))
    
    def _create_order(self, side: Side, quantity: Decimal, reason: str) -> Order:
        """创建订单。"""
        return Order(
            # 0.6.0 Python `axon_quant.oms.Order` 字段集
            symbol="BTCUSDT",
            side=side,
            order_type=OrderType.Market,
            quantity=quantity,
            price=Decimal("0"),  # 市价单 price 传 0
            idempotency_key=f"mean-rev-{reason[:16]}",
        )


# ==================== RL 仓位优化器训练 ====================

def train_rl_position_optimizer():
    """
    训练一个 RL 模型来优化均值回归策略的仓位大小。
    
    观测空间: 价格、成交量、RSI、布林带位置、持仓状态
    动作空间: 连续值 [-1, 1]，映射到仓位比例 [0, 50%]
    奖励: 交易收益 - 风险惩罚 (回撤、波动率)
    """
    from axon_quant import (
        TradingEnv, EnvConfig, DefaultObservationSpace,
        SharpeReward, PnLReward, MultiObjectiveReward,
        ActionSpace,
    )
    
    # 配置环境
    config = EnvConfig(
        initial_capital=10000.0,
        transaction_cost=0.001,
        slippage=0.0005,
        max_position_ratio=0.5,  # RL 最大允许 50% 仓位
        max_steps=1000,
        symbol="BTCUSDT",
        return_window=252,
    )
    
    # 观测空间
    obs_space = DefaultObservationSpace.new(
        window_size=10,
        features=[
            FeatureConfig(name="close", source=FeatureSource.Close, normalizer=NormalizerType.ZScore),
            FeatureConfig(name="volume", source=FeatureSource.Volume, normalizer=NormalizerType.ZScore),
            FeatureConfig(name="rsi", source=FeatureSource.RSI(14), normalizer=NormalizerType.MinMax),
        ],
    )
    
    # 奖励函数：多目标（收益 + 夏普比率）
    reward_fn = MultiObjectiveReward([
        PnLReward(relative=True, scale=1.0),
        SharpeReward(risk_free_rate=0.02, window=20),
    ])
    
    # 创建环境
    env = TradingEnv.new(
        config=config,
        action_space=ActionSpace.continuous(low=-1.0, high=1.0),
        observation_space=obs_space,
        reward_fn=reward_fn,
        market_data=load_training_bars(),
    )
    
    # 使用 PPO 训练
    from stable_baselines3 import PPO
    model = PPO("MlpPolicy", env, verbose=1)
    model.learn(total_timesteps=100_000)
    
    return model
```

### 3.3 套利策略：协整检验 + Ensemble 多对组合

```python
"""
StatArbStrategy - 统计套利策略

核心逻辑:
- 选择: 通过协整检验找到价格序列稳定的配对
- 信号: 价差 Z-Score 超过阈值时开仓
- 仓位: 使用 DynamicWeightedEnsemble 管理多对组合的权重
- 出场: 价差回归均值 或 止损
"""

import numpy as np
from typing import List, Dict, Tuple, Optional
from decimal import Decimal
from dataclasses import dataclass

from axon_quant import (
    MarketBar, Order, OrderType, Side,
    DynamicWeightedEnsemble, ModelPerformance,
)


@dataclass
class Pair:
    """交易对结构体。"""
    symbol_a: str
    symbol_b: str
    hedge_ratio: float      # 对冲比例 (A / B)
    zscore_threshold: float # Z-Score 阈值，默认 2.0
    lookback: int           # 回看周期


class StatArbStrategy:
    """
    统计套利策略：协整配对 + Ensemble 权重管理。
    
    参数:
        pairs: 交易对列表
        max_pairs: 同时持有的最大配对数
        ensemble: 动态权重集成器（管理多对组合的权重）
    """
    
    def __init__(
        self,
        pairs: List[Pair],
        max_pairs: int = 5,
        ensemble: Optional[DynamicWeightedEnsemble] = None,
    ):
        self.pairs = pairs
        self.max_pairs = max_pairs
        self.ensemble = ensemble
        
        # 每个配对的价格历史
        self.price_history: Dict[str, List[float]] = {p.symbol_a: [] for p in pairs}
        self.price_history.update({p.symbol_b: [] for p in pairs})
        
        # 当前持仓
        self.positions: Dict[str, dict] = {}  # pair_key -> position_info
        
        # 各配对的表现记录（用于 Ensemble 权重更新）
        self.pair_returns: Dict[str, List[float]] = {self._pair_key(p): [] for p in pairs}
    
    def on_bar(self, bars: Dict[str, MarketBar]) -> List[Order]:
        """
        每根 K 线到达时处理所有配对。
        
        Args:
            bars: symbol -> MarketBar 的映射
            
        Returns:
            订单列表（可能包含多个配对的订单）
        """
        # 更新价格历史
        for symbol, bar in bars.items():
            if symbol in self.price_history:
                self.price_history[symbol].append(bar.close)
                # 保持历史长度
                max_lookback = max(p.lookback for p in self.pairs) * 2
                if len(self.price_history[symbol]) > max_lookback:
                    self.price_history[symbol] = self.price_history[symbol][-max_lookback:]
        
        orders = []
        pair_signals = []
        
        for pair in self.pairs:
            if len(self.price_history[pair.symbol_a]) < pair.lookback:
                continue
            
            signal = self._evaluate_pair(pair, bars)
            if signal:
                pair_signals.append((pair, signal))
        
        # 使用 Ensemble 选择最优配对（如果配置了）
        if self.ensemble and len(pair_signals) > self.max_pairs:
            pair_signals = self._select_pairs_with_ensemble(pair_signals)
        
        # 生成订单
        for pair, signal in pair_signals:
            pair_orders = self._create_pair_orders(pair, signal, bars)
            orders.extend(pair_orders)
        
        return orders
    
    def _evaluate_pair(self, pair: Pair, bars: Dict[str, MarketBar]) -> Optional[dict]:
        """
        评估单个配对，返回信号或 None。
        
        计算步骤:
        1. 计算价差 spread = price_a - hedge_ratio * price_b
        2. 计算价差的 Z-Score
        3. Z-Score 超过阈值时产生信号
        """
        prices_a = self.price_history[pair.symbol_a][-pair.lookback:]
        prices_b = self.price_history[pair.symbol_b][-pair.lookback:]
        
        # 计算价差序列
        spreads = [a - pair.hedge_ratio * b for a, b in zip(prices_a, prices_b)]
        
        # 计算当前价差和 Z-Score
        current_spread = spreads[-1]
        mean_spread = np.mean(spreads)
        std_spread = np.std(spreads)
        
        if std_spread == 0:
            return None
        
        zscore = (current_spread - mean_spread) / std_spread
        
        # 检查是否已有持仓
        pair_key = self._pair_key(pair)
        has_position = pair_key in self.positions
        
        # 开仓信号
        if not has_position:
            if zscore > pair.zscore_threshold:
                # 价差过高：做空 A，做多 B
                return {
                    "action": "open",
                    "direction": "short_spread",
                    "zscore": zscore,
                    "spread": current_spread,
                }
            elif zscore < -pair.zscore_threshold:
                # 价差过低：做多 A，做空 B
                return {
                    "action": "open",
                    "direction": "long_spread",
                    "zscore": zscore,
                    "spread": current_spread,
                }
        
        # 出场信号：价差回归均值
        elif has_position:
            position = self.positions[pair_key]
            if abs(zscore) < 0.5:  # 回归阈值
                return {
                    "action": "close",
                    "direction": position["direction"],
                    "zscore": zscore,
                    "spread": current_spread,
                }
        
        return None
    
    def _select_pairs_with_ensemble(
        self,
        pair_signals: List[Tuple[Pair, dict]],
    ) -> List[Tuple[Pair, dict]]:
        """
        使用 DynamicWeightedEnsemble 选择最优配对。
        
        思路：将每个配对视为一个"模型"，根据其近期夏普比率动态分配资金权重。
        """
        # 构造观测（简化：使用各配对的 Z-Score 绝对值作为特征）
        observations = []
        for pair, signal in pair_signals:
            obs = Observation(
                market_features=[abs(signal["zscore"]), pair.hedge_ratio],
                technical_indicators=[],
                portfolio_state=PortfolioState(),
                time_features=[],
            )
            observations.append((pair, signal, obs))
        
        # 获取当前权重
        weights = self.ensemble.get_weights()
        weight_map = {w.model_name: w.weight for w in weights}
        
        # 按权重排序，选择 top N
        scored = []
        for pair, signal, obs in observations:
            pair_key = self._pair_key(pair)
            weight = weight_map.get(pair_key, 1.0 / len(pair_signals))
            scored.append((pair, signal, weight))
        
        scored.sort(key=lambda x: x[2], reverse=True)
        return [(p, s) for p, s, _ in scored[:self.max_pairs]]
    
    def _create_pair_orders(
        self,
        pair: Pair,
        signal: dict,
        bars: Dict[str, MarketBar],
    ) -> List[Order]:
        """为配对交易创建双向订单。"""
        orders = []
        pair_key = self._pair_key(pair)
        
        # 固定名义金额
        notional = 1000.0
        
        if signal["action"] == "open":
            if signal["direction"] == "short_spread":
                # 做空 A，做多 B
                qty_a = Decimal(str(notional / bars[pair.symbol_a].close))
                qty_b = Decimal(str(notional / bars[pair.symbol_b].close))
                
                orders.append(self._make_order(pair.symbol_a, Side.Sell, qty_a, signal))
                orders.append(self._make_order(pair.symbol_b, Side.Buy, qty_b, signal))
                
                self.positions[pair_key] = {
                    "direction": "short_spread",
                    "entry_spread": signal["spread"],
                    "qty_a": qty_a,
                    "qty_b": qty_b,
                }
            else:
                # 做多 A，做空 B
                qty_a = Decimal(str(notional / bars[pair.symbol_a].close))
                qty_b = Decimal(str(notional / bars[pair.symbol_b].close))
                
                orders.append(self._make_order(pair.symbol_a, Side.Buy, qty_a, signal))
                orders.append(self._make_order(pair.symbol_b, Side.Sell, qty_b, signal))
                
                self.positions[pair_key] = {
                    "direction": "long_spread",
                    "entry_spread": signal["spread"],
                    "qty_a": qty_a,
                    "qty_b": qty_b,
                }
        
        elif signal["action"] == "close":
            # 平仓：反向操作
            position = self.positions[pair_key]
            
            if position["direction"] == "short_spread":
                orders.append(self._make_order(pair.symbol_a, Side.Buy, position["qty_a"], signal))
                orders.append(self._make_order(pair.symbol_b, Side.Sell, position["qty_b"], signal))
            else:
                orders.append(self._make_order(pair.symbol_a, Side.Sell, position["qty_a"], signal))
                orders.append(self._make_order(pair.symbol_b, Side.Buy, position["qty_b"], signal))
            
            # 计算收益并记录
            pnl = self._calculate_pair_pnl(pair, position, signal["spread"])
            self.pair_returns[pair_key].append(pnl)
            
            del self.positions[pair_key]
        
        return orders
    
    def _make_order(self, symbol: str, side: Side, quantity: Decimal, signal: dict) -> Order:
        """创建单个订单。"""
        return Order(
            # 0.6.0 Python `axon_quant.oms.Order` 字段集
            symbol=symbol,
            side=side,
            order_type=OrderType.Market,
            quantity=quantity,
            price=Decimal("0"),  # 市价单 price 传 0
            idempotency_key=f"stat-arb-{signal.get('action', 'na')[:8]}",
        )
    
    def _calculate_pair_pnl(self, pair: Pair, position: dict, exit_spread: float) -> float:
        """计算配对交易的收益。"""
        entry_spread = position["entry_spread"]
        if position["direction"] == "short_spread":
            return entry_spread - exit_spread
        else:
            return exit_spread - entry_spread
    
    def _pair_key(self, pair: Pair) -> str:
        """生成配对的唯一标识。"""
        return f"{pair.symbol_a}_{pair.symbol_b}"
    
    def update_ensemble_weights(self):
        """
        定期更新 Ensemble 权重。
        
        根据各配对的近期夏普比率，调整资金分配。
        """
        if self.ensemble is None:
            return
        
        performances = []
        for pair in self.pairs:
            pair_key = self._pair_key(pair)
            returns = self.pair_returns[pair_key][-30:]  # 最近 30 期
            
            if len(returns) < 5:
                continue
            
            sharpe = np.mean(returns) / (np.std(returns) + 1e-6)
            max_dd = min(0, min(np.minimum.accumulate(np.cumsum(returns))))
            
            perf = ModelPerformance(
                model_name=pair_key,
                accuracy=0.5,
                sharpe_ratio=sharpe,
                max_drawdown=max_dd,
                total_return=sum(returns),
                sample_count=len(returns),
                last_evaluated=int(time.time()),
            )
            performances.append(perf)
        
        for perf in performances:
            self.ensemble.update_performance(perf)


# ==================== 协整检验（配对选择）====================

def find_cointegrated_pairs(prices_df: pd.DataFrame, significance: float = 0.05) -> List[Pair]:
    """
    从价格数据框中找到协整的配对。
    
    参数:
        prices_df: DataFrame，列为 symbol，行为时间序列价格
        significance: 显著性水平，默认 0.05
        
    返回:
        协整配对列表
    """
    from statsmodels.tsa.stattools import coint
    
    symbols = prices_df.columns.tolist()
    pairs = []
    
    for i, sym_a in enumerate(symbols):
        for sym_b in symbols[i + 1:]:
            # 执行协整检验 (Engle-Granger)
            score, pvalue, _ = coint(prices_df[sym_a], prices_df[sym_b])
            
            if pvalue < significance:
                # 计算对冲比例（OLS 回归）
                hedge_ratio = np.polyfit(prices_df[sym_b], prices_df[sym_a], 1)[0]
                
                pair = Pair(
                    symbol_a=sym_a,
                    symbol_b=sym_b,
                    hedge_ratio=hedge_ratio,
                    zscore_threshold=2.0,
                    lookback=20,
                )
                pairs.append(pair)
                print(f"发现协整配对: {sym_a}/{sym_b}, p-value={pvalue:.4f}, 对冲比={hedge_ratio:.4f}")
    
    return pairs
```

---

## 4. 策略与 AI 模块集成指南

### 4.1 集成架构图

```text
┌─────────────────────────────────────────────────────────────────┐
│                        Strategy Layer                            │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐ │
│  │ Traditional │  │  Hybrid     │  │  AI-Native              │ │
│  │ (Rule-based)│  │ (Rule + AI) │  │  (RL / LLM)             │ │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────────┘ │
│         │                │                     │                │
│         └────────────────┼─────────────────────┘                │
│                          ▼                                      │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                    AXON Core Services                     │  │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐  │  │
│  │  │ Backtest │ │  Risk    │ │  Exchange│ │  Tracker   │  │  │
│  │  │ Engine   │ │  Engine  │ │  Adapter │ │  (MLflow)  │  │  │
│  │  └──────────┘ └──────────┘ └──────────┘ └────────────┘  │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 4.2 集成最佳实践

#### 4.2.1 传统策略接入 RL 环境

```python
from axon_quant import TradingEnv, EnvConfig

# 传统策略可以包装成 Gymnasium 兼容的接口
def run_strategy_in_env(strategy, env: TradingEnv):
    """
    在 TradingEnv 中运行传统策略，利用环境的标准化接口。
    
    好处:
    - 统一的回测和评估标准
    - 自动记录 metrics 到 Tracker
    - 与 RL 策略公平对比
    """
    obs = env.reset()
    done = False
    
    while not done:
        # 传统策略直接读取环境状态做决策
        bar = env.current_bar()
        action = strategy.on_bar(bar)
        
        # 将订单转换为环境的动作格式
        env_action = convert_order_to_action(action)
        
        obs, reward, done, info = env.step(env_action)
    
    return env.get_statistics()
```

#### 4.2.2 策略组合与 Ensemble

```python
from axon_quant import EnsembleManager, SoftVoteStrategy

# 将多个策略（传统 + AI）组合成一个 Ensemble
def create_multi_strategy_ensemble():
    """
    组合不同类型的策略，利用 Ensemble 降低单一策略失效风险。
    """
    manager = EnsembleManager(strategy=SoftVoteStrategy())
    
    # 注册传统策略（包装为 Policy 接口）
    manager.register_model(MomentumPolicy("momentum", 5, 20))
    manager.register_model(MeanReversionPolicy("mean_rev", 20, 2.0))
    
    # 注册 RL 策略
    manager.register_model(RLPolicy("ppo", "models/ppo.pt"))
    manager.register_model(RLPolicy("sac", "models/sac.pt"))
    
    # 注册 LLM 策略
    manager.register_model(LLMPolicy("llm_trend", llm_backend, "trend"))
    
    return manager
```

#### 4.2.3 特征共享

```python
from axon_quant import DefaultObservationSpace, FeatureConfig

# 传统策略和 RL 策略共享同一套特征工程
def build_shared_observation_space():
    """
    统一特征定义，确保传统策略和 RL 模型看到相同的市场表示。
    """
    features = [
        FeatureConfig(name="close", source=FeatureSource.Close, normalizer=NormalizerType.ZScore),
        FeatureConfig(name="volume", source=FeatureSource.Volume, normalizer=NormalizerType.ZScore),
        FeatureConfig(name="rsi_14", source=FeatureSource.RSI(14), normalizer=NormalizerType.MinMax),
        FeatureConfig(name="macd", source=FeatureSource.MACD(12, 26, 9), normalizer=NormalizerType.ZScore),
        FeatureConfig(name="bb_position", source=FeatureSource.BollingerPosition(20, 2.0), normalizer=NormalizerType.MinMax),
    ]
    
    return DefaultObservationSpace.new(window_size=20, features=features)
```

### 4.3 性能对比与选择建议

| 策略类型 | 延迟 | 可解释性 | 适应性 | 最佳场景 |
|----------|------|----------|--------|----------|
| 纯规则 | < 1ms | 极高 | 低 | 明确的市场模式 |
| 规则 + LLM | 10ms ~ 2s | 高 | 中 | 需要新闻验证的场景 |
| 规则 + RL | < 5ms | 中 | 高 | 仓位/风险管理优化 |
| 纯 RL | < 5ms | 低 | 极高 | 复杂多变量环境 |
| Ensemble | < 10ms | 中 | 极高 | 生产环境首选 |

---

## 5. 总结

AXON 框架对传统策略提供了完整的支持：

1. **零 RL 依赖**：纯 Python 策略可直接使用 `BacktestEngine` 和 `ExchangeAdapter`
2. **渐进式增强**：传统信号 + LLM 复核，在保持低延迟的同时获得 AI 能力
3. **统一特征工程**：`DefaultObservationSpace` 让传统策略和 RL 模型共享同一套特征
4. **Ensemble 集成**：`DynamicWeightedEnsemble` 可同时管理传统策略和 AI 模型
5. **完整生命周期**：从回测、优化到生产部署，传统策略与 AI 策略走同一套流程

建议从简单的规则策略开始，逐步引入 LLM 复核和 RL 优化，最终通过 Ensemble 实现多策略协同。
