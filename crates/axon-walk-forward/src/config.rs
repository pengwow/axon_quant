//! Walk-Forward 配置定义

use serde::{Deserialize, Serialize};

/// 窗口类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowType {
    /// 固定长度滑动窗口
    Rolling,
    /// 训练集从起点开始不断增长
    #[default]
    Expanding,
}

/// Walk-Forward 验证配置（索引单位）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardConfig {
    /// 训练窗口大小（数据点数）
    pub train_size: usize,
    /// 验证窗口大小（0 表示无验证集）
    #[serde(default)]
    pub validation_size: usize,
    /// 测试窗口大小
    pub test_size: usize,
    /// 滚动步长
    pub step_size: usize,
    /// 窗口类型
    pub window_type: WindowType,
    /// 训练-测试之间的 purge gap（防标签泄漏）
    #[serde(default)]
    pub purge_gap: usize,
    /// embargo 百分比（0.01 = 1%）
    #[serde(default = "default_embargo_pct")]
    pub embargo_pct: f64,
}

fn default_embargo_pct() -> f64 {
    0.01
}

impl WalkForwardConfig {
    /// 创建 Expanding 窗口配置
    pub fn expanding(train_size: usize, test_size: usize, step_size: usize) -> Self {
        Self {
            train_size,
            validation_size: 0,
            test_size,
            step_size,
            window_type: WindowType::Expanding,
            purge_gap: 0,
            embargo_pct: 0.0,
        }
    }

    /// 创建 Rolling 窗口配置
    pub fn rolling(train_size: usize, test_size: usize, step_size: usize) -> Self {
        Self {
            train_size,
            validation_size: 0,
            test_size,
            step_size,
            window_type: WindowType::Rolling,
            purge_gap: 0,
            embargo_pct: 0.0,
        }
    }

    /// 校验配置合法性
    pub fn validate(&self) -> Result<(), String> {
        if self.train_size == 0 {
            return Err("train_size must be > 0".to_string());
        }
        if self.test_size == 0 {
            return Err("test_size must be > 0".to_string());
        }
        if self.step_size == 0 {
            return Err("step_size must be > 0".to_string());
        }
        if !self.embargo_pct.is_finite() || !(0.0..=1.0).contains(&self.embargo_pct) {
            return Err(format!(
                "embargo_pct ({}) must be in [0.0, 1.0]",
                self.embargo_pct
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expanding_constructor() {
        let cfg = WalkForwardConfig::expanding(252, 63, 63);
        assert_eq!(cfg.train_size, 252);
        assert_eq!(cfg.test_size, 63);
        assert_eq!(cfg.step_size, 63);
        assert_eq!(cfg.window_type, WindowType::Expanding);
    }

    #[test]
    fn test_rolling_constructor() {
        let cfg = WalkForwardConfig::rolling(252, 63, 63);
        assert_eq!(cfg.window_type, WindowType::Rolling);
    }

    #[test]
    fn test_validate_ok() {
        assert!(WalkForwardConfig::expanding(252, 63, 63).validate().is_ok());
    }

    #[test]
    fn test_validate_zero_train() {
        let cfg = WalkForwardConfig {
            train_size: 0,
            ..WalkForwardConfig::expanding(252, 63, 63)
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_zero_test() {
        let cfg = WalkForwardConfig {
            test_size: 0,
            ..WalkForwardConfig::expanding(252, 63, 63)
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_embargo() {
        let mut cfg = WalkForwardConfig::expanding(252, 63, 63);
        cfg.embargo_pct = 1.5;
        assert!(cfg.validate().is_err());
        cfg.embargo_pct = -0.1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_default_window_type() {
        assert_eq!(WindowType::default(), WindowType::Expanding);
    }

    /// 默认 TOML 使用 `[walk_forward]` 子表，
    /// 需要一个外层包装结构来反序列化。
    #[derive(Debug, Deserialize)]
    struct WalkForwardConfigFile {
        walk_forward: WalkForwardConfig,
    }

    #[test]
    fn test_load_default_toml() {
        use std::path::Path;
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("config")
            .join("default_wf.toml");
        let content = std::fs::read_to_string(&path).expect("read toml");
        let file: WalkForwardConfigFile = toml::from_str(&content).expect("parse toml");
        let cfg = file.walk_forward;
        assert_eq!(cfg.train_size, 1260);
        assert_eq!(cfg.test_size, 63);
        assert_eq!(cfg.step_size, 63);
        assert_eq!(cfg.window_type, WindowType::Expanding);
        assert_eq!(cfg.purge_gap, 5);
        assert!((cfg.embargo_pct - 0.01).abs() < 1e-9);
    }
}
