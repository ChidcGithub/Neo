pub use neo_tools::documents::{load, Attachment, MAX_FILES};

/// 工具图片不静默缩放：截图的坐标必须继续对应原始物理像素。
pub fn prepare_tool_images(outcome: &mut neo_tools::Outcome) {
    use base64::Engine;
    let mut images = Vec::new();
    let mut warnings = Vec::new();
    for url in std::mem::take(&mut outcome.images) {
        let prepared = (|| {
            let (header, data) = url.split_once(',').ok_or("图片不是 data URL")?;
            if !matches!(header, "data:image/png;base64" | "data:image/jpeg;base64"
                | "data:image/gif;base64" | "data:image/bmp;base64" | "data:image/webp;base64")
                || data.len() > (32 * 1024 * 1024 + 2) / 3 * 4 {
                return Err("图片格式或编码体积超限".to_owned());
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(data)
                .map_err(|_| "图片 base64 损坏".to_owned())?;
            neo_tools::tools::view_image::model_image(&bytes, false)
        })();
        match prepared {
            Ok(image) => images.push(image),
            Err(error) => warnings.push(error),
        }
    }
    outcome.images = images;
    if !warnings.is_empty() {
        if !outcome.data.is_object() {
            outcome.data = serde_json::json!({"result": outcome.data});
        }
        outcome.data["image_attached"] = serde_json::json!(!outcome.images.is_empty());
        outcome.data["image_warning"] = serde_json::json!(format!(
            "部分图片未附送：{}。原文件不变；请缩小截图区域或用 view_image 查看。", warnings.join("；")
        ));
        outcome.summary.push_str("（部分图片未附送，见 image_warning）");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn tool_image_reencodes_bmp_and_explains_rejection() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, image::ImageFormat::Bmp).unwrap();
        let mut outcome = neo_tools::Outcome::ok("screenshot", "截图", serde_json::json!({"x": -20}))
            .with_image(format!("data:image/bmp;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())))
            .with_image("data:image/png;base64,broken");
        prepare_tool_images(&mut outcome);
        assert_eq!(outcome.images.len(), 1);
        neo_llm::validate_image(&outcome.images[0]).unwrap();
        assert_eq!(outcome.data["x"], -20);
        assert!(outcome.data["image_warning"].as_str().unwrap().contains("未附送"));
    }
}
