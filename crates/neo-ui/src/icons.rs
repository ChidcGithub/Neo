//! 图标集。
//!
//! 全部字形来自 [Phosphor](https://phosphoricons.com/) 图标字体（MIT 许可）：
//! 字体文件内嵌在 `neo-theme`（[`neo_theme::fonts::ICON_FONT`]），字体装配时
//! 注入 egui 族链；本模块只负责「枚举 → 私有使用区字符」的映射，渲染就是
//! 在矩形中央画一个字符。
//!
//! 这套做法退役了旧的 SVG 路径光栅化管线（`paths.rs` + `tools/gen_icons.py`
//! + 每图标一张缓存纹理）：加图标从「跑生成器」变成「查 phosphor.com 加一行」。

use egui::{Align2, Color32, FontId, Painter, Rect};

/// 组件库认得的图标。
///
/// 枚举而不是散常量：调用方在按钮/输入框上只写 `Icon::Board`，
/// 不需要记住具体的字符，也方便日后加「选中态/填充态」变体。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Plus,
    ArrowUp,
    Stop,
    Folder,
    ChevronDown,
    Sun,
    Moon,
    Close,
    Cog,
    Trash,
    // 场景
    Board,
    Checklist,
    Pen,
    Mic,
    // 通用
    Check,
    Info,
    Warn,
    Sparkle,
    Copy,
    Search,
    ArrowLeft,
    ArrowRight,
    Dots,
}

/// 全部图标（顺序即枚举顺序），供遍历与测试使用。
pub const ALL: &[Icon] = &[
    Icon::Plus,
    Icon::ArrowUp,
    Icon::Stop,
    Icon::Folder,
    Icon::ChevronDown,
    Icon::Sun,
    Icon::Moon,
    Icon::Close,
    Icon::Cog,
    Icon::Trash,
    Icon::Board,
    Icon::Checklist,
    Icon::Pen,
    Icon::Mic,
    Icon::Check,
    Icon::Info,
    Icon::Warn,
    Icon::Sparkle,
    Icon::Copy,
    Icon::Search,
    Icon::ArrowLeft,
    Icon::ArrowRight,
    Icon::Dots,
];

impl Icon {
    /// Phosphor regular 字体里的私有区字符（右侧注释是 upstream 名字，
    /// 加图标时到 phosphor.com 查名取码）。
    pub fn glyph(self) -> &'static str {
        match self {
            Icon::Plus => "\u{E3D4}",       // plus
            Icon::ArrowUp => "\u{E08E}",    // arrow-up
            Icon::Stop => "\u{E46C}",       // stop
            Icon::Folder => "\u{E24A}",     // folder
            Icon::ChevronDown => "\u{E136}", // caret-down
            Icon::Sun => "\u{E472}",        // sun
            Icon::Moon => "\u{E330}",       // moon
            Icon::Close => "\u{E4F6}",      // x
            Icon::Cog => "\u{E270}",        // gear
            Icon::Trash => "\u{E4A6}",      // trash
            Icon::Board => "\u{E600}",      // chalkboard-teacher
            Icon::Checklist => "\u{EADC}",  // list-checks
            Icon::Pen => "\u{E3B4}",        // pencil-simple
            Icon::Mic => "\u{E326}",        // microphone
            Icon::Check => "\u{E182}",      // check
            Icon::Info => "\u{E2CE}",       // info
            Icon::Warn => "\u{E4E0}",       // warning
            Icon::Sparkle => "\u{E6A2}",    // sparkle
            Icon::Copy => "\u{E1CA}",       // copy
            Icon::Search => "\u{E30C}",     // magnifying-glass
            Icon::ArrowLeft => "\u{E058}",  // arrow-left
            Icon::ArrowRight => "\u{E06C}", // arrow-right
            Icon::Dots => "\u{E1FE}",       // dots-three
        }
    }

    /// 在 `rect` 内居中绘制。
    ///
    /// 字号取短边 × 1.25：Phosphor 字面在 em 框内留了约两成边距，
    /// 放大一格后与旧管线「viewBox 内接居中」的视觉尺寸相当。
    pub fn paint(self, painter: &Painter, rect: Rect, color: Color32) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 || color.a() == 0 {
            return;
        }
        let size = rect.width().min(rect.height()) * 1.25;
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            self.glyph(),
            FontId::proportional(size),
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个图标都映射到恰好一个私有使用区（PUA）字符 —— 守住码表别打错。
    #[test]
    fn every_icon_maps_to_a_single_pua_char() {
        for &icon in ALL {
            let mut chars = icon.glyph().chars();
            let Some(c) = chars.next() else {
                panic!("{icon:?} 的字形是空串");
            };
            assert!(
                ('\u{E000}'..='\u{F8FF}').contains(&c),
                "{icon:?} 的码位 {c:?} 不在私有使用区"
            );
            assert!(chars.next().is_none(), "{icon:?} 的字形不止一个字符");
        }
    }
}
