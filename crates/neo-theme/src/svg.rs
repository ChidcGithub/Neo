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
        // SVG 2 §9.3.6–7：只有紧邻的同类曲线可供 S/T 反射。
        if !matches!(up, 'C' | 'S') {
            last_cubic = None;
        }
        if !matches!(up, 'Q' | 'T') {
            last_quad = None;
        }
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
        for _ in 0..3 {
            self.number()?;
        }
        // flag 是单个 0/1，语法允许两个 flag 以及后续坐标之间不写分隔符。
        for _ in 0..2 {
            self.skip_separators();
            match self.bytes.get(self.pos) {
                Some(b'0' | b'1') => self.pos += 1,
                _ => return None,
            }
        }
        self.point()
    }
}

/// 弧度转它（保留给未来的圆弧支持，避免删了再写）。
#[allow(dead_code)]
const TAU: f32 = 2.0 * PI;

#[cfg(test)]
#[path = "../../neo-app/src/brand/whale_path.rs"]
mod whale_fixture;

#[cfg(test)]
#[path = "svg_tests.rs"]
mod tests;
