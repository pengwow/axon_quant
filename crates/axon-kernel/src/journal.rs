//! 事件溯源日志
//!
//! journal 记录的是**内核收到的外部输入**（行情 `BarClosed`、live 的交易所回报），
//! 不记内核 / sink 派生出来的订单与成交事件，这样重演语义才闭合：派生事件会在
//! 重演时被同一段逻辑再派生一次，若也落盘就会重复计数。
//!
//! 落盘格式是 append-only JSONL：第一行 [`RunManifest`]，其后每行一个
//! [`JournalEntry`]，即 `{"event": <Event>, "state": <Value|null>}`。
//!
//! 崩溃后恢复走 [`Journal::resume`]：同一套逐行校验，额外容忍文件末尾一行没收尾
//! 的半行（截掉后以追加模式继续写），供内核 `resume_replay` 使用。
//!
//! `state` 是这条外部输入在当帧的逐 bar state（编排层注入、不在
//! `MarketDataPayload` 里），无状态时写 `null`。它必须落盘：带 state 决策的
//! 策略重演时如果拿不到同一份 state，会静默按 `Value::Null` 产出一套不同结果。
//! 缺 `state` 键的行属于旧格式，读回时直接拒绝，不做「按 null 兼容」。
//!
//! 写入在进入内存 vec **之前**完成并 `flush`，写失败即向上抛错，主回路不得越过
//! 这一步继续推进（否则 journal 与实际推进轨迹出现空洞，重演结果不可信）。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use axon_core::Event;

use crate::KernelError;
use crate::persist::{JOURNAL_FORMAT_VERSION, RunManifest};

/// journal 中的一条记录：外部输入事件 + 该事件在当帧的逐 bar state
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    /// 外部输入事件（与首跑时 [`Journal::append`] 收到的完全一致）
    pub event: Event,
    /// 该事件的逐 bar state
    ///
    /// `None` 与 `Some(Value::Null)` 落盘后都是 `null`，读回统一成 `None`；
    /// 两者重演语义等价（都表示「没有 state」）。
    pub state: Option<serde_json::Value>,
}

/// 事件日志（内存 + 可选落盘）
///
/// `Journal::new()` 只留内存（单测 / 不落盘的重演）；`Journal::create` 额外打开
/// JSONL 文件，之后每次 `append` 都先写盘再进内存。
#[derive(Debug, Default)]
pub struct Journal {
    events: Vec<Event>,
    /// 与 `events` 等长、同下标对应：第 i 条事件的 state
    ///
    /// 用平行 vec 而不是 `Vec<JournalEntry>`，是为了让 [`Journal::events`] 仍能
    /// 零拷贝返回 `&[Event]`（既有调用依赖这个签名）。
    states: Vec<Option<serde_json::Value>>,
    /// `None` = 纯内存日志
    file: Option<BufWriter<File>>,
}

impl Journal {
    /// 创建空的内存日志（不落盘）
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建落盘日志：截断文件并写入首行 manifest
    ///
    /// 文件已存在时**截断**（每次运行一份新 journal），不会追加到旧文件——
    /// 追加会让首行 manifest 与后半段事件对不上。
    pub fn create(path: &Path, manifest: &RunManifest) -> Result<Self, KernelError> {
        let file = File::create(path).map_err(|e| {
            KernelError::Journal(format!("无法创建 journal 文件 {}: {e}", path.display()))
        })?;
        let mut writer = BufWriter::new(file);
        // 首行的版本必须描述本次实际写出的格式，不信任调用方传入的旧值：
        // 否则会出现「manifest 自称 v0/v9，文件却是 v1」的头文不符
        let mut head_manifest = manifest.clone();
        head_manifest.journal_format = JOURNAL_FORMAT_VERSION;
        let head = serde_json::to_string(&head_manifest)
            .map_err(|e| KernelError::Journal(format!("manifest 序列化失败: {e}")))?;
        writeln!(writer, "{head}")
            .and_then(|()| writer.flush())
            .map_err(|e| {
                KernelError::Journal(format!("写 journal 首行失败 {}: {e}", path.display()))
            })?;
        Ok(Self {
            events: Vec::new(),
            states: Vec::new(),
            file: Some(writer),
        })
    }

    /// 追加一条事件及其逐 bar state
    ///
    /// 落盘模式下**先写文件并 flush，写成功后才进内存 vec 并返回 `Ok`**；
    /// 调用方（`Kernel::enqueue_external`）必须把错误向上抛，不能吞掉继续跑。
    ///
    /// `state` 无状态传 `None`，落盘写成 `"state": null`（键始终存在，
    /// 读回时靠键是否出现区分新旧格式）。
    pub fn append(
        &mut self,
        ev: &Event,
        state: Option<serde_json::Value>,
    ) -> Result<(), KernelError> {
        if let Some(writer) = self.file.as_mut() {
            let line = serde_json::json!({ "event": ev, "state": &state }).to_string();
            writeln!(writer, "{line}")
                .and_then(|()| writer.flush())
                .map_err(|e| KernelError::Journal(format!("journal 写入失败: {e}")))?;
        }
        self.events.push(ev.clone());
        self.states.push(state);
        Ok(())
    }

    /// 读回 journal：`(manifest, 按文件顺序的外部输入条目)`
    ///
    /// 首行必须是合法 `RunManifest`（缺字段 / 非法 JSON 一律报错），其余每行必须是
    /// `{"event": ..., "state": ...}` 包裹；**任何一行坏掉都报错，不跳过**——跳过
    /// 等于悄悄改变输入序列，重演出来的结果是假的。错误信息统一带"拒绝重演"，
    /// 便于上层直接透出。
    ///
    /// 缺 `state` 键的行 = 旧格式 journal：那份文件没有逐 bar state，重演会静默
    /// 退化成 `Value::Null`，所以直接拒绝，而不是按 `null` 兼容。
    pub fn open(path: &Path) -> Result<(RunManifest, Vec<JournalEntry>), KernelError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            KernelError::Journal(format!("无法读取 journal 文件 {}: {e}", path.display()))
        })?;
        Self::parse_parts(&content, path)
    }

    /// 崩溃恢复：读回 journal 并接管续写，返回 `(manifest, 已落盘条目, 可续写的 journal)`
    ///
    /// 与 [`Journal::open`] 的差别在容错边界：崩溃可能发生在 `writeln!` 中途，文件
    /// 末尾留下没有 '\n' 结尾的半行。约定「flush 成功的行必然以 '\n' 结尾」，因此
    /// 最后一个 '\n' 之后的内容一律视为半行：先按完整前缀逐行校验，通过后把半行从
    /// 文件里截掉，再以 append 模式接管续写。半行里那条输入不重演——它对应的交易
    /// 所回报是否已生效，由上层对账阶段兜底，这里不猜。
    ///
    /// 前缀里任何一行损坏仍然拒绝（同 [`Journal::open`]）；半行截断在解析通过之后
    /// 才执行，解析失败不动文件。
    pub fn resume(path: &Path) -> Result<(RunManifest, Vec<JournalEntry>, Journal), KernelError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            KernelError::Journal(format!("无法读取 journal 文件 {}: {e}", path.display()))
        })?;
        let complete_end = content.rfind('\n').map_or(0, |i| i + 1);
        let has_partial_tail = !content[complete_end..].trim().is_empty();
        let kept = &content[..complete_end];

        // 先校验完整前缀：坏行 / 空 saved 文件在这里报错，文件保持原样
        let (manifest, entries) = Self::parse_parts(kept, path)?;

        if has_partial_tail {
            // read_to_string 已保证内容是合法 UTF-8，字符串字节长度即文件字节长度
            let file = File::options().write(true).open(path).map_err(|e| {
                KernelError::Journal(format!("无法打开 journal {}: {e}", path.display()))
            })?;
            file.set_len(kept.len() as u64).map_err(|e| {
                KernelError::Journal(format!("截断 journal 半行失败 {}: {e}", path.display()))
            })?;
        }

        let file = File::options().append(true).open(path).map_err(|e| {
            KernelError::Journal(format!(
                "无法以追加模式打开 journal {}: {e}",
                path.display()
            ))
        })?;
        let journal = Self {
            // 装入已落盘条目的克隆，保持 len() 口径连续：恢复出的 journal 从
            // 已落盘条数继续向后计数
            events: entries.iter().map(|e| e.event.clone()).collect(),
            states: entries.iter().map(|e| e.state.clone()).collect(),
            file: Some(BufWriter::new(file)),
        };
        Ok((manifest, entries, journal))
    }

    /// 逐行解析：首行 [`RunManifest`] + 其余 `{"event":..,"state":..}` 包裹
    ///
    /// [`Journal::open`]（全量严格）与 [`Journal::resume`]（容忍尾半行）共用。
    fn parse_parts(
        content: &str,
        path: &Path,
    ) -> Result<(RunManifest, Vec<JournalEntry>), KernelError> {
        let mut lines = content.lines();
        let head = lines.next().ok_or_else(|| {
            KernelError::Journal(format!(
                "journal 文件 {} 为空（缺 RunManifest 首行），拒绝重演",
                path.display()
            ))
        })?;
        let manifest: RunManifest = serde_json::from_str(head).map_err(|e| {
            KernelError::Journal(format!(
                "journal {} 首行不是合法 RunManifest（{e}），拒绝重演",
                path.display()
            ))
        })?;

        // 版本门禁：高于当前内核已知版本的 journal 可能带解析不了的字段 / 事件包裹，
        // 强行读回会静默丢掉新语义，直接拒绝。低于当前版本（含无版本字段的 v0 归档）
        // 只需满足下面的逐行格式校验即可。
        if manifest.journal_format > JOURNAL_FORMAT_VERSION {
            return Err(KernelError::Journal(format!(
                "journal {} 格式版本 {} 过新（当前内核支持到 v{}），拒绝重演",
                path.display(),
                manifest.journal_format,
                JOURNAL_FORMAT_VERSION
            )));
        }

        let mut entries = Vec::new();
        for (idx, line) in lines.enumerate() {
            // 行号从 1 起算，首行 manifest 占第 1 行，事件从第 2 行开始
            let line_no = idx + 2;
            let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                KernelError::Journal(format!(
                    "journal {} 第 {line_no} 行事件损坏（{e}），拒绝重演",
                    path.display()
                ))
            })?;
            let obj = value.as_object().ok_or_else(|| {
                KernelError::Journal(format!(
                    "journal {} 第 {line_no} 行不是 JSON 对象，拒绝重演",
                    path.display()
                ))
            })?;
            // 先查 version 标记（state 键），旧格式的事件行是裸 Event（如
            // `{"MarketData": ...}`），没有 event / state 包裹
            let state = obj.get("state").ok_or_else(|| {
                KernelError::Journal(format!(
                    "journal {} 第 {line_no} 行版本过旧：缺少 state 字段，拒绝重演",
                    path.display()
                ))
            })?;
            let event_value = obj.get("event").ok_or_else(|| {
                KernelError::Journal(format!(
                    "journal {} 第 {line_no} 行缺少 event 字段，拒绝重演",
                    path.display()
                ))
            })?;
            let event: Event = serde_json::from_value(event_value.clone()).map_err(|e| {
                KernelError::Journal(format!(
                    "journal {} 第 {line_no} 行事件损坏（{e}），拒绝重演",
                    path.display()
                ))
            })?;
            entries.push(JournalEntry {
                event,
                // null 与缺失在落盘后不可区分，统一按「无 state」处理
                state: (!state.is_null()).then(|| state.clone()),
            });
        }
        Ok((manifest, entries))
    }

    /// 已记录的事件数
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 全部已记录事件（按到达顺序，零拷贝）
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// 带 state 的完整视图（按到达顺序）
    ///
    /// `events()` 只给事件本体；需要 state（重演 / 导出）时用本方法。返回克隆，
    /// 不持有内部引用。
    pub fn entries(&self) -> Vec<JournalEntry> {
        self.events
            .iter()
            .cloned()
            .zip(self.states.iter().cloned())
            .map(|(event, state)| JournalEntry { event, state })
            .collect()
    }

    /// 取走全部事件（落盘 / 交接用）
    ///
    /// 注意：只清内存，不影响已落盘的文件内容。
    pub fn drain(&mut self) -> Vec<Event> {
        self.states.clear();
        std::mem::take(&mut self.events)
    }
}

#[cfg(test)]
mod tests {
    use axon_core::{SystemAction, SystemEvent, Timestamp};

    use super::*;
    use crate::persist::ModelArtifact;

    fn manifest() -> RunManifest {
        RunManifest {
            journal_format: JOURNAL_FORMAT_VERSION,
            code_version: "0.22.0-test".to_string(),
            config_hash: "cfg-abc".to_string(),
            data_hash: "data-def".to_string(),
            seed: 42,
            model_artifacts: vec![ModelArtifact {
                model_id: "ppo_btc".to_string(),
                spec_version: "spec-3".to_string(),
                normalizer_hash: "norm-xyz".to_string(),
            }],
            shadow_cash: None,
            initial_positions: None,
        }
    }

    fn heartbeat() -> Event {
        Event::System(SystemEvent::new(
            7,
            Timestamp::from_nanos(1_700_000_000_000_000_000),
            SystemAction::Heartbeat,
        ))
    }

    #[test]
    fn test_journal_first_line_is_manifest() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("run.jsonl");
        let manifest = manifest();

        let mut journal = Journal::create(&path, &manifest).expect("创建 journal");
        journal.append(&heartbeat(), None).expect("写事件");

        let content = std::fs::read_to_string(&path).expect("读回文件");
        let first = content.lines().next().expect("至少一行");
        let parsed: RunManifest = serde_json::from_str(first).expect("首行应是 RunManifest");

        // 六个字段必须都在且值原样（缺字段 serde 会直接报 missing field）
        assert_eq!(parsed.journal_format, JOURNAL_FORMAT_VERSION);
        assert_eq!(parsed.code_version, "0.22.0-test");
        assert_eq!(parsed.config_hash, "cfg-abc");
        assert_eq!(parsed.data_hash, "data-def");
        assert_eq!(parsed.seed, 42);
        assert_eq!(parsed.model_artifacts, manifest.model_artifacts);
        assert_eq!(parsed, manifest);
        assert_eq!(content.lines().count(), 2, "首行 manifest + 1 行事件");

        // 事件行是 {"event": ..., "state": ...} 包裹，state 键必须存在
        let second: serde_json::Value =
            serde_json::from_str(content.lines().nth(1).expect("事件行")).expect("事件行应是 JSON");
        assert!(second.get("event").is_some(), "事件必须包在 event 键里");
        let state = second
            .get("state")
            .expect("state 键必须存在（旧格式会被拒绝）");
        assert!(state.is_null(), "无状态时 state 必须是 null，实际 {state}");
    }

    #[test]
    fn test_journal_rejects_invalid_manifest() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("bad_head.jsonl");
        // 缺 seed / model_artifacts：按 spec 必须拒绝，不能当默认值补齐
        std::fs::write(
            &path,
            "{\"code_version\":\"0.1.0\",\"config_hash\":\"c\",\"data_hash\":\"d\"}\n",
        )
        .expect("写坏首行");

        let err = Journal::open(&path).expect_err("缺字段的首行必须报错");
        let msg = err.to_string();
        assert!(msg.contains("拒绝重演"), "错误信息应说明拒绝重演: {msg}");
        assert!(msg.contains("RunManifest"), "错误信息应指明首行: {msg}");
    }

    #[test]
    fn test_journal_rejects_corrupted_event_line() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("bad_event.jsonl");
        let mut content = serde_json::to_string(&manifest()).expect("序列化 manifest");
        content.push('\n');
        content.push_str("this is not json\n");
        std::fs::write(&path, content).expect("写损坏文件");

        let err = Journal::open(&path).expect_err("损坏的事件行必须报错，不能静默跳过");
        let msg = err.to_string();
        assert!(msg.contains("第 2 行"), "错误信息应指出坏行行号: {msg}");
        assert!(msg.contains("拒绝重演"), "错误信息应说明拒绝重演: {msg}");
    }

    #[test]
    fn test_journal_records_per_bar_state() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("state.jsonl");
        let state = serde_json::json!({"sma5_vs_sma20": 0.1});

        let mut journal = Journal::create(&path, &manifest()).expect("创建 journal");
        journal
            .append(&heartbeat(), Some(state.clone()))
            .expect("写带 state 的事件");
        journal.append(&heartbeat(), None).expect("写无状态事件");

        let (_, entries) = Journal::open(&path).expect("读回 journal");
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].state.as_ref(),
            Some(&state),
            "state 必须原样读回"
        );
        assert_eq!(entries[1].state, None, "无状态行读回 None");

        // 内存视图与落盘内容同源，且 events() 仍只给事件本体
        let in_memory = journal.entries();
        assert_eq!(in_memory[0].state.as_ref(), Some(&state));
        assert_eq!(journal.events().len(), entries.len());
        for (ev, entry) in journal.events().iter().zip(&entries) {
            assert_eq!(ev, &entry.event, "events() 保持纯事件视图");
        }
    }

    #[test]
    fn test_journal_rejects_legacy_line_without_state() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("legacy.jsonl");
        let mut content = serde_json::to_string(&manifest()).expect("序列化 manifest");
        content.push('\n');
        // 旧格式：裸 Event 一行，没有 {"event":..,"state":..} 包裹
        content.push_str(&serde_json::to_string(&heartbeat()).expect("序列化事件"));
        content.push('\n');
        std::fs::write(&path, content).expect("写旧格式文件");

        let err = Journal::open(&path).expect_err("缺 state 键的旧格式必须拒绝，不能按 null 兼容");
        let msg = err.to_string();
        assert!(msg.contains("拒绝重演"), "错误信息应说明拒绝重演: {msg}");
        assert!(msg.contains("state"), "错误信息应指出缺 state 字段: {msg}");
        assert!(msg.contains("版本过旧"), "错误信息应说明版本过旧: {msg}");
    }

    #[test]
    fn test_journal_format_version_gate() {
        let dir = tempfile::tempdir().expect("临时目录");

        // 版本高于内核已知版本：直接拒绝，不去猜新版字段语义
        let newer = dir.path().join("newer.jsonl");
        let mut head = serde_json::to_value(manifest()).expect("序列化 manifest");
        head["journal_format"] = serde_json::json!(JOURNAL_FORMAT_VERSION + 1);
        std::fs::write(&newer, format!("{head}\n")).expect("写新版首行");
        let err = Journal::open(&newer).expect_err("过新的 journal 必须拒绝重演");
        let msg = err.to_string();
        assert!(msg.contains("过新"), "错误信息应说明版本过新: {msg}");
        assert!(msg.contains("拒绝重演"), "错误信息应说明拒绝重演: {msg}");

        // 无版本字段的 v0 归档：低于当前版本，按已知格式放行
        let legacy = dir.path().join("v0.jsonl");
        let mut v0 = serde_json::to_value(manifest()).expect("序列化 manifest");
        v0.as_object_mut()
            .expect("manifest 应是对象")
            .remove("journal_format");
        std::fs::write(&legacy, format!("{v0}\n")).expect("写无版本首行");
        let (manifest, entries) = Journal::open(&legacy).expect("v0 归档应能读回");
        assert_eq!(manifest.journal_format, 0, "缺字段读成 v0");
        assert!(entries.is_empty(), "文件只有首行 manifest");
    }

    #[test]
    fn test_journal_resume_tolerates_partial_tail() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("crash.jsonl");

        let mut journal = Journal::create(&path, &manifest()).expect("创建 journal");
        journal.append(&heartbeat(), None).expect("写事件 1");
        journal.append(&heartbeat(), None).expect("写事件 2");
        drop(journal); // 模拟崩溃：写手停止

        // 崩溃残留：writeln! 中途断电留下的、没有 '\n' 结尾的半行
        let mut content = std::fs::read_to_string(&path).expect("读文件");
        content.push_str("{\"event\": {\"System\"");
        std::fs::write(&path, content).expect("写回带半行的文件");

        let (head, entries, mut journal) = Journal::resume(&path).expect("半行应被截掉后恢复");
        assert_eq!(head.config_hash, "cfg-abc", "manifest 原样读回");
        assert_eq!(entries.len(), 2, "半行不重演，只返回完整前缀条目");

        // 接管后续写：新事件必须落在半行截断点之后，不产生叉行
        journal
            .append(&heartbeat(), None)
            .expect("恢复后续写事件 3");
        drop(journal);

        let (_, reopened) = Journal::open(&path).expect("续写后的文件应完整可读");
        assert_eq!(reopened.len(), 3, "2 条历史 + 1 条续写");
        assert_eq!(reopened[2].event, heartbeat(), "续写条目内容正确");
    }

    #[test]
    fn test_journal_resume_rejects_corrupted_middle_line() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("corrupted.jsonl");

        // 先用真实 append 写出两条合法条目，保证中部坏行的前后缀绝对合法
        let mut journal = Journal::create(&path, &manifest()).expect("创建 journal");
        journal.append(&heartbeat(), None).expect("写事件 1");
        journal.append(&heartbeat(), None).expect("写事件 2");
        drop(journal);

        // 重排：manifest / 合法事件 / 坏行 / 合法事件 —— 坏行落在文件第 3 行
        let lines: Vec<String> = std::fs::read_to_string(&path)
            .expect("读回文件")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 3, "manifest + 2 条事件");
        let corrupted = format!("{}\n{}\n{{broken json\n{}\n", lines[0], lines[1], lines[2]);
        std::fs::write(&path, &corrupted).expect("写回中部损坏文件");
        let before = std::fs::read(&path).expect("读损坏文件字节");

        let err = Journal::resume(&path).expect_err("中部坏行必须拒绝，不能静默跳过");
        let msg = err.to_string();
        assert!(msg.contains("第 3 行"), "错误信息应指出坏行行号: {msg}");
        assert!(msg.contains("拒绝重演"), "错误信息应说明拒绝重演: {msg}");

        // 解析失败不动文件：修复失败后崩溃现场必须原样保留，供人工排查
        let after = std::fs::read(&path).expect("读恢复后字节");
        assert_eq!(before, after, "resume 失败不得改动文件");
    }
}
