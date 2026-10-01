pub use neo_tools::documents::{load, Attachment, MAX_FILES};

/// 工具图片不静默缩放：截图的坐标必须继续对应原始物理像素。
pub fn prepare_tool_images(outcome: &mut neo_tools::Outcome) {
    use base64::Engine;
    use neo_tools::tools::view_image::{model_image_with_sizes, MAX_SOURCE_IMAGE_BYTES};
    let had_images = !outcome.images.is_empty();
    let mut images = Vec::new();
    let mut warnings = Vec::new();
    if !had_images && (outcome.data["image_attached"] == true || outcome.data["screenshot_id"].is_string()) {
        warnings.push("工具声称附图或提供 screenshot_id，但没有可发送的图片数据".to_owned());
    }
    for url in std::mem::take(&mut outcome.images) {
        let prepared = (|| {
            let (header, data) = url.split_once(',').ok_or("图片不是 data URL")?;
            if !matches!(header, "data:image/png;base64" | "data:image/jpeg;base64"
                | "data:image/gif;base64" | "data:image/bmp;base64" | "data:image/webp;base64")
                || data.len() > (MAX_SOURCE_IMAGE_BYTES + 2) / 3 * 4 {
                return Err("图片格式或编码体积超限".to_owned());
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(data)
                .map_err(|_| "图片 base64 损坏".to_owned())?;
            let image = model_image_with_sizes(&bytes, false)?;
            let (width, height) = image.sent_size;
            for (field, w, h) in [
                ("sent_size", "width", "height"),
                ("image_space", "width", "height"),
            ] {
                if let Some(size) = outcome.data.get(field).filter(|v| !v.is_null()) {
                    if size[w].as_u64() != Some(u64::from(width)) || size[h].as_u64() != Some(u64::from(height)) {
                        return Err(format!("{field} 与实际附图尺寸不一致"));
                    }
                }
            }
            if outcome.tool == "screenshot" {
                for (key, expected) in [("sent_width", width), ("sent_height", height)] {
                    if let Some(value) = outcome.data.get(key).filter(|v| !v.is_null()) {
                        if value.as_u64() != Some(u64::from(expected)) {
                            return Err(format!("{key} 与实际附图尺寸不一致"));
                        }
                    }
                }
                if let Some(mapping) = outcome.data.get("image_to_desktop") {
                    let region = &outcome.data["region"];
                    if mapping["scale_x"] != 1 || mapping["scale_y"] != 1
                        || mapping["dpi_conversion"] != false
                        || mapping["offset_x"] != region["x"] || mapping["offset_y"] != region["y"]
                        || region["width"].as_u64() != Some(u64::from(width))
                        || region["height"].as_u64() != Some(u64::from(height)) {
                        return Err("image_to_desktop 与原尺寸附图或 region 不一致".to_owned());
                    }
                }
            }
            neo_llm::validate_image(&image.url)?;
            Ok(image.url)
        })();
        match prepared {
            Ok(image) => images.push(image),
            Err(error) => warnings.push(error),
        }
    }
    outcome.images = images;
    if had_images && !outcome.data.is_object() {
        outcome.data = serde_json::json!({"result": outcome.data});
    }
    if outcome.tool == "screenshot" && outcome.images.len() == 1 {
        let (header, encoded) = outcome.images[0].split_once(',').unwrap();
        let format = if header == "data:image/png;base64" { "png" } else { "jpeg" };
        outcome.data["sent_format"] = serde_json::json!(format);
        outcome.data["lossless"] = serde_json::json!(format == "png");
        outcome.data["sent_bytes"] = serde_json::json!(encoded.len() / 4 * 3 - encoded.bytes().rev().take_while(|b| *b == b'=').count());
    }
    if had_images || !warnings.is_empty() || outcome.data.get("image_attached").is_some() {
        if !outcome.data.is_object() {
            outcome.data = serde_json::json!({"result": outcome.data});
        }
        outcome.data["image_attached"] = serde_json::json!(!outcome.images.is_empty());
    }
    if !warnings.is_empty() {
        let status = if outcome.images.is_empty() { "图片未发送" } else { "部分图片未附送" };
        outcome.data["image_warning"] = serde_json::json!(format!(
            "{status}：{}。原文件不变；请重新截取更小的区域（x/y/width/height），不要用未发送图片定位。", warnings.join("；")
        ));
        outcome.data["next"] = serde_json::json!("重新截取更小的区域（x/y/width/height）以获取可发送图片及新的截图引用；不得按未发送图片或 view_image 缩略图操作桌面。");
        if outcome.images.is_empty() {
            if let Some(id) = outcome.data["screenshot_id"].as_str() {
                neo_tools::tools::screenshot_space::revoke(id);
            }
            if let Some(data) = outcome.data.as_object_mut() {
                for key in ["sent_size", "sent_width", "sent_height", "sent_format", "lossless", "screenshot_id"] {
                    if data.contains_key(key) { data.insert(key.into(), serde_json::Value::Null); }
                }
                if data.contains_key("sent_bytes") { data.insert("sent_bytes".into(), serde_json::json!(0)); }
                for key in ["source_to_sent_scale", "image_to_desktop", "image_space", "reference_ttl_seconds"] {
                    data.remove(key);
                }
            }
        }
        outcome.summary = if outcome.tool == "screenshot" && outcome.is_ok() {
            format!("截图已保存到 {}，{status}；请重新截取更小的区域，见 image_warning。",
                outcome.data["path"].as_str().unwrap_or("工具返回路径"))
        } else {
            format!("{}：{status}；原文件不变，见 image_warning。", outcome.tool)
        };
    }
}

// 仅改出站副本；数据库正文和摘要指纹仍保留原始审计记录。
pub fn tool_content_without_images(content: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(content) else {
        return if content.contains("已附图") || content.contains("已把图交给模型") {
            "[历史图片未随本次请求提供；原始记录保留在聊天历史中，操作前必须重新观察]".into()
        } else { content.to_owned() };
    };
    let data = &value["data"];
    if data["image_attached"] != true && !data["screenshot_id"].is_string()
        && data.get("image_to_desktop").is_none() && data.get("image_space").is_none()
        && !content.contains("已附图") && !content.contains("已把图交给模型") {
        return content.to_owned();
    }
    if !value.is_object() { return content.to_owned(); }
    let path = value["data"]["path"].as_str().map(str::to_owned);
    value.as_object_mut().unwrap().retain(|key, _| {
        matches!(key.as_str(), "ok" | "tool" | "call_id" | "tool_call_id")
    });
    value["historical_image"] = serde_json::json!(true);
    value["data"] = serde_json::json!({"image_attached": false});
    if let Some(path) = path { value["data"]["path"] = serde_json::json!(path); }
    value["data"]["next"] = serde_json::json!("重新观察并获取新的图片和引用，不得使用历史映射操作桌面。");
    value["summary"] = serde_json::json!("历史图片未随本次请求提供；原始审计记录仅保留在聊天历史中。");
    value.to_string()
}

fn clear_image_reference(data: &mut serde_json::Value) {
    if let Some(data) = data.as_object_mut() {
        data.insert("screenshot_id".into(), serde_json::Value::Null);
        data.insert("reference_status".into(), serde_json::json!("stale"));
        for key in ["image_to_desktop", "image_space", "source_to_sent_scale", "reference_ttl_seconds"] {
            data.remove(key);
        }
    }
}

pub fn mark_stale_screenshot(outcome: &mut neo_tools::Outcome) {
    // 图仍是历史观察证据，但交付前引用已失效；不重新注册或猜测当前桌面。
    outcome.data["historical_reference"] = serde_json::json!(true);
    clear_image_reference(&mut outcome.data);
    outcome.data["next"] = serde_json::json!("截图引用在交付前已失效；附图仅为历史观察，操作前必须重新观察获取新引用。");
    outcome.summary = "保留历史观察图，截图引用已失效；操作前必须重新观察。".into();
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;
