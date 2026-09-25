//! 超椭圆圆角 —— Harness 的 `corner-shape: superellipse(1.5)`。
//!
//! CSS 的 `corner-shape: superellipse(K)` 中，`K = 1` 是正圆弧、`K = 2` 是
//! squircle。两者都落在 `|x|^p + |y|^p = 1` 这一族曲线上，其中 `p = 2K`：
//!
//! | K   | p   | 形状       |
//! |-----|-----|-----------|
//! | 0.5 | 1   | 直切角     |
//! | 1   | 2   | 正圆弧 ✓   |
//! | 1.5 | 3   | **本项目** |
//! | 2   | 4   | squircle   |
//!
//! 参数化后 `x = r·|cosθ|^(2/p)`、`y = r·|sinθ|^(2/p)`，指数记为 `e = 2/p = 1/K`。
//!
//! egui 只提供正圆圆角，因此这里自行生成多边形路径交给 `PathShape` 填充，
//! 把上游那个「介于圆和 squircle 之间」的手感原样搬过来。

use egui::{Pos2, Rect};

/// Harness 实际使用的 `corner-shape` 参数。
pub const HARNESS_SUPERELLIPSE: f32 = 1.5;

/// 每个圆角的采样段数。12 段在 200px 级圆角下已与解析曲线无法分辨。
pub const DEFAULT_SEGMENTS: usize = 12;

/// 每个圆角的参数化指数 `e = 1/K`。
///
/// `e = 1` 即正圆（`K = 1`）；`e` 越小越接近直角。
pub fn parametric_power(k: f32) -> f32 {
    if k <= 0.0 {
        0.0
    } else {
        1.0 / k
    }
}

/// 一个圆角的弧参数。
#[derive(Clone, Copy)]
struct Arc {
    /// 该圆角的「圆心」（正圆时的弧心）。
    center: Pos2,
    /// 圆角半径。
    r: f32,
    /// x 方向的外扩符号（±1）。
    sx: f32,
    /// y 方向的外扩符号（±1）。
    sy: f32,
    /// 参数角区间，弧度。
    from: f32,
    to: f32,
    /// 参数化指数 `1/K`。
    power: f32,
    /// 采样段数。
    segments: usize,
}

impl Arc {
    /// 一个尚未指定方向的圆角弧。
    fn new(center: Pos2, r: f32, power: f32, segments: usize) -> Self {
        Self {
            center,
            r,
            sx: 1.0,
            sy: 1.0,
            from: 0.0,
            to: 0.0,
            power,
            segments,
        }
    }

    /// 指定外扩方向与参数角区间。
    fn sweep(mut self, sx: f32, sy: f32, from: f32, to: f32) -> Self {
        self.sx = sx;
        self.sy = sy;
        self.from = from;
        self.to = to;
        self
    }

    /// 采样成折线。
    fn sample(&self) -> Vec<Pos2> {
        let n = self.segments.max(1);
        (0..=n)
            .map(|i| {
                let t = self.from + (self.to - self.from) * (i as f32 / n as f32);
                let cx = t.cos().abs().powf(self.power);
                let cy = t.sin().abs().powf(self.power);
                Pos2::new(
                    self.center.x + self.sx * self.r * cx,
                    self.center.y + self.sy * self.r * cy,
                )
            })
            .collect()
    }
}

/// 把矩形转成一条超椭圆圆角多边形路径（顺时针）。
///
/// 半径会被钳制到短边的一半；`radius <= 0.5` 时退化为矩形四角。
pub fn squircle_points(rect: Rect, radius: f32, k: f32, segments: usize) -> Vec<Pos2> {
    let r = radius.max(0.0).min(rect.width().min(rect.height()) * 0.5);
    if r <= 0.5 {
        let (l, t, rr, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
        return vec![
            Pos2::new(l, t),
            Pos2::new(rr, t),
            Pos2::new(rr, b),
            Pos2::new(l, b),
        ];
    }

    let e = parametric_power(k);
    let (l, t, rr, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let half_pi = std::f32::consts::FRAC_PI_2;
    let pi = std::f32::consts::PI;

    let mut pts = Vec::with_capacity(segments * 4 + 4);
    // 上边 → 右上角 → 右边 → 右下角 → 下边 → 左下角 → 左边 → 左上角。
    let corner = |center: Pos2| Arc::new(center, r, e, segments);
    pts.push(Pos2::new(l + r, t));
    pts.extend(
        corner(Pos2::new(rr - r, t + r))
            .sweep(1.0, -1.0, half_pi, 0.0)
            .sample(),
    );
    pts.extend(
        corner(Pos2::new(rr - r, b - r))
            .sweep(1.0, 1.0, 0.0, half_pi)
            .sample(),
    );
    pts.push(Pos2::new(l + r, b));
    pts.extend(
        corner(Pos2::new(l + r, b - r))
            .sweep(-1.0, 1.0, half_pi, pi)
            .sample(),
    );
    pts.push(Pos2::new(l, t + r));
    pts.extend(
        corner(Pos2::new(l + r, t + r))
            .sweep(-1.0, -1.0, pi, pi + half_pi)
            .sample(),
    );
    pts
}
