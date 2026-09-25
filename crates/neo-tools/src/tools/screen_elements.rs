//! `screen_elements` —— 屏幕上所有可交互元素的编号清单（SoM）。
//!
//! 这是"感知"与"行动"之间的桥：`screenshot` 给模型看图，`click` 让模型动手，
//! 而这个工具把"屏幕上有什么可点"变成**结构化的编号列表** —— 模型回答
//! 「点击 7」，不用再从图里手算像素坐标。
//!
//! 算法与验证数据见 `docs/screen-elements-research.md`。要点：
//!
//! - 枚举走 **UIA 辅助功能树**（系统免费提供，本机实测 144 元素 / ~540ms /
//!   96% 带名 / 物理像素坐标）；
//! - 编号按**上 → 下、左 → 右**（阅读顺序），模型看截图能对上空间关系；
//! - `click` 的 `element_id` 参数按编号查这里的缓存（5 分钟内有效）。

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::base64_lite::encode as b64;
use super::{screen, screen_uia};

pub static PARAMS: &[Param] = &[
    Param::opt_text(
        "window",
        "只枚举标题**包含**该串的顶层窗口（大小写不敏感）。给了该参数会覆盖默认的焦点窗口范围。\
         元素太多时用它缩小范围。",
    ),
    Param::flag(
        "annotate",
        "true = 同时返回一张画好编号框的截图（作为图片随结果交给模型）。\
         默认 false，只返回元素清单。",
    ),
    Param::flag(
        "all",
        "true = 枚举所有窗口；默认只枚举当前焦点窗口。每次最多返回 50 个元素；\
         元素超过 50 个时，使用相同参数重复调用会轮换到下一页。",
    ),
];

pub fn preview(args: &Args) -> String {
    match args.opt_str("window").ok().as_deref() {
        Some(w) if !w.trim().is_empty() => format!("枚举「{}」的可交互元素并编号", w.trim()),
        _ if args.flag("all").unwrap_or(false) => "枚举所有窗口的可交互元素并编号".to_owned(),
        _ => "枚举当前焦点窗口的可交互元素并编号".to_owned(),
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("screen_elements", e),
    }
}

/// 元素列表的字符预算。`result.rs` 的 `MODEL_JSON_CHARS`（24K）是整个工具结果
/// 的外壳预算，这里留出外壳与 `data` 其它字段的量。
const DATA_CHARS_BUDGET: usize = 20_000;

/// 每次最多给模型 50 个元素，避免可访问树淹没上下文。
const PAGE_SIZE: usize = 50;

#[derive(Default)]
struct PageCursor {
    key: String,
    next_page: usize,
}

static PAGE_CURSOR: Mutex<PageCursor> = Mutex::new(PageCursor {
    key: String::new(),
    next_page: 0,
});

fn take_page(key: &str, total: usize) -> (usize, usize, usize) {
    let pages = total.max(1).div_ceil(PAGE_SIZE);
    let mut cursor = PAGE_CURSOR.lock().unwrap();
    if cursor.key != key {
        cursor.key = key.to_owned();
        cursor.next_page = 0;
    }
    let page = cursor.next_page % pages;
    cursor.next_page = (page + 1) % pages;
    let start = page * PAGE_SIZE;
    (page, pages, start)
}

fn intent_hint(role: &str, name: &str) -> String {
    let action = match role {
        "Button" | "SplitButton" => "执行",
        "Hyperlink" => "打开链接或跳转到",
        "MenuItem" => "选择菜单项",
        "TabItem" => "切换到",
        "ListItem" | "TreeItem" | "DataItem" => "选择",
        "CheckBox" => "切换选项",
        "RadioButton" => "选择选项",
        "ComboBox" => "展开并选择",
        "Edit" => "输入或编辑",
        "Slider" | "Spinner" => "调整",
        "Calendar" => "选择日期",
        _ => "操作",
    };
    format!(
        "可能用于{action}“{name}”；这是根据控件类型和可访问名称生成的提示，需结合窗口与截图确认"
    )
}

fn act(_scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let window = args.opt_str("window")?;
    let annotate = args.flag("annotate")?;
    let all = args.flag("all")?;
    let filter = Some(window.trim()).filter(|s| !s.is_empty());
    let all_windows = all || filter.is_some();

    let els = screen_uia::enumerate_mode(filter, all_windows, 600, 800)?;
    if els.is_empty() {
        return Err(
            ToolError::not_found("屏幕上没有枚举到可交互元素").with_hint(match filter {
                Some(f) => {
                    format!("窗口过滤「{f}」可能太严：去掉 window 参数枚举全屏，或核对窗口标题")
                }
                None => {
                    "确认屏幕上有可见的应用窗口；刚切换窗口时 UIA 树在重建，等一秒重试".to_owned()
                }
            }),
        );
    }
    // 存进缓存：接下来模型大概率会按编号 click。
    screen_uia::cache_store(&els);

    // 相同范围的重复调用自动轮换页面；范围改变则从第一页重新开始。
    let key = format!(
        "{}|{}",
        if all_windows { "all" } else { "focus" },
        filter.unwrap_or("")
    );
    let (page, pages, start) = take_page(&key, els.len());
    let page_els = &els[start..els.len().min(start + PAGE_SIZE)];

    let mut items: Vec<Value> = Vec::with_capacity(page_els.len());
    let mut used = 0usize;
    let mut truncated = false;
    for e in page_els {
        let item = json!({
            "id": e.id, "role": e.role, "name": e.name, "window": e.window,
            "intent_hint": intent_hint(e.role, &e.name),
            "x": e.rect[0], "y": e.rect[1], "w": e.rect[2], "h": e.rect[3],
        });
        let len = item.to_string().len() + 2;
        if used + len > DATA_CHARS_BUDGET && !items.is_empty() {
            truncated = true;
            break;
        }
        used += len;
        items.push(item);
    }

    let vs = screen::virtual_screen();
    let data = json!({
        "count": els.len(),
        "shown": items.len(),
        "page": page + 1,
        "pages": pages,
        "has_more": pages > 1,
        "truncated": truncated,
        "scope": if all_windows { "all_windows" } else { "focused_window" },
        "coordinate_space": "虚拟桌面物理像素（与 screenshot / click / drag 同一坐标系）",
        "virtual_screen": {
            "x": vs.x, "y": vs.y, "width": vs.width, "height": vs.height
        },
        "elements": items,
        "next": if pages > 1 {
            "当前结果已分页；用相同参数再次调用会轮换下一批元素，直到覆盖所有按钮。"
        } else {
            "用 click 给 element_id 点选编号（推荐，自动点元素中心）；也可以给 x/y 直接点坐标"
        },
    });

    let mut outcome = Outcome::ok(
        "screen_elements",
        format!("屏幕上 {} 个可交互元素（已按上→下、左→右编号）", els.len()),
        data,
    );

    if annotate {
        // 标注图超限就不附（32 MiB 是官方单图上限），清单本身仍然完整可用。
        let shot = screen::capture(vs)?;
        let marked = annotate_shot(&shot, page_els, (vs.x, vs.y))?;
        let png = marked.to_png()?;
        if png.len() <= super::limits::INLINE_IMAGE_BYTES {
            outcome = outcome.with_image(format!("data:image/png;base64,{}", b64(&png)));
        }
    }
    Ok(outcome)
}

// ==== SoM 标注图 ==========================================================

/// 在截图上画编号框：高对比红框 + 黑底白字的编号标签（5×7 点阵数字放大 2×）。
///
/// 为什么不用真正的字体渲染：工具层没有字体基建，而 5×7 点阵在 2× 缩放下
/// 对视觉模型完全可读 —— 简单、零依赖、可测。
fn annotate_shot(
    shot: &screen::Shot,
    els: &[screen_uia::ScreenElement],
    shot_origin: (i32, i32),
) -> Result<screen::Shot, ToolError> {
    let mut img = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone())
        .ok_or_else(|| ToolError::io("截图位图尺寸与宽高不一致"))?;

    const BOX: [u8; 3] = [255, 72, 72]; // 高对比红：深浅主题下都醒目
    for e in els {
        draw_rect(
            &mut img,
            [
                e.rect[0] - shot_origin.0,
                e.rect[1] - shot_origin.1,
                e.rect[2],
                e.rect[3],
            ],
            BOX,
            2,
        );
        draw_label(
            &mut img,
            e.id,
            e.rect[0] - shot_origin.0,
            e.rect[1] - shot_origin.1,
            shot.width,
            shot.height,
        );
    }
    Ok(screen::Shot {
        width: shot.width,
        height: shot.height,
        rgba: img.into_raw(),
    })
}

/// 画 2px 边框矩形（超出画布的部分自动裁掉）。
fn draw_rect(img: &mut image::RgbaImage, rect: [i32; 4], color: [u8; 3], thick: u32) {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let [x, y, rw, rh] = rect.map(i64::from);
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + rw).min(w - 1), (y + rh).min(h - 1));
    if x1 < x0 || y1 < y0 {
        return;
    }
    let (x0, y0, x1, y1) = (x0 as u32, y0 as u32, x1 as u32, y1 as u32);
    let px = |img: &mut image::RgbaImage, px: u32, py: u32| {
        if px < img.width() && py < img.height() {
            img.get_pixel_mut(px, py).0 = [color[0], color[1], color[2], 255];
        }
    };
    for t in 0..thick {
        for xx in x0..=x1 {
            px(img, xx, y0 + t);
            px(img, xx, y1.saturating_sub(t));
        }
        for yy in y0..=y1 {
            px(img, x0 + t, yy);
            px(img, x1.saturating_sub(t), yy);
        }
    }
}

/// 5×7 点阵数字（MSB 在左）。只有 0-9 —— 编号就是数字。
const DIGITS: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ], // 0
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ], // 1
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ], // 2
    [
        0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
    ], // 3
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ], // 4
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ], // 5
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ], // 6
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ], // 7
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ], // 8
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ], // 9
];

/// 在框的左上角画「编号标签」：黑底白字（放不进屏幕就贴着框内画）。
fn draw_label(img: &mut image::RgbaImage, id: usize, x: i32, y: i32, w: u32, h: u32) {
    let text = id.to_string();
    let scale = 2u32; // 每点阵点 2×2 像素
    let gw = 5 * scale as i32;
    let gh = 7 * scale as i32;
    let gap = scale as i32; // 字符间距
    let text_w = text.len() as i32 * (gw + gap) - gap;
    let pad = scale as i32 + 1;

    // 标签默认贴在框上方；y 太靠顶就放进框里。
    let mut ly = y - gh - pad * 2;
    if ly < 0 {
        ly = y + pad;
    }
    let mut lx = x.max(0);
    // 右侧放不下就往左挪（仍从元素左缘起画，保持"标签属于哪个框"的归属感）
    if lx + text_w + pad * 2 > w as i32 {
        lx = (w as i32) - text_w - pad * 2;
    }
    if lx < 0 {
        return; // 整个标签都放不下（极端小屏），框本身仍在
    }

    let bg = [0, 0, 0, 230];
    for yy in (ly - pad)..(ly + gh + pad) {
        for xx in (lx - pad)..(lx + text_w + pad) {
            if xx >= 0 && yy >= 0 && (xx as u32) < w && (yy as u32) < h {
                img.get_pixel_mut(xx as u32, yy as u32).0 = bg;
            }
        }
    }
    let fg = [255, 255, 255, 255];
    for (ci, ch) in text.chars().enumerate() {
        let digit = match ch.to_digit(10) {
            Some(d) => d as usize,
            None => continue,
        };
        let cx = lx + ci as i32 * (gw + gap);
        for (ry, row) in DIGITS[digit].iter().enumerate() {
            for rx in 0..5 {
                if row & (0b10000 >> rx) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = cx + rx * scale as i32 + dx as i32;
                            let py = ly + ry as i32 * scale as i32 + dy as i32;
                            if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                                img.get_pixel_mut(px as u32, py as u32).0 = fg;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba([20, 20, 22, 255]))
    }

    /// 点阵字形要可辨 —— 至少笔画量在一个合理区间（空字形/涂满都是 bug）。
    #[test]
    fn digit_bitmaps_are_plausible() {
        for (d, rows) in DIGITS.iter().enumerate() {
            let ink: usize = rows.iter().map(|r| r.count_ones() as usize).sum();
            assert!(
                (8..=25).contains(&ink),
                "数字 {d} 的笔画量 {ink} 不像 5×7 数字"
            );
        }
        // 手工核对两个签名特征：1 的顶旗、8 的双环
        assert_eq!(DIGITS[1][0], 0b00100);
        assert_eq!(DIGITS[8][3], 0b01110);
    }

    #[test]
    fn intent_hint_is_explicitly_heuristic() {
        let hint = intent_hint("Button", "保存");
        assert!(hint.contains("执行"));
        assert!(hint.contains("需结合窗口与截图确认"));
    }

    #[test]
    fn pages_rotate_with_same_scope() {
        let key = "test-pages-rotation";
        let first = take_page(key, 121);
        let second = take_page(key, 121);
        let third = take_page(key, 121);
        let fourth = take_page(key, 121);
        assert_eq!(first, (0, 3, 0));
        assert_eq!(second, (1, 3, 50));
        assert_eq!(third, (2, 3, 100));
        assert_eq!(fourth, (0, 3, 0));
    }

    #[test]
    fn page_scope_change_resets_to_first_page() {
        let first_key = "test-pages-a";
        let second_key = "test-pages-b";
        let _ = take_page(first_key, 80);
        let _ = take_page(first_key, 80);
        assert_eq!(take_page(second_key, 80), (0, 2, 0));
    }
    /// 标签画上去之后：黑底存在、白点存在，且都落在画布内（不 panic）。
    #[test]
    fn label_is_drawn_inside_canvas() {
        let mut im = img(1920, 1080);
        draw_label(&mut im, 7, 900, 500, 1920, 1080);
        let mut black = 0;
        let mut white = 0;
        for p in im.pixels() {
            if p.0 == [0, 0, 0, 230] {
                black += 1;
            }
            if p.0 == [255, 255, 255, 255] {
                white += 1;
            }
        }
        assert!(black > 100, "标签底色没画上：{black}");
        assert!(white > 10, "数字笔画没画上：{white}");
    }

    /// 编号标签贴屏幕顶时放进框内（y<0 的标签会整个丢掉）。
    #[test]
    fn label_clamps_below_top_edge() {
        let mut im = img(400, 300);
        draw_label(&mut im, 3, 10, 0, 400, 300); // y=0，标签上方放不下
        let white: usize = im.pixels().filter(|p| p.0 == [255, 255, 255, 255]).count();
        assert!(white > 0, "顶边元素的编号标签丢了");
    }

    /// 框画在部分出界的元素上不应 panic，且画布内的部分有墨。
    #[test]
    fn rect_clips_at_canvas_edges() {
        let mut im = img(200, 200);
        draw_rect(&mut im, [-10, -10, 60, 60], [255, 0, 0], 2);
        let red: usize = im.pixels().filter(|p| p.0[0] == 255 && p.0[1] == 0).count();
        assert!(red > 0, "出界框在画布内的部分应该有墨");
        draw_rect(&mut im, [500, 500, 60, 60], [255, 0, 0], 2); // 完全出界：no-op
    }

    /// UIA 使用虚拟桌面绝对坐标，截图位图使用以左上角为零的局部坐标。
    /// 左侧副屏会让虚拟桌面原点为负；标注前必须减掉该原点。
    #[test]
    fn annotation_translates_negative_virtual_desktop_origin() {
        let shot = screen::Shot {
            width: 200,
            height: 100,
            rgba: img(200, 100).into_raw(),
        };
        let els = vec![screen_uia::ScreenElement {
            id: 1,
            role: "Button",
            name: "副屏按钮".into(),
            window: "测试".into(),
            rect: [-1910, 30, 40, 20],
        }];
        let marked = annotate_shot(&shot, &els, (-1920, 0)).expect("标注");
        let pixel = image::RgbaImage::from_raw(marked.width, marked.height, marked.rgba)
            .unwrap()
            .get_pixel(10, 30)
            .0;
        assert_eq!(pixel, [255, 72, 72, 255], "框应落在截图局部坐标 (10,30)");
    }
}
