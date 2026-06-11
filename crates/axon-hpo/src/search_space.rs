//! 搜索空间定义
//!
//! 支持 6 种参数类型：Uniform / LogUniform / IntUniform / Discrete / Choice / Categorical。

use serde::{Deserialize, Serialize};

/// 搜索空间参数定义
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchSpaceDef {
    /// 均匀分布（浮点）
    Uniform {
        /// 下界
        low: f64,
        /// 上界
        high: f64,
    },
    /// 对数均匀分布
    LogUniform {
        /// 下界
        low: f64,
        /// 上界
        high: f64,
    },
    /// 整数均匀分布
    IntUniform {
        /// 下界
        low: i64,
        /// 上界
        high: i64,
        /// 步长
        #[serde(default = "default_step")]
        step: i64,
    },
    /// 离散浮点列表
    Discrete {
        /// 候选值列表
        choices: Vec<f64>,
    },
    /// 离散值列表（支持混合类型：整数/浮点/字符串/布尔，统一转字符串后传入 Optuna）
    Choice {
        /// 候选值列表
        choices: Vec<serde_json::Value>,
    },
    /// 任意 JSON 值列表（用于混合类型选择，如 [32, "ppo", true]）
    Categorical {
        /// 候选值列表
        choices: Vec<serde_json::Value>,
    },
}

fn default_step() -> i64 {
    1
}

impl SearchSpaceDef {
    /// 均匀分布便捷构造
    pub fn uniform(low: f64, high: f64) -> Self {
        Self::Uniform { low, high }
    }

    /// 对数均匀分布便捷构造
    pub fn log_uniform(low: f64, high: f64) -> Self {
        Self::LogUniform { low, high }
    }

    /// 整数均匀分布便捷构造
    pub fn int_uniform(low: i64, high: i64, step: i64) -> Self {
        Self::IntUniform { low, high, step }
    }

    /// 离散浮点便捷构造
    pub fn discrete(choices: Vec<f64>) -> Self {
        Self::Discrete { choices }
    }

    /// 离散字符串便捷构造
    pub fn choice(choices: Vec<String>) -> Self {
        Self::Choice {
            choices: choices.into_iter().map(serde_json::Value::String).collect(),
        }
    }

    /// 类别便捷构造
    pub fn categorical(choices: Vec<serde_json::Value>) -> Self {
        Self::Categorical { choices }
    }

    /// 校验参数空间合法性
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Uniform { low, high } => {
                if low >= high {
                    return Err(format!("Uniform: low ({low}) must be < high ({high})"));
                }
            }
            Self::LogUniform { low, high } => {
                if *low <= 0.0 {
                    return Err(format!("LogUniform: low ({low}) must be > 0"));
                }
                if low >= high {
                    return Err(format!("LogUniform: low ({low}) must be < high ({high})"));
                }
            }
            Self::IntUniform { low, high, step } => {
                if low >= high {
                    return Err(format!("IntUniform: low ({low}) must be < high ({high})"));
                }
                if *step <= 0 {
                    return Err(format!("IntUniform: step ({step}) must be > 0"));
                }
            }
            Self::Discrete { choices } => {
                if choices.is_empty() {
                    return Err("Discrete: choices must not be empty".to_string());
                }
            }
            Self::Choice { choices } => {
                if choices.is_empty() {
                    return Err("Choice: choices must not be empty".to_string());
                }
            }
            Self::Categorical { choices } => {
                if choices.is_empty() {
                    return Err("Categorical: choices must not be empty".to_string());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uniform_validate_ok() {
        assert!(SearchSpaceDef::uniform(0.0, 1.0).validate().is_ok());
    }

    #[test]
    fn test_uniform_validate_low_ge_high() {
        assert!(SearchSpaceDef::uniform(1.0, 0.0).validate().is_err());
        assert!(SearchSpaceDef::uniform(1.0, 1.0).validate().is_err());
    }

    #[test]
    fn test_log_uniform_validate_low_le_zero() {
        assert!(SearchSpaceDef::log_uniform(0.0, 1.0).validate().is_err());
        assert!(SearchSpaceDef::log_uniform(-1.0, 1.0).validate().is_err());
    }

    #[test]
    fn test_log_uniform_validate_ok() {
        assert!(SearchSpaceDef::log_uniform(1e-5, 1e-2).validate().is_ok());
    }

    #[test]
    fn test_int_uniform_validate_step() {
        assert!(SearchSpaceDef::int_uniform(0, 10, 0).validate().is_err());
        assert!(SearchSpaceDef::int_uniform(0, 10, -1).validate().is_err());
        assert!(SearchSpaceDef::int_uniform(0, 10, 2).validate().is_ok());
    }

    #[test]
    fn test_discrete_validate_empty() {
        assert!(SearchSpaceDef::discrete(vec![]).validate().is_err());
        assert!(SearchSpaceDef::discrete(vec![1.0, 2.0]).validate().is_ok());
    }

    #[test]
    fn test_choice_validate_empty() {
        assert!(SearchSpaceDef::choice(vec![]).validate().is_err());
        assert!(SearchSpaceDef::choice(vec!["a".into()]).validate().is_ok());
        // 整数也能接受
        assert!(
            SearchSpaceDef::Choice {
                choices: vec![serde_json::json!(32), serde_json::json!(64)],
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn test_categorical_validate_empty() {
        assert!(SearchSpaceDef::categorical(vec![]).validate().is_err());
        assert!(
            SearchSpaceDef::categorical(vec![serde_json::json!(1)])
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let def = SearchSpaceDef::log_uniform(1e-5, 1e-2);
        let json = serde_json::to_string(&def).unwrap();
        let parsed: SearchSpaceDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, parsed);
    }
}
