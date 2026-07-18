//! 端到端测试:Streaming 报告导出 (JSON / CSV / HTML)
//!
//! ## 5 个测试场景
//!
//! 1. `report_json_export_has_all_fields`:JSON 导出包含所有 snapshot 字段
//! 2. `report_csv_export_has_headers_and_equity_rows`:CSV 导出含表头 + equity_curve 数据
//! 3. `report_html_export_contains_key_metrics`:HTML 导出包含 Sharpe / Max Drawdown 等指标
//! 4. `report_export_to_file_roundtrip`:export_to_file 写入 tempfile → 读回验证
//! 5. `report_from_engine_reflects_fills`:构造有 fill 的 engine → report() → snapshot 字段非零
//!
//! 运行:`cargo test -p axon-backtest --test streaming_report_e2e`

use std::fs;

use axon_backtest::streaming::{
    MarketDataEvent, PaperTradingEngine, ReportFormat, SimulatedExchange, StrategyAction,
    StreamingEngine, StreamingStrategy, TradingMode,
};
use axon_core::market::{Side, Tick};
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::Currency;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};

// ── helpers ────────────────────────────────────────────────────────────

fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn make_tick(price: f64) -> Tick {
    Tick::new(
        Timestamp::from_nanos(1_000),
        Price::from_f64(price),
        Quantity::from_f64(1.0),
        Side::Buy,
    )
}

/// 构造有 fill 的 engine(含 deposit + maker + strategy)
fn engine_with_fills() -> StreamingEngine {
    struct BuyStrategy {
        id: u64,
    }
    impl StreamingStrategy for BuyStrategy {
        fn on_tick(&mut self, instrument: &Instrument, _price: f64) -> Vec<StrategyAction> {
            // 从 Instrument 直接取 base/quote(0.6.0:不再按 '/' 拆 string)
            let (base, quote) = (
                instrument.base().as_str().to_string(),
                instrument.quote().as_str().to_string(),
            );
            let order = Order::spot(
                self.id,
                base,
                quote,
                Side::Buy,
                OrderType::Market,
                Quantity::from_f64(0.1),
                TimeInForce::IOC,
            );
            self.id += 1;
            vec![StrategyAction::Submit(order)]
        }
    }

    let mut engine = StreamingEngine::new(TradingMode::PaperTrading).with_paper_engine(
        PaperTradingEngine::new(SimulatedExchange {
            fill_probability: 1.0,
            ..SimulatedExchange::default()
        }),
    );
    engine.register_instrument(btc_spot());
    engine.portfolio_mut().deposit(Currency::USD, 100_000.0);
    engine.set_initial_cash(100_000.0);

    // 挂 Sell Limit maker(给 Market Buy 对手盘)
    let maker = Order::spot(
        900,
        "BTC",
        "USDT",
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(100.0),
        },
        Quantity::from_f64(10.0),
        TimeInForce::GTC,
    );
    engine.submit_order(maker).expect("submit maker");

    engine.with_strategy(Box::new(BuyStrategy { id: 1 }))
}

// ── 1. JSON 导出包含所有 snapshot 字段 ─────────────────────────────────

#[test]
fn report_json_export_has_all_fields() {
    let mut engine = engine_with_fills();
    // 喂一个 tick 触发 strategy → fill
    let _ = engine.on_market_event(MarketDataEvent::Tick {
        instrument: btc_spot(),
        tick: make_tick(100.0),
    });

    let report = engine.report();
    let data = report.export(ReportFormat::Json).unwrap();
    let json_str = String::from_utf8(data).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // snapshot 字段齐全
    let snap = &parsed["snapshot"];
    assert!(snap.get("total_pnl").is_some());
    assert!(snap.get("total_fees").is_some());
    assert!(snap.get("win_rate").is_some());
    assert!(snap.get("sharpe_ratio").is_some());
    assert!(snap.get("max_drawdown").is_some());
    assert!(snap.get("max_drawdown_pct").is_some());
    assert!(snap.get("nav_peak").is_some());
    assert!(snap.get("final_nav").is_some());
    assert!(snap.get("mode").is_some());

    // equity_curve 非空
    let curve = parsed["equity_curve"].as_array().unwrap();
    assert!(!curve.is_empty());
}

// ── 2. CSV 导出含表头 + equity_curve 数据 ──────────────────────────────

#[test]
fn report_csv_export_has_headers_and_equity_rows() {
    let mut engine = engine_with_fills();
    let _ = engine.on_market_event(MarketDataEvent::Tick {
        instrument: btc_spot(),
        tick: make_tick(100.0),
    });

    let report = engine.report();
    let data = report.export(ReportFormat::Csv).unwrap();
    let csv_str = String::from_utf8(data).unwrap();
    let lines: Vec<&str> = csv_str.trim().lines().collect();

    // snapshot header + snapshot data + equity header + equity rows
    assert!(lines.len() >= 4, "CSV 应有至少 4 行,实为 {}", lines.len());
    assert!(lines[0].contains("total_pnl"), "第一行应为 snapshot 表头");
    assert!(
        lines[2].contains("equity_timestamp_ns"),
        "第三行应为 equity 表头"
    );
}

// ── 3. HTML 导出包含关键指标 ──────────────────────────────────────────

#[test]
fn report_html_export_contains_key_metrics() {
    let mut engine = engine_with_fills();
    let _ = engine.on_market_event(MarketDataEvent::Tick {
        instrument: btc_spot(),
        tick: make_tick(100.0),
    });

    let report = engine.report();
    let data = report.export(ReportFormat::Html).unwrap();
    let html = String::from_utf8(data).unwrap();

    assert!(html.contains("Sharpe Ratio"));
    assert!(html.contains("Max Drawdown"));
    assert!(html.contains("Win Rate"));
    assert!(html.contains("Total PnL"));
    assert!(html.contains("Equity Curve"));
    assert!(html.contains("<table>"));
}

// ── 4. export_to_file 写入 tempfile → 读回验证 ────────────────────────

#[test]
fn report_export_to_file_roundtrip() {
    let mut engine = engine_with_fills();
    let _ = engine.on_market_event(MarketDataEvent::Tick {
        instrument: btc_spot(),
        tick: make_tick(100.0),
    });

    let report = engine.report();
    let dir = tempfile::tempdir().unwrap();

    // JSON
    let json_path = dir.path().join("report.json");
    report
        .export_to_file(&json_path, ReportFormat::Json)
        .unwrap();
    let json_content = fs::read_to_string(&json_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_content).unwrap();
    assert!(parsed.is_object());

    // CSV
    let csv_path = dir.path().join("report.csv");
    report.export_to_file(&csv_path, ReportFormat::Csv).unwrap();
    let csv_content = fs::read_to_string(&csv_path).unwrap();
    assert!(csv_content.contains("total_pnl"));

    // HTML
    let html_path = dir.path().join("report.html");
    report
        .export_to_file(&html_path, ReportFormat::Html)
        .unwrap();
    let html_content = fs::read_to_string(&html_path).unwrap();
    assert!(html_content.contains("<html"));
}

// ── 5. from_engine 反映真实 fill 状态 ──────────────────────────────────

#[test]
fn report_from_engine_reflects_fills() {
    let mut engine = engine_with_fills();
    // 喂 3 个 tick 触发多次 fill
    for price in [100.0, 101.0, 102.0] {
        let _ = engine.on_market_event(MarketDataEvent::Tick {
            instrument: btc_spot(),
            tick: make_tick(price),
        });
    }

    let report = engine.report();

    // snapshot 字段应反映真实状态
    assert!(
        report.snapshot.total_trades > 0,
        "应有 fill,实为 {}",
        report.snapshot.total_trades
    );
    assert!(report.snapshot.equity_curve_len > 0, "equity_curve 应非空");
    assert!(!report.equity_curve.is_empty());
    assert!(report.snapshot.final_nav > 0.0);
}
