//! `click` —— 在屏幕上的某个位置点击鼠标（支持左/右键、双击）。
//!
//! 这是"AI 能动手"的那只手。三点约定：
//!
//! 1. **两种定位方式，`element_id` 优先**：先跑 `screen_elements` 拿到编号清单，
//!    然后给 `element_id` 让它自动点元素中心 —— 比手算坐标准得多；
//!    UIA 照不到的东西（游戏、自绘界面）才退回 `x/y` 直连。
//! 2. **坐标是虚拟桌面的物理像素**，与 `screenshot` 同一套 ——
//!    照截图里看到的像素位置写就行，不用换算缩放（见 [`super::screen`]）。
//! 3. **双击是一个动作**，不是两次 `click`：两次调用之间隔着一次网络往返，
//!    早就超过系统的双击间隔了。要双击就把 `double` 设为 true。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::screen::{self, Button};

pub static PARAMS: &[Param] = &[
    Param::opt_int(
        "element_id",
        "`screen_elements` 给出的元素编号 —— 点击该元素的**中心**。\
         给了它就不用给 x/y（两者都给时以它为准）。",
        0,
        0,
        9999,
    ),
    Param::opt_int(
        "x",
        "点击位置的 x（虚拟桌面物理像素，多显示器时可能为负）。照 screenshot 里的像素位置写。",
        0,
        -32768,
        32768,
    ),
    Param::opt_int(
        "y",
        "点击位置的 y（虚拟桌面物理像素）。同上，照 screenshot 里的像素位置写。",
        0,
        -32768,
        32768,
    ),
    Param::opt_text(
        "button",
        "`left`（默认，左键）或 `right`（右键，常用于呼出上下文菜单）。",
    ),
    Param::flag(
        "double",
        "true = 双击（一次调用内完成，符合系统的双击判定）；false = 单击。\
         需要双击时**必须**用它，不要连着调两次 click —— 两次调用之间隔着网络往返，\
         早就超过系统的双击间隔了。",
    ),
];

pub fn preview(args: &Args) -> String {
    let which = match args.opt_str("button").unwrap_or_default().as_str() {
        "right" | "r" | "secondary" => "右键",
        _ => "左键",
    };
    let times = if args.flag("double").unwrap_or(false) {
        "双击"
    } else {
        "点击"
    };
    let id = args.opt_int("element_id").unwrap_or(0);
    if id > 0 {
        return format!("{times}{which}：元素 #{id}");
    }
    let x = args.opt_int("x").unwrap_or(0);
    let y = args.opt_int("y").unwrap_or(0);
    format!("在 ({x}, {y}) {times}{which}")
}

pub fn run(_scope: &Scope, args: &Args) -> Outcome {
    match act(args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("click", e),
    }
}

fn act(args: &Args) -> Result<Outcome, ToolError> {
    let button = Button::parse(&args.opt_str("button")?)?;
    let double = args.flag("double")?;

    // 两种定位方式必须给一种 —— 缺了就明说，不能被悄悄当成 (0,0) 点下去。
    // （`opt_int` 带默认值，"给没给"只能看 `has()`，不能看值。）
    let has_id = args.has("element_id");
    let has_x = args.has("x");
    let has_y = args.has("y");
    if !has_id && !has_x && !has_y {
        return Err(ToolError::bad_args(
            "缺少定位：给 element_id（推荐，来自 screen_elements 的编号）或完整的 x/y 坐标",
        )
        .with_hint("先跑 screen_elements 拿编号清单，再按编号点击"));
    }
    if !has_id && has_x != has_y {
        let missing = if has_x { "y" } else { "x" };
        return Err(
            ToolError::bad_args(format!("坐标定位必须同时给 x 和 y；缺少 `{missing}`"))
                .with_hint("补全 x/y，或改用 screen_elements 返回的 element_id"),
        );
    }

    // 定位：element_id 优先（离散引用比手算像素可靠），x/y 是直连兜底。
    let located_by;
    let (x, y) = if has_id {
        let id = args.opt_int("element_id")? as usize;
        let Some(el) = super::screen_uia::cache_lookup(id) else {
            return Err(
                ToolError::bad_args(format!("编号 #{id} 查不到：缓存里没有它")).with_hint(
                    "编号来自 `screen_elements` 的最近一次结果（5 分钟内有效）。\
                     先重新跑一次 screen_elements，再用返回列表里的 id。",
                ),
            );
        };
        located_by = format!("element_id #{id}");
        el.center()
    } else {
        located_by = "x/y".to_owned();
        (args.opt_int("x")? as i32, args.opt_int("y")? as i32)
    };

    screen::ensure_dpi_aware();
    screen::click(x, y, button, double)?;

    let what = if double { "双击" } else { "点击" };
    let scale = screen::dpi_scale_at(x, y);
    Ok(Outcome::ok(
        "click",
        format!("已在 ({x}, {y}) {what}{}", button_label(button)),
        json!({
            "x": x,
            "y": y,
            "located_by": located_by,
            "button": button.name(),
            "double": double,
            "dpi_scale": scale,
            "next": "要确认点到哪儿了，用 `screenshot` 再看一眼屏幕",
        }),
    ))
}

fn button_label(b: Button) -> &'static str {
    match b {
        Button::Left => "（左键）",
        Button::Right => "（右键）",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preview_reads_like_a_sentence() {
        let tool = crate::find("click").unwrap();

        let v = json!({ "x": 100, "y": 200 });
        assert_eq!(preview(&Args::new(tool, &v)), "在 (100, 200) 点击左键");

        let v = json!({ "x": 1, "y": 2, "button": "right", "double": true });
        assert_eq!(preview(&Args::new(tool, &v)), "在 (1, 2) 双击右键");

        // 编号定位的确认框要说的是"点哪个元素"，不是坐标
        let v = json!({ "element_id": 7 });
        assert_eq!(preview(&Args::new(tool, &v)), "点击左键：元素 #7");
    }

    /// 两种定位方式必须给一种 —— 漏了要报错，不能被悄悄当成 (0,0) 点下去。
    #[test]
    fn missing_locator_is_rejected_without_touching_the_mouse() {
        let tool = crate::find("click").unwrap();
        let value = json!({});
        let a = Args::new(tool, &value);
        let err = act(&a).unwrap_err();
        assert!(err.message.contains("缺少定位"), "实际：{}", err.message);
    }

    #[test]
    fn partial_xy_is_rejected_without_touching_the_mouse() {
        let tool = crate::find("click").unwrap();
        for value in [json!({ "x": 10 }), json!({ "y": 20 })] {
            let err = act(&Args::new(tool, &value)).unwrap_err();
            assert!(
                err.message.contains("同时给 x 和 y"),
                "实际：{}",
                err.message
            );
        }
    }

    /// 查不到的编号要在**发事件之前**拒绝，并告诉模型下一步怎么做。
    #[test]
    fn unknown_element_id_fails_before_sending_input() {
        let tool = crate::find("click").unwrap();
        let value = json!({ "element_id": 9999 });
        let a = Args::new(tool, &value);
        let err = act(&a).unwrap_err();
        assert!(err.message.contains("9999"));
        assert!(err.hint.unwrap().contains("screen_elements"));
    }
}
