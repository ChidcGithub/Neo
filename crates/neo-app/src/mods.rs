//! MOD 只读发现与「启用意图」的本地胶水。
//!
//! Phase 1 不启动任何进程：开关只记录「允许加载该 MOD」的意图，
//! 意图绑定 `id + revision + capabilities`，任一不符即视为需重新确认。
//! `Manifest` 里的能力与限制都是**声明值**，不是实际授权。

use neo_tools::mods::{
    discovery::{self, DiscoveredMod},
    Capability, Manifest,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// MOD 根目录：`NEO_MODS_DIR` 覆盖，否则可执行文件同级的 `mods`。
/// 不创建目录 —— 缺失由发现层按「MOD root missing」fail-closed。
pub fn root() -> PathBuf {
    match std::env::var_os("NEO_MODS_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => crate::state::app_dir().join("mods"),
    }
}

/// 单个 MOD 的开关状态（由启用意图与当前 manifest 对账得出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModStatus {
    /// 没有启用意图。
    Off,
    /// 意图与当前 manifest 完全一致。
    On,
    /// 有旧意图，但 revision 或能力变了，需要重新打开开关重新确认。
    NeedsReconfirm,
}

/// 已确认的启用意图：绑定 id（map 键）+ revision + capabilities。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub revision: u32,
    pub capabilities: Vec<Capability>,
}

impl Grant {
    /// 从当前 manifest 构造意图；能力排序保证编码与比对都确定。
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut capabilities = manifest.capabilities.clone();
        capabilities.sort();
        Self {
            revision: manifest.revision,
            capabilities,
        }
    }

    /// revision 与能力集合都一致才算有效意图。
    pub fn matches(&self, manifest: &Manifest) -> bool {
        self.revision == manifest.revision && {
            let mut capabilities = manifest.capabilities.clone();
            capabilities.sort();
            capabilities == self.capabilities
        }
    }
}

/// 一个已发现 MOD 的展示模型。
#[derive(Debug)]
pub struct ModRow {
    pub manifest: Manifest,
    pub directory: PathBuf,
    pub executable: PathBuf,
    pub status: ModStatus,
    /// 详情是否展开（纯 UI 状态）。
    pub expanded: bool,
}

impl From<&DiscoveredMod> for ModRow {
    fn from(entry: &DiscoveredMod) -> Self {
        Self {
            manifest: entry.manifest().manifest().clone(),
            directory: entry.directory().to_path_buf(),
            executable: entry.executable().to_path_buf(),
            status: ModStatus::Off,
            expanded: false,
        }
    }
}

/// 按启用意图更新每行状态：无意图 → Off，意图失效 → NeedsReconfirm。
pub fn reconcile(rows: &mut [ModRow], grants: &BTreeMap<String, Grant>) {
    for row in rows {
        row.status = match grants.get(&row.manifest.id) {
            None => ModStatus::Off,
            Some(grant) if grant.matches(&row.manifest) => ModStatus::On,
            Some(_) => ModStatus::NeedsReconfirm,
        };
    }
}

/// 只读扫描。任何失败整批作废（fail-closed），错误是契约层的静态文案。
pub fn scan(root: &Path) -> Result<Vec<ModRow>, String> {
    let discovery = discovery::discover(root).map_err(|error| error.0.to_string())?;
    Ok(discovery.entries().iter().map(ModRow::from).collect())
}

/// 容错解码：整体不是对象时给空表；坏条目跳过（fail-toward-disabled）。
pub fn decode_grants(raw: &str) -> BTreeMap<String, Grant> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return BTreeMap::new();
    };
    let Some(object) = value.as_object() else {
        return BTreeMap::new();
    };
    object
        .iter()
        .filter_map(|(id, entry)| {
            serde_json::from_value::<Grant>(entry.clone())
                .ok()
                .map(|grant| (id.clone(), grant))
        })
        .collect()
}

/// 键序由 BTreeMap 保证确定，便于差分比较「有没有变化」。
pub fn encode_grants(grants: &BTreeMap<String, Grant>) -> String {
    serde_json::to_string(grants).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
#[path = "mods_tests.rs"]
mod tests;
