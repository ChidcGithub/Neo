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
