/// DeepSeek 官方价目（美元 / 1M tokens，off-peak 价；peak 时段翻倍）。
struct Rates {
    input_hit: f64,
    input_miss: f64,
    output: f64,
}

const FLASH: Rates = Rates {
    input_hit: 0.003,
    input_miss: 0.15,
    output: 0.6,
};

const V4_PRO: Rates = Rates {
    input_hit: 0.022,
    input_miss: 0.66,
    output: 1.98,
};

/// 最长前缀优先。`deepseek-v4-flash*` 旧模型按 Flash 价计费。
const TABLE: &[(&str, &Rates)] = &[
    ("deepseek-v4-flash-vision-exp", &FLASH),
    ("deepseek-v4-flash", &FLASH),
    ("deepseek-v4-pro", &V4_PRO),
    ("deepseek-flash", &FLASH),
];

/// 估算一次请求的费用（如 `$0.0032`）；模型不在价目表时返回 None（UI 不显示金额）。
///
/// `cached` 是输入里缓存命中的部分（`prompt_tokens` 的子集），`miss` 是未命中部分。
/// 金额按当前 UTC 时间区分峰谷（峰时翻倍）；因未实现中国法定节假日日历，
/// 调用方应以 `≈` 前缀展示。
pub fn estimate(model: &str, cached: u64, miss: u64, output: u64) -> Option<String> {
    let rates = TABLE
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, rates)| *rates)?;
    let multiplier = if is_peak_utc() { 2.0 } else { 1.0 };
    let cost = (cached as f64 * rates.input_hit
        + miss as f64 * rates.input_miss
        + output as f64 * rates.output)
        / 1_000_000.0
        * multiplier;
    Some(format_cost(cost))
}

fn format_cost(cost: f64) -> String {
    if cost < 0.0001 {
        "<$0.0001".to_owned()
    } else if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

/// 峰时 = UTC 周一至周五 01:00–04:00 与 06:00–10:00（DeepSeek 官方定义，
/// 排除中国法定节假日 —— 此处不实现节假日日历，按工作日近似）。
fn is_peak_utc() -> bool {
    let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    let secs = d.as_secs();
    let hour = (secs % 86_400) / 3_600;
    // 1970-01-01 是周四；weekday: 0=周日。
    let weekday = (secs / 86_400 + 4) % 7;
    let weekday = weekday != 0 && weekday != 6;
    weekday && (matches!(hour, 1..=3) || matches!(hour, 6..=9))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_has_no_price() {
        assert_eq!(estimate("gpt-5.5", 1, 2, 3), None);
        assert_eq!(estimate("", 0, 0, 0), None);
    }

    #[test]
    fn longest_prefix_wins_and_flash_aliases_share_rate() {
        // v4-flash-vision-exp 与 v4-flash 的 Flash 价一致；v4-pro 单独一档。
        let pro = estimate("deepseek-v4-pro", 0, 1_000_000, 0).unwrap();
        let flash = estimate("deepseek-v4-flash", 0, 1_000_000, 0).unwrap();
        let vision = estimate("deepseek-v4-flash-vision-exp", 0, 1_000_000, 0).unwrap();
        assert_eq!(flash, vision);
        assert_ne!(pro, flash);
        // 最长前缀：带后缀的模型仍匹配到 vision-exp/flash 档而非更短的不相干键。
        assert!(estimate("deepseek-flash-mini", 0, 0, 0).is_some());
    }

    #[test]
    fn cached_and_miss_price_separately() {
        // 1M 全命中 vs 1M 全未命中（Flash，离线时段固定对比会随峰谷浮动，用比值断言）。
        let hit = estimate("deepseek-flash", 1_000_000, 0, 0).unwrap();
        let miss = estimate("deepseek-flash", 0, 1_000_000, 0).unwrap();
        let hit_v: f64 = hit.trim_start_matches(['$', '<']).parse().unwrap();
        let miss_v: f64 = miss.trim_start_matches(['$', '<']).parse().unwrap();
        assert!((miss_v / hit_v - 50.0).abs() < 0.5, "{hit} vs {miss}");
    }

    #[test]
    fn cost_formatting_is_adaptive() {
        assert_eq!(format_cost(0.0), "<$0.0001");
        assert_eq!(format_cost(0.00005), "<$0.0001");
        assert_eq!(format_cost(0.0032), "$0.0032");
        assert_eq!(format_cost(0.01234), "$0.01");
        assert_eq!(format_cost(1.5), "$1.50");
    }

    #[test]
    fn peak_window_matches_deepseek_schedule() {
        let at = |days: u64, hour: u64| {
            let secs = days * 86_400 + hour * 3_600 + 1_800;
            let hour = (secs % 86_400) / 3_600;
            let weekday = ((secs / 86_400) + 4) % 7;
            let weekday = weekday != 0 && weekday != 6;
            weekday && (matches!(hour, 1..=3) || matches!(hour, 6..=9))
        };
        // 1970-01-01 是周四（weekday=4）：days=0 周四，days=3 周日，days=4 周一。
        assert!(at(0, 2), "周四 02:00 峰时");
        assert!(at(0, 7), "周四 07:00 峰时");
        assert!(!at(0, 4), "周四 04:00 已过峰");
        assert!(!at(0, 10), "周四 10:00 已过峰");
        assert!(!at(0, 0), "周四 00:00 未入峰");
        assert!(!at(3, 2), "周日 02:00 周末不加价");
        assert!(at(4, 2), "周一 02:00 峰时");
    }
}
