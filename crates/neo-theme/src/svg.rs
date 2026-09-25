//! SVG 路径解析与扫描线光栅化。
//!
//! ## 为什么不用 egui 的填充
//!
//! `epaint` 的 `fill_closed_path` 是**三角扇**，只对外凸多边形正确。而上游
//! （DeepSeek Harness）的图标恰恰不是"一笔轮廓"：它是**双轮廓挖空**的填充图形 ——
//! 描边效果靠两条反向绕行的轮廓实现（外轮廓 + 内轮廓，中间那一圈才会被填上）。
//! 所以必须自己做带洞填充。
//!
//! 这里用**扫描线 + 填充规则**：逐子采样行求交点、排序、按 nonzero / even-odd
//! 累积绕数，再把命中区间按像素覆盖率累加。天然支持洞、自相交与任意绕行方向，
//! 不需要三角化，也不挑多边形形状。
//!
//! ## 覆盖的语法
//!
//! `M/L/H/V/C/Q/S/T/Z`，绝对与相对（小写）都支持，数字支持科学计数法
//! （上游的 `IconFolderClose16` 真的写了 `1.23e-05`），也支持隐式重复
//! （`C` 后面省略后续 `C`）。`A`（圆弧）不支持：上游 67 个图标都没用到，
//! 与其写一个半吊子的椭圆弧，不如让它在解析时直连落点（不会崩，只是不准）。

use std::f32::consts::PI;

/// 填充规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    /// 非零环绕（SVG 默认）。反向绕行的内轮廓挖出洞。
    NonZero,
    /// 奇偶。子路径重叠处交替填充。
    EvenOdd,
}

/// 把 `d` 解析成若干条**闭合**折线（曲线已展平）。
///
/// 返回的每条折线点数 ≥ 2；`Z` 会隐式闭合（不重复首点，光栅化按闭合处理）。
pub fn parse_subpaths(d: &str) -> Vec<Vec<[f32; 2]>> {
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let mut pos = [0.0f32, 0.0f32];
    let mut start = [0.0f32, 0.0f32];
    // 上一条三次/二次曲线的控制点，供 S/T 的反射使用。
    let mut last_cubic: Option<[f32; 2]> = None;
    let mut last_quad: Option<[f32; 2]> = None;

    let mut lex = Lexer::new(d);
    let mut last_cmd: Option<char> = None;

    while let Some(cmd) = lex.next_command(last_cmd) {
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        let shift = |p: [f32; 2]| -> [f32; 2] {
            if rel {
                [pos[0] + p[0], pos[1] + p[1]]
            } else {
                p
            }
        };

        match up {
            'M' => {
                let Some(p) = lex.point() else { break };
                let p = shift(p);
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
                pos = p;
                start = p;
                cur.push(p);
                // 后续隐式坐标按 L 处理（SVG 规定）。
                last_cmd = Some(if rel { 'l' } else { 'L' });
            }
            'L' => {
                let Some(p) = lex.point() else { break };
                pos = shift(p);
                cur.push(pos);
                last_cmd = Some(cmd);
            }
            'H' => {
                let Some(x) = lex.number() else { break };
                pos = [if rel { pos[0] + x } else { x }, pos[1]];
                cur.push(pos);
                last_cmd = Some(cmd);
            }
            'V' => {
                let Some(y) = lex.number() else { break };
                pos = [pos[0], if rel { pos[1] + y } else { y }];
                cur.push(pos);
                last_cmd = Some(cmd);
            }
            'C' | 'S' => {
                let (c1, c2, p) = if up == 'C' {
                    let (Some(a), Some(b), Some(c)) = (lex.point(), lex.point(), lex.point())
                    else {
                        break;
                    };
                    (shift(a), shift(b), shift(c))
                } else {
                    // S：首控制点是上一控制点关于当前点的反射。
                    let (Some(b), Some(c)) = (lex.point(), lex.point()) else {
                        break;
                    };
                    let r = last_cubic
                        .map(|c| [2.0 * pos[0] - c[0], 2.0 * pos[1] - c[1]])
                        .unwrap_or(pos);
                    (r, shift(b), shift(c))
                };
                flatten_cubic(&mut cur, pos, c1, c2, p);
                last_cubic = Some(c2);
                pos = p;
                last_cmd = Some(cmd);
            }
            'Q' | 'T' => {
                let (c, p) = if up == 'Q' {
                    let (Some(a), Some(b)) = (lex.point(), lex.point()) else {
                        break;
                    };
                    (shift(a), shift(b))
                } else {
                    let Some(b) = lex.point() else { break };
                    let r = last_quad
                        .map(|c| [2.0 * pos[0] - c[0], 2.0 * pos[1] - c[1]])
                        .unwrap_or(pos);
                    (r, shift(b))
                };
                // 二次贝塞尔升阶成三次，复用同一条展平路径。
                let c1 = [
                    pos[0] + 2.0 / 3.0 * (c[0] - pos[0]),
                    pos[1] + 2.0 / 3.0 * (c[1] - pos[1]),
                ];
                let c2 = [
                    p[0] + 2.0 / 3.0 * (c[0] - p[0]),
                    p[1] + 2.0 / 3.0 * (c[1] - p[1]),
                ];
                flatten_cubic(&mut cur, pos, c1, c2, p);
                last_quad = Some(c);
                pos = p;
                last_cmd = Some(cmd);
            }
            'A' => {
                // 不支持圆弧：直连落点（保证不崩、不产生畸形轮廓）。
                let Some(p) = lex.arc_end() else { break };
                pos = shift(p);
                cur.push(pos);
                last_cmd = Some(cmd);
            }
            'Z' => {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
                pos = start;
                last_cubic = None;
                last_quad = None;
                last_cmd = None;
                continue;
            }
            _ => break,
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// 三次贝塞尔展平（自适应细分）。
///
/// 固定段数在大曲率处会露折线、在小曲率处又浪费顶点；按"控制点离弦的垂距"
/// 递归细分，代价小且看不出多边形。
fn flatten_cubic(out: &mut Vec<[f32; 2]>, p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2]) {
    /// 允许的最大偏差（viewBox 单位）。16 栅格下约 0.6% —— 128px 纹理上看不见。
    const TOL: f32 = 0.01;
    const MAX_DEPTH: u8 = 10;

    fn flat(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], tol: f32) -> bool {
        // 控制点到弦的距离近似：取两点到弦的垂距之和。
        let dx = p3[0] - p0[0];
        let dy = p3[1] - p0[1];
        let d1 = ((p1[0] - p3[0]) * dy - (p1[1] - p3[1]) * dx).abs();
        let d2 = ((p2[0] - p3[0]) * dy - (p2[1] - p3[1]) * dx).abs();
        (d1 + d2) * (d1 + d2) <= tol * tol * (dx * dx + dy * dy)
    }

    #[allow(clippy::too_many_arguments)]
    fn rec(
        out: &mut Vec<[f32; 2]>,
        p0: [f32; 2],
        p1: [f32; 2],
        p2: [f32; 2],
        p3: [f32; 2],
        depth: u8,
        tol: f32,
    ) {
        if depth >= MAX_DEPTH || flat(p0, p1, p2, p3, tol) {
            out.push(p3);
            return;
        }
        let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let p01 = mid(p0, p1);
        let p12 = mid(p1, p2);
        let p23 = mid(p2, p3);
        let p012 = mid(p01, p12);
        let p123 = mid(p12, p23);
        let p0123 = mid(p012, p123);
        rec(out, p0, p01, p012, p0123, depth + 1, tol);
        rec(out, p0123, p123, p23, p3, depth + 1, tol);
    }

    rec(out, p0, p1, p2, p3, 0, TOL);
}

/// 扫描线光栅化：返回每个像素的覆盖率（0..1，行主序，`w × h`）。
///
/// `view` 是路径所在的 viewBox 尺寸，用来把路径坐标映射到纹理像素。
pub fn rasterize(
    subpaths: &[Vec<[f32; 2]>],
    view: (f32, f32),
    w: usize,
    h: usize,
    supersample: usize,
    rule: FillRule,
) -> Vec<f32> {
    let mut coverage = vec![0.0f32; w * h];
    if w == 0 || h == 0 || subpaths.is_empty() {
        return coverage;
    }

    // 闭合边表（跳过水平边：它们不产生交点）。
    let mut edges: Vec<([f32; 2], [f32; 2])> = Vec::new();
    for sp in subpaths {
        if sp.len() < 2 {
            continue;
        }
        for i in 0..sp.len() {
            let a = sp[i];
            let b = sp[(i + 1) % sp.len()];
            if (a[1] - b[1]).abs() > f32::EPSILON {
                edges.push((a, b));
            }
        }
    }
    if edges.is_empty() {
        return coverage;
    }

    let ss = supersample.max(1);
    let rows = h * ss;
    let inv_ss = 1.0 / ss as f32;
    let scale_x = w as f32 / view.0;
    let mut hits: Vec<(f32, i32)> = Vec::with_capacity(edges.len());

    for row in 0..rows {
        let y = (row as f32 + 0.5) / rows as f32 * view.1;
        hits.clear();
        for &(a, b) in &edges {
            // 半开区间 [min, max) 判定，顶点不会被数两次。
            let up = a[1] <= y && b[1] > y;
            let down = b[1] <= y && a[1] > y;
            if !up && !down {
                continue;
            }
            let t = (y - a[1]) / (b[1] - a[1]);
            hits.push((a[0] + t * (b[0] - a[0]), if up { 1 } else { -1 }));
        }
        if hits.len() < 2 {
            continue;
        }
        hits.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap_or(std::cmp::Ordering::Equal));

        let row_base = (row / ss) * w;
        let mut winding = 0i32;
        // 命中的区间要**合并成段**：相邻两段都"在内部"时必须连成一段再填，
        // 否则重叠区（winding = ±2）会因为不是"由外向内"的跃变而被漏掉。
        let mut span_start: Option<usize> = None;
        let fill = |from: f32, to: f32, cov: &mut [f32], row_base: usize| {
            let x0 = (from * scale_x).max(0.0);
            let x1 = (to * scale_x).min(w as f32);
            if x1 <= x0 {
                return;
            }
            let px0 = x0.floor() as usize;
            let px1 = (x1.ceil() as usize).min(w);
            for px in px0..px1 {
                let left = (px as f32).max(x0);
                let right = ((px + 1) as f32).min(x1);
                let frac = right - left;
                if frac > 0.0 {
                    cov[row_base + px] += frac * inv_ss;
                }
            }
        };

        for i in 0..hits.len() {
            let (x, dirn) = hits[i];
            winding += dirn;
            let inside = match rule {
                FillRule::NonZero => winding != 0,
                FillRule::EvenOdd => i % 2 == 0,
            };
            if inside {
                span_start.get_or_insert(i);
            } else if let Some(s) = span_start.take() {
                fill(hits[s].0, x, &mut coverage, row_base);
            }
        }
        // 收尾：正常的闭合路径不会以"仍在内部"结束，防御性地补一段。
        if let Some(s) = span_start {
            fill(hits[s].0, hits[hits.len() - 1].0, &mut coverage, row_base);
        }
    }

    for c in &mut coverage {
        *c = c.clamp(0.0, 1.0);
    }
    coverage
}

/// 路径边界框（`[x0, y0, x1, y1]`）。空路径返回 `None`。
pub fn bounds(subpaths: &[Vec<[f32; 2]>]) -> Option<[f32; 4]> {
    let mut b = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    let mut any = false;
    for sp in subpaths {
        for p in sp {
            b[0] = b[0].min(p[0]);
            b[1] = b[1].min(p[1]);
            b[2] = b[2].max(p[0]);
            b[3] = b[3].max(p[1]);
            any = true;
        }
    }
    any.then_some(b)
}

// ---------------------------------------------------------------------------
// 词法器
// ---------------------------------------------------------------------------

struct Lexer<'a> {
    bytes: &'a [u8],
    pos: usize,
    pending: Option<char>,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
            pending: None,
        }
    }

    fn skip_separators(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b',' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    /// 取下一个命令；流里没有字母时按 SVG 规则复用 `implicit`。
    fn next_command(&mut self, implicit: Option<char>) -> Option<char> {
        if let Some(c) = self.pending.take() {
            return Some(c);
        }
        self.skip_separators();
        if self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_alphabetic() {
            let c = self.bytes[self.pos] as char;
            self.pos += 1;
            return Some(c);
        }
        // 没有字母：只有后面还跟着数字时才允许隐式重复。
        let save = self.pos;
        let has_number = self.number_start();
        self.pos = save;
        has_number.then_some(implicit).flatten()
    }

    fn number_start(&mut self) -> bool {
        self.skip_separators();
        self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b'0'..=b'9' | b'-' | b'+' | b'.')
    }

    /// 读一个数：支持符号、小数与**科学计数法**（上游真有 `1.23e-05`）。
    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let start = self.pos;
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'-' | b'+') {
            self.pos += 1;
        }
        let mut seen_dot = false;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'0'..=b'9' => self.pos += 1,
                b'.' if !seen_dot => {
                    seen_dot = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        // 指数部分：e/E 后必须跟数字（或带符号的数字）。
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'e' | b'E') {
            let save = self.pos;
            self.pos += 1;
            if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'-' | b'+') {
                self.pos += 1;
            }
            if self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            } else {
                self.pos = save; // 不是指数，回退
            }
        }
        if self.pos == start {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()?
            .parse::<f32>()
            .ok()
    }

    fn point(&mut self) -> Option<[f32; 2]> {
        let x = self.number()?;
        let y = self.number()?;
        Some([x, y])
    }

    /// 圆弧的落点：跳过 rx ry rotation flag flag 五个参数，只取终点。
    fn arc_end(&mut self) -> Option<[f32; 2]> {
        for _ in 0..4 {
            self.number()?;
        }
        self.point()
    }
}

/// 弧度转它（保留给未来的圆弧支持，避免删了再写）。
#[allow(dead_code)]
const TAU: f32 = 2.0 * PI;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_square() {
        let sp = parse_subpaths("M0 0L10 0L10 10L0 10Z");
        assert_eq!(sp.len(), 1);
        assert_eq!(sp[0].len(), 4);
        assert_eq!(sp[0][0], [0.0, 0.0]);
        assert_eq!(sp[0][2], [10.0, 10.0]);
    }

    #[test]
    fn supports_relative_and_implicit_repeat() {
        // 隐含重复：M 之后的两组坐标按 L 处理。
        let sp = parse_subpaths("M0 0 5 0 5 5Z");
        assert_eq!(sp[0].len(), 3);
        assert_eq!(sp[0][2], [5.0, 5.0]);
        // 相对命令
        let sp = parse_subpaths("m1 1l2 0l0 2z");
        assert_eq!(sp[0][0], [1.0, 1.0]);
        assert_eq!(sp[0][2], [3.0, 3.0]);
    }

    #[test]
    fn supports_h_v_and_scientific_notation() {
        let sp = parse_subpaths("M1 1H9V9H1Z");
        assert_eq!(sp[0][1], [9.0, 1.0]);
        assert_eq!(sp[0][2], [9.0, 9.0]);

        // 上游 IconFolderClose16 里的写法
        let sp = parse_subpaths("M1.2e-05 3L4 3Z");
        assert_eq!(sp[0][0][0], 1.2e-05);
    }

    #[test]
    fn supports_quadratic_via_elevation() {
        let sp = parse_subpaths("M0 0Q5 10 10 0Z");
        // 曲线被展平：点数明显多于 3，且中段向上凸（y 大于 0）
        assert!(sp[0].len() > 5, "二次曲线未展平");
        let max_y = sp[0].iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        assert!(max_y > 4.0, "曲线没有拱起来: {max_y}");
    }

    #[test]
    fn multiple_subpaths_are_split() {
        let sp = parse_subpaths("M0 0L4 0L4 4ZM6 6L8 6L8 8Z");
        assert_eq!(sp.len(), 2);
    }

    #[test]
    fn nonzero_cuts_a_hole() {
        // 外框顺时针、内框逆时针：nonzero 下中间必须空。
        let outer = "M0 0L16 0L16 16L0 16Z";
        let inner = "M4 4L4 12L12 12L12 4Z"; // 反向绕行
        let sp = parse_subpaths(&format!("{outer}{inner}"));
        let cov = rasterize(&sp, (16.0, 16.0), 32, 32, 2, FillRule::NonZero);
        // 像素 (2,2) → viewBox ≈(1,1)：在外框上、洞外
        assert!(cov[2 * 32 + 2] > 0.9, "外框没被填");
        // 像素 (16,16) → viewBox ≈(8.25,8.25)：洞中心
        assert!(cov[16 * 32 + 16] < 0.1, "内框没有成为洞");
    }

    #[test]
    fn even_odd_differs_from_nonzero_on_overlap() {
        // 同向绕行的两个重叠矩形：nonzero 全填，even-odd 中间留洞。
        let a = "M0 0L10 0L10 10L0 10Z";
        let b = "M5 5L15 5L15 15L5 15Z";
        let sp = parse_subpaths(&format!("{a}{b}"));
        let nz = rasterize(&sp, (16.0, 16.0), 32, 32, 2, FillRule::NonZero);
        let eo = rasterize(&sp, (16.0, 16.0), 32, 32, 2, FillRule::EvenOdd);
        // 重叠区（约 (7.5, 7.5) → 像素 15,15）nonzero 实心、even-odd 空
        assert!(nz[15 * 32 + 15] > 0.9, "nonzero 重叠区应为实心");
        assert!(eo[15 * 32 + 15] < 0.1, "even-odd 重叠区应为空");
    }

    #[test]
    fn degenerate_input_does_not_panic() {
        for d in ["", "M", "M0", "L3 3", "M0 0A1 1 0 0 1 2 2", "M0 0Q1"] {
            let _ = parse_subpaths(d);
        }
    }

    #[test]
    fn bounds_are_computed() {
        let sp = parse_subpaths("M1 2L5 2L5 9L1 9Z");
        assert_eq!(bounds(&sp), Some([1.0, 2.0, 5.0, 9.0]));
        assert_eq!(bounds(&[]), None);
    }
}
