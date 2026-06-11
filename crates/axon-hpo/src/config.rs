//! HPO / Study / Sampler / Pruner 配置定义
//!
//! 所有配置类型均实现 `Serialize` + `Deserialize`，支持从 TOML 加载。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::search_space::SearchSpaceDef;

/// Study 优化方向
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudyDirection {
    /// 最小化
    Minimize,
    /// 最大化
    Maximize,
}

impl StudyDirection {
    /// 是否最大化
    pub fn is_maximize(&self) -> bool {
        matches!(self, StudyDirection::Maximize)
    }

    /// 转换为 Optuna 字符串
    pub fn as_optuna_str(&self) -> &'static str {
        match self {
            StudyDirection::Minimize => "minimize",
            StudyDirection::Maximize => "maximize",
        }
    }
}

/// Sampler 类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sampler_type", rename_all = "snake_case")]
pub enum SamplerType {
    /// TPE 采样器
    Tpe {
        /// 启动 trial 数
        #[serde(default = "default_tpe_n_startup")]
        n_startup_trials: usize,
        /// 预热步数
        #[serde(default)]
        n_warmup_steps: usize,
    },
    /// 随机采样器
    #[serde(alias = "random")]
    Random,
    /// CMA-ES 采样器
    #[serde(alias = "cma_es")]
    CmaEs,
    /// 网格搜索
    Grid,
}

fn default_tpe_n_startup() -> usize {
    10
}

impl Default for SamplerType {
    fn default() -> Self {
        SamplerType::Tpe {
            n_startup_trials: 10,
            n_warmup_steps: 0,
        }
    }
}

/// Sampler 配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SamplerConfig {
    /// sampler 类型
    #[serde(flatten)]
    pub sampler_type: SamplerType,
    /// 随机种子
    pub seed: Option<u64>,
}

/// Pruner 类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "pruner_type", rename_all = "snake_case")]
pub enum PrunerType {
    /// 中位数剪枝
    #[serde(alias = "median")]
    MedianPruner {
        /// 启动 trial 数
        #[serde(default = "default_median_n_startup")]
        n_startup_trials: usize,
        /// 预热步数
        #[serde(default)]
        n_warmup_steps: usize,
    },
    /// Hyperband 剪枝
    #[serde(alias = "hyperband")]
    HyperbandPruner {
        /// 缩减因子
        #[serde(default = "default_reduction_factor")]
        reduction_factor: f64,
    },
    /// 逐步减半剪枝
    #[serde(alias = "successive_halving")]
    SuccessiveHalvingPruner {
        /// 最小资源
        #[serde(default = "default_min_resource")]
        min_resource: usize,
        /// 缩减因子
        #[serde(default = "default_reduction_factor")]
        reduction_factor: f64,
    },
    /// 不剪枝
    #[serde(alias = "none", alias = "nop")]
    NopPruner,
}

fn default_median_n_startup() -> usize {
    5
}

fn default_reduction_factor() -> f64 {
    3.0
}

fn default_min_resource() -> usize {
    1
}

impl Default for PrunerType {
    fn default() -> Self {
        PrunerType::MedianPruner {
            n_startup_trials: 5,
            n_warmup_steps: 0,
        }
    }
}

/// Pruner 配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrunerConfig {
    /// pruner 类型
    #[serde(flatten)]
    pub pruner_type: PrunerType,
}

/// Study 级别配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyConfig {
    /// study 名称
    pub study_name: String,
    /// 优化方向
    pub direction: StudyDirection,
    /// sampler 配置
    #[serde(default)]
    pub sampler: SamplerConfig,
    /// pruner 配置
    #[serde(default)]
    pub pruner: PrunerConfig,
    /// Optuna storage URL（如 "sqlite:///hpo.db"）
    pub storage: Option<String>,
    /// 如果已存在 study 是否加载
    #[serde(default)]
    pub load_if_exists: bool,
}

impl StudyConfig {
    /// 创建最大化方向的 study
    pub fn maximize(study_name: impl Into<String>) -> Self {
        Self {
            study_name: study_name.into(),
            direction: StudyDirection::Maximize,
            sampler: SamplerConfig::default(),
            pruner: PrunerConfig::default(),
            storage: None,
            load_if_exists: true,
        }
    }
}

/// 目标函数配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectiveConfig {
    /// 目标定义（单目标 or 多目标）
    #[serde(flatten)]
    pub objective: ObjectiveDef,
    /// 单次 trial 超时（秒）
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// 早停资源名
    #[serde(default = "default_resource_name")]
    pub resource_name: String,
}

fn default_resource_name() -> String {
    "episode_reward".to_string()
}

impl Default for ObjectiveConfig {
    fn default() -> Self {
        Self {
            objective: ObjectiveDef::Single {
                direction: StudyDirection::Maximize,
            },
            timeout_seconds: None,
            resource_name: default_resource_name(),
        }
    }
}

/// 目标函数定义
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObjectiveDef {
    /// 单目标
    Single {
        /// 方向
        direction: StudyDirection,
    },
    /// 多目标
    Multi {
        /// 多个方向
        directions: Vec<StudyDirection>,
    },
}

impl ObjectiveDef {
    /// 方向数量
    pub fn n_directions(&self) -> usize {
        match self {
            ObjectiveDef::Single { .. } => 1,
            ObjectiveDef::Multi { directions } => directions.len(),
        }
    }

    /// 转换为 Optuna 字符串列表
    pub fn to_optuna_directions(&self) -> Vec<&'static str> {
        match self {
            ObjectiveDef::Single { direction } => vec![direction.as_optuna_str()],
            ObjectiveDef::Multi { directions } => {
                directions.iter().map(|d| d.as_optuna_str()).collect()
            }
        }
    }
}

/// HPO 运行参数子表（TOML 中 `[hpo]`）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HPORunConfig {
    /// 总 trial 数
    #[serde(default = "default_n_trials")]
    pub n_trials: usize,
    /// 并行 trial 数
    #[serde(default = "default_n_jobs")]
    pub n_jobs: usize,
    /// 总超时（秒）
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// 是否启用早停
    #[serde(default)]
    pub early_stopping: bool,
}

fn default_n_trials() -> usize {
    50
}

fn default_n_jobs() -> usize {
    1
}

/// 完整 HPO 配置
///
/// TOML 格式：
/// - `[study]` / `[search_space.xxx]` / `[objective]` 在顶层
/// - `[hpo]` 子表存储运行参数（n_trials / n_jobs / ...）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HPOConfig {
    /// study 配置
    pub study: StudyConfig,
    /// 搜索空间
    pub search_space: HashMap<String, SearchSpaceDef>,
    /// 目标函数配置
    pub objective: ObjectiveConfig,
    /// 运行参数（n_trials / n_jobs / ...）
    #[serde(default)]
    pub hpo: HPORunConfig,
}

// 便捷字段代理
impl HPOConfig {
    /// 总 trial 数
    pub fn n_trials(&self) -> usize {
        self.hpo.n_trials
    }

    /// 并行 trial 数
    pub fn n_jobs(&self) -> usize {
        self.hpo.n_jobs
    }

    /// 总超时（秒）
    pub fn timeout_seconds(&self) -> Option<u64> {
        self.hpo.timeout_seconds
    }

    /// 是否启用早停
    pub fn early_stopping(&self) -> bool {
        self.hpo.early_stopping
    }
}

impl HPOConfig {
    /// 创建简单最大化 HPO 配置
    pub fn new(
        study_name: impl Into<String>,
        search_space: HashMap<String, SearchSpaceDef>,
        n_trials: usize,
    ) -> Self {
        Self {
            study: StudyConfig::maximize(study_name),
            search_space,
            objective: ObjectiveConfig::default(),
            hpo: HPORunConfig {
                n_trials,
                n_jobs: 1,
                timeout_seconds: None,
                early_stopping: false,
            },
        }
    }

    /// 设置多目标
    pub fn with_multi_objective(mut self, directions: Vec<StudyDirection>) -> Self {
        self.objective.objective = ObjectiveDef::Multi { directions };
        self
    }

    /// 设置 n_jobs
    pub fn with_n_jobs(mut self, n_jobs: usize) -> Self {
        self.hpo.n_jobs = n_jobs;
        self
    }

    /// 从 TOML 字符串加载配置
    pub fn from_toml(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }

    /// 从 TOML 文件加载配置
    pub fn from_toml_file(path: &std::path::Path) -> Result<Self, HPOConfigError> {
        let content = std::fs::read_to_string(path).map_err(HPOConfigError::Io)?;
        Self::from_toml(&content).map_err(HPOConfigError::Toml)
    }
}

/// HPOConfig 加载错误
#[derive(Debug, thiserror::Error)]
pub enum HPOConfigError {
    /// TOML 解析错误
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    /// IO 错误
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_study_config_maximize() {
        let cfg = StudyConfig::maximize("test");
        assert_eq!(cfg.study_name, "test");
        assert!(cfg.direction.is_maximize());
        assert!(cfg.load_if_exists);
    }

    #[test]
    fn test_study_direction_optuna_str() {
        assert_eq!(StudyDirection::Maximize.as_optuna_str(), "maximize");
        assert_eq!(StudyDirection::Minimize.as_optuna_str(), "minimize");
    }

    #[test]
    fn test_objective_def_n_directions() {
        let single = ObjectiveDef::Single {
            direction: StudyDirection::Maximize,
        };
        assert_eq!(single.n_directions(), 1);
        let multi = ObjectiveDef::Multi {
            directions: vec![StudyDirection::Maximize, StudyDirection::Maximize],
        };
        assert_eq!(multi.n_directions(), 2);
    }

    #[test]
    fn test_objective_def_to_optuna_directions() {
        let multi = ObjectiveDef::Multi {
            directions: vec![StudyDirection::Maximize, StudyDirection::Minimize],
        };
        let dirs = multi.to_optuna_directions();
        assert_eq!(dirs, vec!["maximize", "minimize"]);
    }

    #[test]
    fn test_hpo_config_new() {
        let mut space = HashMap::new();
        space.insert("lr".to_string(), SearchSpaceDef::log_uniform(1e-5, 1e-2));
        let cfg = HPOConfig::new("test_study", space, 50);
        assert_eq!(cfg.study.study_name, "test_study");
        assert_eq!(cfg.n_trials(), 50);
        assert_eq!(cfg.n_jobs(), 1);
    }

    #[test]
    fn test_hpo_config_with_multi_objective() {
        let mut space = HashMap::new();
        space.insert("lr".to_string(), SearchSpaceDef::log_uniform(1e-5, 1e-2));
        let cfg = HPOConfig::new("test", space, 10)
            .with_multi_objective(vec![StudyDirection::Maximize, StudyDirection::Maximize]);
        match &cfg.objective.objective {
            ObjectiveDef::Multi { directions } => {
                assert_eq!(directions.len(), 2);
            }
            _ => panic!("expected Multi"),
        }
    }

    #[test]
    fn test_hpo_config_with_n_jobs() {
        let mut space = HashMap::new();
        space.insert("x".to_string(), SearchSpaceDef::uniform(0.0, 1.0));
        let cfg = HPOConfig::new("t", space, 1).with_n_jobs(8);
        assert_eq!(cfg.n_jobs(), 8);
    }

    #[test]
    fn test_hpo_config_from_toml() {
        let toml_str = r#"
[study]
study_name = "ppo_v1"
direction = "maximize"
storage = "sqlite:///hpo.db"
load_if_exists = true

[search_space.learning_rate]
type = "log_uniform"
low = 1e-5
high = 1e-2

[search_space.batch_size]
type = "choice"
choices = [32, 64, 128]

[objective]
type = "single"
direction = "maximize"

[hpo]
n_trials = 50
n_jobs = 4
"#;
        let cfg = HPOConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.study.study_name, "ppo_v1");
        assert!(cfg.study.direction.is_maximize());
        assert_eq!(cfg.study.storage.as_deref(), Some("sqlite:///hpo.db"));
        assert!(cfg.study.load_if_exists);
        assert_eq!(cfg.search_space.len(), 2);
        assert_eq!(cfg.n_trials(), 50);
        assert_eq!(cfg.n_jobs(), 4);
    }

    #[test]
    fn test_hpo_config_from_toml_full() {
        // 验证默认配置文件能加载
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("config")
            .join("default_hpo.toml");
        let cfg = HPOConfig::from_toml_file(&path).unwrap();
        assert_eq!(cfg.study.study_name, "axon_rl_ppo_v1");
        assert!(cfg.study.direction.is_maximize());
        assert!(cfg.study.load_if_exists);
        // 至少包含 learning_rate / gamma / batch_size / hidden_size
        assert!(cfg.search_space.contains_key("learning_rate"));
        assert!(cfg.search_space.contains_key("gamma"));
        assert!(cfg.search_space.contains_key("batch_size"));
        assert!(cfg.search_space.contains_key("hidden_size"));
        // 11 个参数
        assert!(cfg.search_space.len() >= 10);
        assert_eq!(cfg.n_trials(), 100);
        assert_eq!(cfg.n_jobs(), 4);
    }

    #[test]
    fn test_sampler_config_default() {
        let cfg = SamplerConfig::default();
        match cfg.sampler_type {
            SamplerType::Tpe {
                n_startup_trials, ..
            } => {
                assert_eq!(n_startup_trials, 10);
            }
            _ => panic!("expected Tpe"),
        }
    }

    #[test]
    fn test_pruner_config_default() {
        let cfg = PrunerConfig::default();
        match cfg.pruner_type {
            PrunerType::MedianPruner {
                n_startup_trials, ..
            } => {
                assert_eq!(n_startup_trials, 5);
            }
            _ => panic!("expected MedianPruner"),
        }
    }
}
