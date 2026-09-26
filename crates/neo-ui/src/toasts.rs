//! toast 队列：以 [egui-toast](https://github.com/urholaukkarinen/egui-toast)
//! 0.22（MIT © Urho Laukkarinen）为底的裁剪 vendor 版。
//!
//! 为什么 vendor 而不是直接依赖：
//!
//! 1. **上游的 toast 层写死 `interactable(true)`**（悬停暂停倒计时用）。
//!    代价是它盖住谁，谁就点不动 —— 底部中央的 toast 会把输入卡的
//!    附件区 / 工具栏盖进「最长 4 秒点不动」的状态（有回归测试实锤）。
//!    大屏上提示层必须让点击穿得过去，这里改成不可交互。
//! 2. 上游每个版本锁死一个 egui 小版本，升级节奏被动；vendor 后自己掌握。
//!
//! 裁剪掉的：emoji 默认外观（Neo 走组件库自绘）、进度条、悬停暂停、
//! 点击关闭。保留的：队列（egui memory）、到期回收、锚定与堆叠、
//! `Toast` / `ToastKind` / `ToastOptions` 的 API 形状。

use std::time::Duration;

use egui::{Align2, Area, Direction, Id, Order, Pos2, Ui};

/// toast 类别（决定图标与强调色）。
#[derive(Default, Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum ToastKind {
    #[default]
    Info,
    Warning,
    Error,
    Success,
    #[allow(dead_code)]
    Custom(u32),
}

/// 一条待显示的 toast。
#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub options: ToastOptions,
}

impl Toast {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn kind(mut self, kind: ToastKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    pub fn options(mut self, options: ToastOptions) -> Self {
        self.options = options;
        self
    }
}

impl Default for Toast {
    fn default() -> Self {
        Self {
            kind: ToastKind::default(),
            text: String::new(),
            options: ToastOptions::default(),
        }
    }
}

/// 生命周期选项。`None` 时长 = 永不过期。
#[derive(Debug, Copy, Clone)]
pub struct ToastOptions {
    /// 剩余存活秒数（队列每帧递减，归零即弃）。
    pub(crate) ttl_sec: f64,
}

impl Default for ToastOptions {
    fn default() -> Self {
        Self {
            ttl_sec: f64::INFINITY,
        }
    }
}

impl ToastOptions {
    pub fn duration(mut self, duration: impl Into<Option<Duration>>) -> Self {
        self.ttl_sec = duration
            .into()
            .map_or(f64::INFINITY, |d| d.as_secs_f64());
        self
    }

    pub fn duration_in_seconds(self, secs: f64) -> Self {
        self.duration(Duration::from_secs_f64(secs))
    }
}

/// 队列本体。每帧重建实例即可：真正的队列存在 egui memory 里，
/// 这里只保存「本帧新投递」与渲染配置。
pub struct Toasts {
    id: Id,
    align: Align2,
    offset: Pos2,
    direction: Direction,
    order: Order,
    contents: std::sync::Arc<dyn Fn(&mut Ui, &Toast) + Send + Sync>,
    added: Vec<Toast>,
}

impl Toasts {
    /// `contents` 是单条 toast 的自绘函数（组件库注入 Neo 外观）。
    pub fn new(contents: impl Fn(&mut Ui, &Toast) + Send + Sync + 'static) -> Self {
        Self {
            id: Id::new("neo-toasts"),
            align: Align2::CENTER_BOTTOM,
            offset: Pos2::ZERO,
            direction: Direction::BottomUp,
            order: Order::Tooltip,
            contents: std::sync::Arc::new(contents),
            added: Vec::new(),
        }
    }

    /// 锚点 + 自锚点的偏移（如 `CENTER_BOTTOM` + `(0, -150)` = 底边中央再上移 150pt）。
    pub fn anchor(mut self, anchor: Align2, offset: impl Into<Pos2>) -> Self {
        self.align = anchor;
        self.offset = offset.into();
        self
    }

    /// 多条时的堆叠方向。
    pub fn direction(mut self, direction: impl Into<Direction>) -> Self {
        self.direction = direction.into();
        self
    }

    /// 投递一条。
    pub fn add(&mut self, toast: Toast) -> &mut Self {
        self.added.push(toast);
        self
    }

    /// 画出并维护整条队列：合并新投递、递减寿命、回收过期、按方向堆叠。
    ///
    /// toast 层**不可交互**：提示只是过眼信息，不该挡住它盖住的控件。
    pub fn show(&mut self, ui: &mut Ui) {
        let dt = ui.input(|i| i.unstable_dt) as f64;

        let mut queue: Vec<Toast> = ui.data_mut(|d| d.get_temp(self.id).unwrap_or_default());
        queue.extend(std::mem::take(&mut self.added));
        queue.retain(|t| t.options.ttl_sec > 0.0);

        let mut offset = self.offset;
        for (i, toast) in queue.iter_mut().enumerate() {
            let response = Area::new(self.id.with(i))
                .anchor(self.align, offset.to_vec2())
                .order(self.order)
                .interactable(false)
                .show(ui, |ui| (self.contents)(ui, toast))
                .response;

            toast.options.ttl_sec -= dt;
            if toast.options.ttl_sec.is_finite() {
                // 到期那一帧醒来一次，把尸体收走。
                ui.request_repaint_after(Duration::from_secs_f64(toast.options.ttl_sec.max(0.0)));
            }

            match self.direction {
                Direction::LeftToRight => offset.x += response.rect.width() + 10.0,
                Direction::RightToLeft => offset.x -= response.rect.width() + 10.0,
                Direction::TopDown => offset.y += response.rect.height() + 10.0,
                Direction::BottomUp => offset.y -= response.rect.height() + 10.0,
            }
        }

        ui.data_mut(|d| d.insert_temp(self.id, queue));
    }
}
