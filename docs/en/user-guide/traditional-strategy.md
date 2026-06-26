# Scenario 4 — Traditional Strategy Migration and Custom Strategies

> **Full runnable example**: [`examples/16_traditional_strategy/traditional_strategy_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/16_traditional_strategy/traditional_strategy_demo.py)
> Covers momentum, mean reversion, and trend following strategies with signal generation and backtest comparison.

This document demonstrates how to implement and run traditional quantitative strategies (without RL) in the AXON framework, and how to combine traditional signals with AI modules to build hybrid trading systems.

---

## 1. Pure Python Strategies (No RL)

AXON's trading environment (`TradingEnv`) and backtesting engine (`BacktestEngine`) fully support rule-based strategies. You can implement strategies like regular Python classes without understanding any RL details.

### 1.1 SimpleMomentumStrategy Complete Code

```python
"""
SimpleMomentumStrategy - Pure Python momentum strategy example

This strategy doesn't depend on any RL modules, directly calculates
momentum signals from price data, and backtests via AXON's BacktestEngine.
"""

from dataclasses import dataclass
from typing import List, Optional, Dict
from decimal import Decimal
import numpy as np

from axon_quant import (
    BacktestEngine,
    MarketBar,
    Order,
    OrderId,
    OrderType,
    Side,
    TimeInForce,
    Symbol,
    ExchangeId,
)


@dataclass
class Signal:
    """Strategy signal structure."""
    action: str           # "buy" / "sell" / "hold"
    confidence: float     # 0.0 ~ 1.0
    target_position: float  # Target position ratio 0.0 ~ 1.0
    reason: str           # Signal reason description


class SimpleMomentumStrategy:
    """
    Simple momentum strategy: Based on dual moving average crossover + volume confirmation.
    
    Strategy Logic:
    1. Calculate short-term MA (MA5) and long-term MA (MA20)
    2. Generate buy signal when MA5 crosses above MA20 and volume increases 1.5x
    3. Generate sell signal when MA5 crosses below MA20 and volume increases 1.5x
    4. Hold position otherwise
    
    Parameters:
        short_window: Short-term MA window, default 5
        long_window: Long-term MA window, default 20
        volume_threshold: Volume increase threshold, default 1.5
        position_size: Position size per trade, default 0.5 (50%)
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
        
        # State tracking
        self.price_history: List[float] = []
        self.volume_history: List[float] = []
        self.current_position: float = 0.0  # Current position ratio -1.0 ~ 1.0
        self.signals: List[Dict] = []
    
    def on_bar(self, bar: MarketBar) -> Optional[Order]:
        """
        Called on each K-line arrival, returns trading order or None.
        
        Args:
            bar: Current K-line data with open/high/low/close/volume
            
        Returns:
            Order object (when signal exists) or None (no signal)
        """
        # Record historical data
        self.price_history.append(bar.close)
        self.volume_history.append(bar.volume)
        
        # Don't trade if insufficient data
        if len(self.price_history) < self.long_window:
            return None
        
        # Calculate signals
        signal = self._calculate_signal()
        
        if signal and signal.action != "hold":
            return self._create_order(signal, bar)
        
        return None
    
    def _calculate_signal(self) -> Optional[Signal]:
        """Calculate momentum signal."""
        prices = self.price_history
        volumes = self.volume_history
        
        # Calculate moving averages
        short_ma = np.mean(prices[-self.short_window:])
        long_ma = np.mean(prices[-self.long_window:])
        
        # Calculate volume average
        avg_volume = np.mean(volumes[-self.long_window:])
        current_volume = volumes[-1]
        
        # Volume confirmation
        volume_confirmed = current_volume > avg_volume * self.volume_threshold
        
        # Generate signal
        if short_ma > long_ma and volume_confirmed:
            return Signal(
                action="buy",
                confidence=min(0.9, (short_ma / long_ma - 1) * 10),
                target_position=self.position_size,
                reason=f"MA{self.short_window} > MA{self.long_window}, volume confirmed",
            )
        elif short_ma < long_ma and volume_confirmed:
            return Signal(
                action="sell",
                confidence=min(0.9, (1 - short_ma / long_ma) * 10),
                target_position=-self.position_size,
                reason=f"MA{self.short_window} < MA{self.long_window}, volume confirmed",
            )
        
        return None
    
    def _create_order(self, signal: Signal, bar: MarketBar) -> Order:
        """Create order from signal."""
        side = Side.Buy if signal.action == "buy" else Side.Sell
        quantity = abs(signal.target_position) * 10000  # Scale to actual quantity
        
        return Order(
            client_order_id=OrderId.new(),
            symbol=Symbol("BTCUSDT"),
            side=side,
            order_type=OrderType.Market,
            price=None,
            quantity=Decimal(str(quantity)),
            time_in_force=TimeInForce.Ioc,
            exchange=ExchangeId.Binance,
            meta={"strategy": "momentum", "reason": signal.reason},
        )
```

---

## 2. Traditional Strategy with Backtesting

```python
"""
Run SimpleMomentumStrategy backtest
"""

from axon_quant import BacktestEngine, BacktestConfig

def run_momentum_backtest():
    # 1. Create strategy
    strategy = SimpleMomentumStrategy(
        short_window=5,
        long_window=20,
        volume_threshold=1.5,
        position_size=0.3,
    )
    
    # 2. Configure backtest
    config = BacktestConfig(
        initial_capital=100_000.0,
        symbol="BTCUSDT",
        start_date="2024-01-01",
        end_date="2024-06-01",
        data_source="binance_1h",
    )
    
    # 3. Create engine and run
    engine = BacktestEngine(config)
    result = engine.run(strategy)
    
    # 4. Analyze results
    print(f"Total Return: {result.total_return:.2%}")
    print(f"Sharpe Ratio: {result.sharpe_ratio:.2f}")
    print(f"Max Drawdown: {result.max_drawdown:.2%}")
    print(f"Total Trades: {result.total_trades}")
    
    return result
```

---

## 3. Hybrid Strategy: Traditional + AI

AXON supports combining traditional signals with AI modules for hybrid trading systems.

### 3.1 Hybrid Momentum + RL Strategy

```python
"""
Hybrid Strategy: Traditional momentum signals combined with RL position sizing
"""

from dataclasses import dataclass
from typing import Optional
import numpy as np

from axon_quant import (
    TradingEnv,
    Order,
    Side,
)


class HybridMomentumRL:
    """
    Hybrid strategy combining traditional momentum with RL position sizing.
    
    - Traditional: Generates buy/sell signals based on moving averages
    - RL: Determines optimal position size based on market conditions
    """
    
    def __init__(
        self,
        rl_model,
        short_window: int = 5,
        long_window: int = 20,
    ):
        self.rl_model = rl_model
        self.short_window = short_window
        self.long_window = long_window
        self.price_history = []
    
    def on_bar(self, bar, portfolio_state) -> Optional[Order]:
        """
        Process each bar with hybrid decision making.
        
        Args:
            bar: Current market bar
            portfolio_state: Current portfolio state
            
        Returns:
            Order or None
        """
        self.price_history.append(bar.close)
        
        if len(self.price_history) < self.long_window:
            return None
        
        # Step 1: Traditional signal generation
        signal = self._generate_traditional_signal()
        
        # Step 2: RL-based position sizing
        if signal != "hold":
            position_size = self._get_rl_position_size(
                self.price_history,
                portfolio_state,
                signal,
            )
            
            # Step 3: Create order with RL-determined size
            side = Side.Buy if signal == "buy" else Side.Sell
            quantity = position_size * portfolio_state.total_value / bar.close
            
            return Order(
                symbol=bar.symbol,
                side=side,
                quantity=quantity,
                price=bar.close,
            )
        
        return None
    
    def _generate_traditional_signal(self) -> str:
        """Generate traditional momentum signal."""
        prices = self.price_history
        
        short_ma = np.mean(prices[-self.short_window:])
        long_ma = np.mean(prices[-self.long_window:])
        
        if short_ma > long_ma * 1.01:  # 1% threshold
            return "buy"
        elif short_ma < long_ma * 0.99:
            return "sell"
        return "hold"
    
    def _get_rl_position_size(
        self,
        prices: list,
        portfolio_state,
        signal: str,
    ) -> float:
        """Get RL model's recommended position size."""
        # Prepare features for RL model
        features = self._extract_features(prices, portfolio_state)
        
        # Get RL prediction
        position_size = self.rl_model.predict(features)
        
        # Clip to reasonable range
        return np.clip(position_size, 0.0, 0.5)  # Max 50% position
    
    def _extract_features(self, prices, portfolio_state) -> np.ndarray:
        """Extract features for RL model."""
        returns = np.diff(prices[-20:]) / prices[-21:-1]
        
        return np.array([
            np.mean(returns),           # Mean return
            np.std(returns),            # Volatility
            np.max(returns),            # Max return
            np.min(returns),            # Min return
            portfolio_state.cash_ratio, # Cash ratio
            portfolio_state.position_ratio,  # Current position
        ])
```

---

## 4. Strategy Testing Framework

```python
"""
Test traditional strategies with AXON's testing framework
"""

from axon_quant import BacktestEngine, BacktestConfig
from axon_quant.testing import StrategyTester


def test_momentum_strategy():
    """Test momentum strategy with various market conditions."""
    
    tester = StrategyTester()
    
    # Test with trending market
    trend_result = tester.run_test(
        strategy=SimpleMomentumStrategy(),
        market_data="trending_market",
        duration="6months",
    )
    assert trend_result.sharpe_ratio > 0.5, "Should profit in trending market"
    
    # Test with sideways market
    sideways_result = tester.run_test(
        strategy=SimpleMomentumStrategy(),
        market_data="sideways_market",
        duration="6months",
    )
    assert abs(sideways_result.total_return) < 0.1, "Should not lose much in sideways market"
    
    # Test with volatile market
    volatile_result = tester.run_test(
        strategy=SimpleMomentumStrategy(),
        market_data="volatile_market",
        duration="6months",
    )
    assert volatile_result.max_drawdown < 0.2, "Drawdown should be controlled"
    
    print("All strategy tests passed!")


def test_risk_management():
    """Test risk management rules."""
    
    # Configure risk limits
    risk_config = {
        "max_position_size": 0.3,
        "max_daily_loss": 0.02,
        "max_drawdown": 0.15,
    }
    
    tester = StrategyTester(risk_config=risk_config)
    
    result = tester.run_test(
        strategy=SimpleMomentumStrategy(position_size=0.5),  # Larger than limit
        market_data="test_market",
    )
    
    # Risk limits should have been enforced
    assert result.max_position <= 0.3, "Position size should be limited"
    assert result.max_drawdown <= 0.15, "Drawdown should be limited"
```

---

## 5. Performance Optimization for Traditional Strategies

### 5.1 Caching

```python
from axon_quant import LRUCache

# Cache expensive calculations
ma_cache = LRUCache(maxsize=1000)

def cached_moving_average(prices, window):
    """Cache moving average calculations."""
    cache_key = (tuple(prices[-window:]), window)
    
    if cache_key in ma_cache:
        return ma_cache[cache_key]
    
    result = np.mean(prices[-window:])
    ma_cache[cache_key] = result
    return result
```

### 5.2 Vectorized Operations

```python
import numpy as np

def vectorized_momentum(prices, short_window=5, long_window=20):
    """Vectorized momentum calculation for better performance."""
    prices = np.array(prices)
    
    # Calculate moving averages using convolution
    short_ma = np.convolve(prices, np.ones(short_window)/short_window, mode='valid')
    long_ma = np.convolve(prices, np.ones(long_window)/long_window, mode='valid')
    
    # Align arrays
    min_len = min(len(short_ma), len(long_ma))
    short_ma = short_ma[-min_len:]
    long_ma = long_ma[-min_len:]
    
    # Generate signals
    signals = np.where(short_ma > long_ma, 1, np.where(short_ma < long_ma, -1, 0))
    
    return signals
```

---

## Next Steps

- [Architecture Overview](architecture.md) — System components and design
- [AI-Native Design](ai-native-design.md) — Understanding AXON's AI-first approach
- [Strategy Development Pipeline](strategy-development.md) — Full RL strategy development
- [Backtesting Guide](../getting-started/quickstart.md) — Getting started with backtesting
