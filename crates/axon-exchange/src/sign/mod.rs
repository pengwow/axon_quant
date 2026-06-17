//! 各交易所签名实现
//!
//! - [`binance`]:HMAC-SHA256(secret, query_string),hex 编码
//! - [`okx`]:HMAC-SHA256(secret, timestamp + method + path + body),Base64 编码 + 4 header
//!
//! 设计动机:把签名算法从 adapter 实现里独立出来,便于
//! 1) 单元测试覆盖签名向量
//! 2) 未来加入更多交易所时复用基础设施
//! 3) 减少 adapter 文件中的样板代码

pub mod binance;
pub mod okx;
