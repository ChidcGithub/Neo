//! 工作区围栏：所有 `path` 参数都必须经过这里。
//!
//! 两道防线：
//! 1. **词法归一**：`..` 先被吃掉再判是否仍在根内 —— 挡住 `../../../etc/passwd`；
//! 2. **真实路径复查**：目标若已存在，再 `canonicalize` 一次并重新判边界 ——
//!    挡住「工作区内放一个指向外面的符号链接」这种绕行。

use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::result::ToolError;

/// 工作区根。`Neo` 里来自 `AppState::workspace`（空态时是当前工作目录）。
#[derive(Clone, Debug)]
pub struct Scope {
    root: PathBuf,
    cancel: Option<Arc<AtomicBool>>,
}

impl Scope {
    /// 以 `root` 为围栏。根不存在时退回"当前目录 + 原路径"的绝对化结果，
    /// 不因为目录还没建起来就把工具全禁用掉。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or_else(|_| {
            let abs = absolutize(&root);
            abs.canonicalize().unwrap_or(abs)
        });
        Self {
            root: normalize(&root),
            cancel: None,
        }
    }

    /// 为本次调用绑定取消标记；克隆的 Scope 共享同一个标记。
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 解析模型的 `path` 参数。
    ///
    /// - 相对路径 → 相对工作区根；
    /// - 绝对路径 → 必须落在工作区内，否则 [`crate::ErrorKind::NotAllowed`]。
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ToolError::bad_args("`path` 不能为空"));
        }
        if raw.contains('\0') {
            return Err(ToolError::bad_args("`path` 含非法字符"));
        }
        let candidate = {
            let p = Path::new(raw);
            let joined = if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.root.join(p)
            };
            normalize(&joined)
        };
        if !candidate.starts_with(&self.root) {
            return Err(ToolError::not_allowed(format!(
                "`{raw}` 解析到工作区之外（{}）",
                self.root.display()
            ))
            .with_hint("所有工具的路径都必须落在当前工作区内；工作区外的操作请让用户手动处理"));
        }
        Ok(candidate)
    }

    /// 目标已存在时，再按真实路径（化解符号链接）复查一次边界。
    ///
    /// 返回化解后的路径 —— 后续读写用它，避免中间被换掉。
    pub fn verify_existing(&self, path: &Path) -> Result<PathBuf, ToolError> {
        let real = path
            .canonicalize()
            .map_err(|e| ToolError::io(format!("无法访问 {}：{e}", self.display(path))))?;
        let real = normalize(&real);
        if !real.starts_with(&self.root) {
            return Err(ToolError::not_allowed(format!(
                "{} 是指向工作区之外的链接",
                self.display(path)
            )));
        }
        Ok(real)
    }

    /// 目标尚不存在时的边界复查：顺**最近一个已存在的祖先**化解符号链接，
    /// 真实落点在工作区外则拒绝。词法路径在围栏内不代表真实落点在围栏内
    /// （`ws\linkdir\new.txt`：linkdir 若是指向外的 Junction，写入就逃逸了）。
    pub fn verify_new(&self, path: &Path) -> Result<(), ToolError> {
        let mut ancestor = path.parent();
        while let Some(p) = ancestor {
            if p.exists() {
                let real = p
                    .canonicalize()
                    .map_err(|e| ToolError::io(format!("无法访问 {}：{e}", self.display(p))))?;
                let real = normalize(&real);
                if !real.starts_with(&self.root) {
                    return Err(ToolError::not_allowed(format!(
                        "{} 经过指向工作区之外的目录链接",
                        self.display(path)
                    )));
                }
                return Ok(());
            }
            ancestor = p.parent();
        }
        Ok(())
    }

    /// 输出用的相对路径（永远用 `/` 分隔，跨平台一致）。
    pub fn display(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let s = rel.to_string_lossy().replace('\\', "/");
        if s.is_empty() {
            ".".to_owned()
        } else {
            s
        }
    }
}

/// 绝对化（不碰文件系统）：相对路径按当前目录展开。
fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        match std::env::current_dir() {
            Ok(cwd) => normalize(&cwd.join(p)),
            Err(_) => normalize(p),
        }
    }
}

/// 词法归一：吃掉 `.` 与 `..`。**不做任何 I/O**，所以对不存在的路径同样有效。
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            #[cfg(windows)]
            Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                    out.push(format!(r"\\?\{}:", char::from(drive.to_ascii_uppercase())));
                }
                std::path::Prefix::UNC(server, share)
                | std::path::Prefix::VerbatimUNC(server, share) => {
                    let mut value = std::ffi::OsString::from(r"\\?\UNC\");
                    value.push(server);
                    value.push(r"\");
                    value.push(share);
                    out.push(value);
                }
                _ => out.push(prefix.as_os_str()),
            },
            Component::CurDir => {}
            Component::ParentDir => {
                // pop 失败说明已经到根/前缀了，此时 `..` 无意义，直接丢弃。
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope::new(std::env::temp_dir().join("neo-scope-test"))
    }

    #[test]
    fn cancellation_is_optional_and_shared_by_clones() {
        let original = scope();
        assert!(!original.is_cancelled());
        let token = Arc::new(AtomicBool::new(false));
        let bound = original.clone().with_cancel(token.clone());
        let cloned = bound.clone();
        assert!(!cloned.is_cancelled());
        token.store(true, Ordering::Release);
        assert!(bound.is_cancelled());
        assert!(cloned.is_cancelled());
        assert!(!original.is_cancelled());
    }

    #[test]
    fn relative_paths_land_inside_root() {
        let s = scope();
        let p = s.resolve("src/main.rs").unwrap();
        assert!(p.starts_with(s.root()));
        assert_eq!(s.display(&p), "src/main.rs");
    }

    #[test]
    fn traversal_escape_is_rejected() {
        let s = scope();
        for bad in [
            "../../etc/passwd",
            "a/../../../../etc/passwd",
            "/etc/passwd",
        ] {
            let err = s.resolve(bad).unwrap_err();
            assert_eq!(err.kind, crate::ErrorKind::NotAllowed, "{bad} 应被拒绝");
        }
    }

    #[test]
    fn inner_dotdot_is_fine() {
        let s = scope();
        // a/../b 仍在根内，属于正常写法，不该被误杀。
        let p = s.resolve("a/../b/c.txt").unwrap();
        assert_eq!(s.display(&p), "b/c.txt");
    }

    #[cfg(windows)]
    #[test]
    fn windows_plain_and_verbatim_paths_share_one_fence() {
        let s = Scope::new(r"C:\neo-scope-absolute-test");
        for path in [
            r"C:\neo-scope-absolute-test\a.txt",
            r"\\?\C:\neo-scope-absolute-test\a.txt",
            r"c:\neo-scope-absolute-test\a.txt",
        ] {
            assert_eq!(s.display(&s.resolve(path).unwrap()), "a.txt");
        }
        assert_eq!(
            s.resolve(r"C:\neo-scope-absolute-test\..\outside.txt")
                .unwrap_err()
                .kind,
            crate::ErrorKind::NotAllowed
        );
        let unc = Scope {
            root: normalize(Path::new(r"\\server\share\workspace")),
            cancel: None,
        };
        assert_eq!(
            unc.display(&unc.resolve(r"\\server\share\workspace\a.txt").unwrap()),
            "a.txt"
        );
        assert_eq!(
            unc.resolve(r"\\server\other\workspace\a.txt")
                .unwrap_err()
                .kind,
            crate::ErrorKind::NotAllowed
        );
    }

    #[test]
    fn empty_and_nul_rejected() {
        let s = scope();
        assert_eq!(
            s.resolve("  ").unwrap_err().kind,
            crate::ErrorKind::BadArguments
        );
        assert_eq!(
            s.resolve("a\0b").unwrap_err().kind,
            crate::ErrorKind::BadArguments
        );
    }
}
