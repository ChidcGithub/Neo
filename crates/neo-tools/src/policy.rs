//! 权限模型：谁来裁定一次工具调用该不该放行。
//!
//! 裁定只看**工具声明的风险等级**与**当前策略**，不看内容 ——
//! 内容级的判断交给用户（确认框里看 `preview`）。这样策略是可预测的：
//! 同一类工具永远得到同一种待遇。

use serde_json::Value;

use crate::result::Args;
use crate::spec::Tool;

/// 风险等级。决定默认要不要用户点头。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    /// 只读：读文件、看图。不改任何东西。
    Read,
    /// 会打开东西给用户看（起一个系统进程），但不改文件。
    Open,
    /// 改文件：写入与替换。
    Write,
    /// 执行任意命令 —— 能力上界最高的一类。
    Exec,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Read => "read",
            Risk::Open => "open",
            Risk::Write => "write",
            Risk::Exec => "exec",
        }
    }

    /// 是否改变了工作区（含"改变了机器状态"）。
    pub fn mutates(self) -> bool {
        matches!(self, Risk::Write | Risk::Exec)
    }
}

/// 裁定结果。
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// 直接执行。
    Allow,
    /// 需要用户确认；字符串是给确认框看的一行"将要发生什么"。
    Confirm(String),
    /// 直接拒绝；字符串是给模型看的理由。
    Deny(String),
}

/// 当前策略。由 UI 的开关映射而来。
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// 输入卡的「只读」开关：打开后写与执行一律拒绝。
    pub read_only: bool,
    /// 用户显式信任本会话（"全部允许"）。默认关。
    pub auto_approve: bool,
    /// 是否允许 `open_file` 拉起系统程序。
    pub allow_open: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            read_only: false,
            auto_approve: false,
            allow_open: true,
        }
    }
}

impl Policy {
    /// 只读策略（最小权限的默认姿态）。
    pub fn read_only() -> Self {
        Self {
            read_only: true,
            ..Self::default()
        }
    }

    /// 裁定一次调用。
    ///
    /// `tool` 要求 `&'static`：工具一律来自 [`crate::registry`]，
    /// 拿掉这个约束只会让"运行期拼出来的假工具"有机会绕过策略表。
    pub fn decide(&self, tool: &'static Tool, args: &Value) -> Decision {
        let a = Args::new(tool, args);
        // 参数先过一遍：参数都不合法的调用，没必要打扰用户。
        if let Err(e) = a.reject_unknown() {
            return Decision::Deny(format!("参数不合法：{}", e.message));
        }
        if self.read_only && tool.risk.mutates() {
            return Decision::Deny(
                "当前处于「只读」模式：写文件与执行命令已被禁用，请让用户关闭只读后重试".to_owned(),
            );
        }
        match tool.risk {
            Risk::Read => Decision::Allow,
            Risk::Open => {
                if self.allow_open {
                    Decision::Allow
                } else {
                    Decision::Deny("当前策略不允许打开本机文件".to_owned())
                }
            }
            Risk::Write | Risk::Exec => {
                if self.auto_approve {
                    Decision::Allow
                } else {
                    Decision::Confirm((tool.preview)(&a))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_tools_never_ask() {
        let p = Policy::default();
        let tool = crate::find("read_file").unwrap();
        assert_eq!(p.decide(tool, &json!({ "path": "a.txt" })), Decision::Allow);
    }

    #[test]
    fn write_tools_ask_unless_trusted() {
        let tool = crate::find("write_file").unwrap();
        let args = json!({ "path": "a.txt", "content": "hi" });
        assert!(matches!(
            Policy::default().decide(tool, &args),
            Decision::Confirm(_)
        ));
        let trusted = Policy {
            auto_approve: true,
            ..Policy::default()
        };
        assert_eq!(trusted.decide(tool, &args), Decision::Allow);
    }

    #[test]
    fn read_only_denies_mutations() {
        let p = Policy::read_only();
        let exec = crate::find("powershell").unwrap();
        let args = json!({ "command": "Get-ChildItem" });
        assert!(matches!(p.decide(exec, &args), Decision::Deny(_)));
        let read = crate::find("read_file").unwrap();
        assert_eq!(p.decide(read, &json!({ "path": "a.txt" })), Decision::Allow);
    }

    #[test]
    fn unknown_argument_is_denied_before_asking() {
        let p = Policy::default();
        let tool = crate::find("write_file").unwrap();
        let d = p.decide(
            tool,
            &json!({ "path": "a.txt", "content": "x", "rm_rf": true }),
        );
        assert!(matches!(d, Decision::Deny(_)));
    }
}
