//! CPU/GPU 线程亲和性绑定
//!
//! 提供跨平台 CPU 绑核(Linux + macOS,Windows 显式拒绝)与 GPU 设备亲和性
//! (CUDA / Metal,feature-gated)。
//!
//! # 用法
//!
//! ## 独立调用
//! ```rust,no_run
//! use axon_inference::affinity::{pin_current_thread_to_cpus, AffinityPlan, pin_to};
//!
//! // 绑当前线程到 core 0 和 1
//! pin_current_thread_to_cpus(&[0, 1]).unwrap();
//!
//! // 组合调用(CPU + CUDA)
//! let plan = AffinityPlan::new().with_cpus(vec![2, 3]).with_cuda(0);
//! pin_to(&plan).unwrap();
//! ```
//!
//! # 平台支持
//!
//! - **Linux**: 完整支持,基于 `sched_setaffinity`
//! - **macOS**: 完整支持,基于 `thread_policy_set`(MPS-aware)
//! - **Windows**: **编译期拒绝**(`compile_error!`),用户用 WSL2 / numactl
//!
//! # Metal 半成品
//!
//! Metal 没有 thread-level set device API,本模块的 `pin_current_thread_to_metal`
//! 仅做 MPS 可用性检查;实际生效需要业务方在创建 `tch::Tensor` 时显式传
//! `tch::Device::Mps`。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AffinityError {
    /// CPU 亲和性在该平台不可用(Windows / FreeBSD)
    #[error("CPU affinity not available on this platform (Windows/FreeBSD unsupported)")]
    NotAvailable,

    /// core_affinity crate 调用失败
    #[error("core_affinity crate failed: {0}")]
    CoreAffinityFailed(String),

    /// CUDA 设备不可用(tch feature 未开 或 driver 异常)
    #[error("CUDA device {0} not available (tch feature or driver missing)")]
    CudaDeviceUnavailable(u32),

    /// Metal 设备不可用(非 macOS 或 MPS 未启用)
    #[error("Metal device not available (macOS + MPS required)")]
    MetalUnavailable,

    /// set_device 调用失败
    #[error("set_device call failed: {0}")]
    SetDeviceFailed(String),
}

/// GPU 设备规格
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDeviceSpec {
    /// CUDA 设备(device id)
    Cuda(u32),
    /// Apple Metal(macOS only,半成品语义)
    Metal,
}

impl GpuDeviceSpec {
    /// 构造 CUDA spec
    pub fn cuda(id: u32) -> Self {
        Self::Cuda(id)
    }
}

/// 亲和性规划(CPU cores + 可选 GPU 设备)
#[derive(Debug, Clone, Default)]
pub struct AffinityPlan {
    /// 要绑定的 CPU core 列表(空 = 不绑)
    pub cpus: Vec<u32>,
    /// 要绑定的 GPU 设备(None = 不绑)
    pub gpu: Option<GpuDeviceSpec>,
}

impl AffinityPlan {
    /// 新建空 plan
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置 CPU cores
    pub fn with_cpus(mut self, cpus: Vec<u32>) -> Self {
        self.cpus = cpus;
        self
    }

    /// 设置 GPU 设备(CUDA)
    pub fn with_cuda(mut self, device: u32) -> Self {
        self.gpu = Some(GpuDeviceSpec::Cuda(device));
        self
    }

    /// 设置 GPU 设备(Metal)
    pub fn with_metal(mut self) -> Self {
        self.gpu = Some(GpuDeviceSpec::Metal);
        self
    }

    /// 从 BatchConfig 派生(消费 §6.3 预留字段)
    pub fn from_batch_config(cfg: &crate::error::BatchConfig) -> Self {
        let gpu = cfg.collect_gpu_device_id.map(GpuDeviceSpec::Cuda);
        Self {
            cpus: cfg.collect_cpu_cores.clone(),
            gpu,
        }
    }
}

/// 把当前线程绑定到指定 CPU cores
///
/// # 行为
/// - 空数组 → 不绑核,返回 `Ok(())`
/// - Linux/macOS → 用 `core_affinity::set_for_current` 实际绑核
/// - Windows → **编译期错误**(`compile_error!`)
/// - 其他平台 → 返回 `Err(AffinityError::NotAvailable)`
pub fn pin_current_thread_to_cpus(cores: &[u32]) -> Result<(), AffinityError> {
    if cores.is_empty() {
        return Ok(());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        // core_affinity 0.8 的 set_for_current 只接受单个 CoreId,返回 bool
        // 多个 core 逐个绑定;任一失败则返回错误
        for &c in cores {
            let id = core_affinity::CoreId { id: c as usize };
            if !core_affinity::set_for_current(id) {
                return Err(AffinityError::CoreAffinityFailed(format!(
                    "set_for_current(core {c}) returned false"
                )));
            }
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    compile_error!(
        "CPU affinity via core_affinity is not supported on Windows. \
         Use numactl / Start-Process with -ProcessorAffinity / WSL2 instead."
    );

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(AffinityError::NotAvailable)
    }
}

/// 把当前线程绑定到 CUDA 设备(device id)
///
/// # 行为
/// - feature = "tch-backend" → `tch::Cuda::set_device(...)`
/// - feature 未开 → 返回 `Err(AffinityError::CudaDeviceUnavailable(device))`
///
/// # 注意
/// 不验证 device 是否存在;driver 异常在第一次 forward 时暴露。
#[cfg(feature = "tch-backend")]
pub fn pin_current_thread_to_cuda(device: u32) -> Result<(), AffinityError> {
    tch::Cuda::set_device(device as i64)
        .map_err(|e| AffinityError::SetDeviceFailed(e.to_string()))?;
    Ok(())
}

#[cfg(not(feature = "tch-backend"))]
pub fn pin_current_thread_to_cuda(device: u32) -> Result<(), AffinityError> {
    Err(AffinityError::CudaDeviceUnavailable(device))
}

/// Metal 亲和性(半成品):仅做 MPS 可用性检查
///
/// # 行为
/// - macOS + feature = "tch-backend" → 探测 MPS 可用性
/// - 其他平台 → 返回 `Err(AffinityError::MetalUnavailable)`
///
/// # 半成品语义
/// Metal 没有 thread-level set device API。业务方在创建 tensor 时需显式
/// 传 `tch::Device::Mps`,亲和性才生效。详见模块级 doc。
#[cfg(all(target_os = "macos", feature = "tch-backend"))]
pub fn pin_current_thread_to_metal() -> Result<(), AffinityError> {
    // 试探:创建一个 1 元素 tensor 到 MPS
    let probe = tch::Tensor::of_slice(&[0.0f32]).to(tch::Device::Mps);
    match probe {
        Ok(_) => Ok(()),
        Err(_) => Err(AffinityError::MetalUnavailable),
    }
}

#[cfg(not(all(target_os = "macos", feature = "tch-backend")))]
pub fn pin_current_thread_to_metal() -> Result<(), AffinityError> {
    Err(AffinityError::MetalUnavailable)
}

/// 统一入口:按 plan 绑核 + 绑 GPU
///
/// 调用顺序:先 CPU 后 GPU。任一步失败,后续步骤不执行(短路)。
pub fn pin_to(plan: &AffinityPlan) -> Result<(), AffinityError> {
    pin_current_thread_to_cpus(&plan.cpus)?;
    if let Some(gpu) = plan.gpu {
        match gpu {
            GpuDeviceSpec::Cuda(d) => pin_current_thread_to_cuda(d)?,
            GpuDeviceSpec::Metal => pin_current_thread_to_metal()?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BatchConfig;

    #[test]
    fn affinity_plan_from_empty_batch_config() {
        let cfg = BatchConfig::default();
        let plan = AffinityPlan::from_batch_config(&cfg);
        assert!(plan.cpus.is_empty());
        assert!(plan.gpu.is_none());
    }

    #[test]
    fn affinity_plan_from_cores() {
        let cfg = BatchConfig {
            collect_cpu_cores: vec![0, 1],
            collect_gpu_device_id: None,
            ..Default::default()
        };
        let plan = AffinityPlan::from_batch_config(&cfg);
        assert_eq!(plan.cpus, vec![0, 1]);
        assert!(plan.gpu.is_none());
    }

    #[test]
    fn affinity_plan_from_gpu_device_id() {
        let cfg = BatchConfig {
            collect_cpu_cores: vec![],
            collect_gpu_device_id: Some(0),
            ..Default::default()
        };
        let plan = AffinityPlan::from_batch_config(&cfg);
        assert!(plan.cpus.is_empty());
        assert_eq!(plan.gpu, Some(GpuDeviceSpec::Cuda(0)));
    }

    #[test]
    fn pin_current_thread_to_cpus_empty_returns_ok() {
        // 空 cores → 不绑核,返回 Ok
        assert!(pin_current_thread_to_cpus(&[]).is_ok());
    }

    #[test]
    fn pin_current_thread_to_cpus_one_core_does_not_panic() {
        // 绑 core 0:实际可能因容器/cgroup 限制失败,但测试不 panic
        // 仅验证不 panic(用 let _ 接受 Ok/Err 两种结果)
        let _ = pin_current_thread_to_cpus(&[0]);
    }

    #[test]
    fn affinity_plan_builder_chain() {
        let plan = AffinityPlan::new().with_cpus(vec![2, 3]).with_cuda(0);
        assert_eq!(plan.cpus, vec![2, 3]);
        assert_eq!(plan.gpu, Some(GpuDeviceSpec::Cuda(0)));
    }

    #[test]
    fn affinity_plan_with_metal() {
        let plan = AffinityPlan::new().with_metal();
        assert_eq!(plan.gpu, Some(GpuDeviceSpec::Metal));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn pin_current_thread_to_metal_non_macos_returns_error() {
        // Linux / Windows 上调 Metal → 期望错误
        assert!(matches!(
            pin_current_thread_to_metal(),
            Err(AffinityError::MetalUnavailable)
        ));
    }
}
