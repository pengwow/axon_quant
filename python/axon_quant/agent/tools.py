"""Trading Tools

交易工具定义，包括:
1. place_order - 下单
2. query_portfolio - 查询持仓
3. query_market - 查询市场数据
4. risk_check - 风险检查
"""

from __future__ import annotations

from typing import Any, Dict, Optional


class TradingTools:
    """交易工具集合"""

    def __init__(self, backend: Any):
        self.backend = backend

    def place_order(
        self,
        symbol: str,
        side: str,
        quantity: float,
        price: Optional[float] = None,
    ) -> Dict[str, Any]:
        """下单"""
        try:
            result = self.backend.place_order(
                symbol=symbol,
                side=side,
                quantity=quantity,
                price=price,
            )
            return {"status": "success", "order_id": result.get("order_id"), "message": "Order placed"}
        except Exception as e:
            return {"status": "error", "message": str(e)}

    def query_portfolio(self) -> Dict[str, Any]:
        """查询持仓"""
        try:
            portfolio = self.backend.query_portfolio()
            return {"status": "success", "portfolio": portfolio}
        except Exception as e:
            return {"status": "error", "message": str(e)}

    def query_market(self, symbol: str) -> Dict[str, Any]:
        """查询市场数据"""
        try:
            snapshot = self.backend.book_snapshot(symbol)
            return {"status": "success", "snapshot": snapshot}
        except Exception as e:
            return {"status": "error", "message": str(e)}

    def risk_check(self, symbol: str, side: str, quantity: float, price: float) -> Dict[str, Any]:
        """风险检查"""
        try:
            portfolio = self.backend.query_portfolio()
            cash = portfolio.get("cash", 0.0)
            position = portfolio.get("positions", {}).get(symbol, 0.0)
            notional = quantity * price

            if side == "Buy" and cash < notional:
                return {"status": "reject", "reason": "Insufficient cash"}
            if side == "Sell" and position < quantity:
                return {"status": "reject", "reason": "Insufficient position"}

            return {"status": "approve", "cash": cash, "position": position}
        except Exception as e:
            return {"status": "error", "message": str(e)}

    def to_tool_list(self) -> list:
        """转换为工具列表格式"""
        return [
            {
                "name": "place_order",
                "description": "Place a trading order. Args: symbol(str), side(Buy/Sell), quantity(float), price(float)",
                "func": self.place_order,
            },
            {
                "name": "query_portfolio",
                "description": "Query current portfolio positions and cash",
                "func": self.query_portfolio,
            },
            {
                "name": "query_market",
                "description": "Query market data for a symbol. Args: symbol(str)",
                "func": self.query_market,
            },
            {
                "name": "risk_check",
                "description": "Check risk before placing order. Args: symbol(str), side(Buy/Sell), quantity(float), price(float)",
                "func": self.risk_check,
            },
        ]