
    use super::*;
    use serde_json::json;

    #[test]
    fn original_size_attachment_allows_jpeg_fallback_and_rejects_mismatch() {
        use base64::Engine;
        let (width, height) = (1536, 1024);
        let mut state = 1234567u32;
        let image = image::RgbImage::from_fn(width, height, |_, _| {
            let mut rgb = [0; 3];
            for byte in &mut rgb {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = state as u8;
            }
            image::Rgb(rgb)
        });
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert!(png.get_ref().len() > super::super::view_image::MAX_MODEL_IMAGE_BYTES);
        let prepared = prepare_image(png.get_ref(), (width, height)).unwrap();
        assert!(prepared.url.starts_with("data:image/jpeg;base64,"));
        assert_eq!(prepared.sent_size, (width, height));
        let bytes = base64::engine::general_purpose::STANDARD.decode(prepared.url.split_once(',').unwrap().1).unwrap();
        assert!(bytes.len() <= super::super::view_image::MAX_MODEL_IMAGE_BYTES);
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (width, height));
        assert!(prepare_image(png.get_ref(), (width / 2, height / 2)).is_err());
        assert!(prepare_image(b"invalid image", (1, 1)).is_err());
        let wide = image::RgbImage::new(super::super::view_image::MAX_MODEL_IMAGE_EDGE + 1, 1);
        let mut png = std::io::Cursor::new(Vec::new());
        wide.write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert!(prepare_image(png.get_ref(), (wide.width(), 1)).is_err());
        let small = image::RgbImage::new(10, 20);
        let mut png = std::io::Cursor::new(Vec::new());
        small.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let prepared = prepare_image(png.get_ref(), (10, 20)).unwrap();
        assert!(prepared.url.starts_with("data:image/png;base64,"));
        assert_eq!(prepared.source_size, prepared.sent_size);
    }

    #[test]
    fn waiting_desktop_tools_cancel_before_any_desktop_access() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        for (name, value) in [("screenshot", json!({})), ("click", json!({"x": 1, "y": 1})),
            ("drag", json!({"x": 1, "y": 1, "to_x": 2, "to_y": 2}))] {
            let interaction = super::super::screen_uia::INTERACTION.lock().unwrap();
            let token = Arc::new(AtomicBool::new(false));
            let scope = Scope::new(std::env::temp_dir()).with_cancel(token.clone());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let worker_barrier = barrier.clone();
            let worker = std::thread::spawn(move || {
                let tool = crate::find(name).unwrap();
                worker_barrier.wait();
                (tool.run)(&scope, &Args::new(tool, &value))
            });
            barrier.wait();
            token.store(true, Ordering::Release);
            drop(interaction);
            let outcome = worker.join().unwrap();
            assert_eq!(outcome.error.unwrap().kind, crate::cancelled_error().kind);
            assert!(outcome.images.is_empty());
        }
    }

    #[cfg(windows)]
    #[test]
    fn saving_png_rejects_screenshot_directory_junction_escape() {
        let dir = std::env::temp_dir().join(format!("neo-shot-fence-{}-{}", std::process::id(), epoch_ms()));
        let workspace = dir.join("workspace");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = workspace.join("screenshots");
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path $env:NEO_TEST_LINK -Target $env:NEO_TEST_TARGET | Out-Null"])
            .env("NEO_TEST_LINK", &link)
            .env("NEO_TEST_TARGET", &outside)
            .status().unwrap();
        assert!(status.success());
        let scope = Scope::new(&workspace);
        let result = save_png(&scope, b"synthetic image bytes");
        let outside_count = std::fs::read_dir(&outside).unwrap().count();
        std::fs::remove_dir(&link).unwrap();
        let normal = save_png(&scope, b"synthetic image bytes").unwrap();
        assert_eq!(std::fs::read(normal).unwrap(), b"synthetic image bytes");
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(result.unwrap_err().kind, ErrorKind::NotAllowed);
        assert_eq!(outside_count, 0);
    }

    #[test]
    fn region_all_sixteen_combinations_and_schema_are_strict() {
        let tool = crate::find("screenshot").unwrap();
        let keys = ["x", "y", "width", "height"];
        for mask in 0..16 {
            let mut value = json!({});
            for (bit, key) in keys.iter().enumerate() {
                if mask & (1 << bit) != 0 { value[*key] = json!(10); }
            }
            assert_eq!(region(&Args::new(tool, &value)).is_ok(), mask == 0 || mask == 15, "{mask}");
        }
        for value in [json!({"x": null}), json!({"x": null, "y": null, "width": null, "height": null}),
            json!({"right": 10}), json!({"screenshot_id": null}), json!({"screenshot_id": "old"}),
            json!({"x": 0, "y": 0, "width": 0, "height": 1}), json!({"x": 0, "y": 0, "width": 1, "height": -1}),
            json!(null), json!([])] {
            assert!(region(&Args::new(tool, &value)).is_err(), "{value}");
            assert!(shot(&Scope::new(std::env::temp_dir()), &Args::new(tool, &value)).is_err());
        }
        let unknown = json!({"screenshot_id": "definitely-unknown", "x": 0, "y": 0, "width": 1, "height": 1});
        assert!(shot(&Scope::new(std::env::temp_dir()), &Args::new(tool, &unknown)).unwrap_err().message.contains("screenshot_id"));
        let schema = tool.schema();
        assert_eq!(schema["additionalProperties"], false);
        for key in ["width", "height"] {
            assert_eq!(schema["properties"][key]["minimum"], 1);
            assert!(schema["properties"][key].get("default").is_none());
        }
    }

    #[test]
    fn png_saves_are_unique_and_never_overwrite() {
        let root = std::env::temp_dir().join(format!("neo-unique-shot-{}-{}", std::process::id(), epoch_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let scope = Scope::new(&root);
        let first = save_png(&scope, b"first").unwrap();
        let second = save_png(&scope, b"second").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(first).unwrap(), b"first");
        assert_eq!(std::fs::read(second).unwrap(), b"second");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn region_needs_all_four_or_none() {
        let tool = crate::find("screenshot").unwrap();

        let empty = json!({});
        assert!(
            region(&Args::new(tool, &empty)).unwrap().is_none(),
            "都不给 = 整屏"
        );

        let full = json!({ "x": 10, "y": 20, "width": 100, "height": 50 });
        let r = region(&Args::new(tool, &full)).unwrap().unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (10, 20, 100, 50));

        // 只给两个 → 明确拒绝，并指出缺了什么
        let half = json!({ "x": 10, "y": 20 });
        let e = region(&Args::new(tool, &half)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadArguments);
        assert!(e.message.contains("width"), "{}", e.message);
        assert!(e.message.contains("height"), "{}", e.message);
    }

    #[test]
    fn negative_coordinates_are_allowed() {
        // 副屏排在左边时 x 是负数 —— 范围声明必须容得下
        let tool = crate::find("screenshot").unwrap();
        let v = json!({ "x": -1920, "y": -100, "width": 800, "height": 600 });
        let r = region(&Args::new(tool, &v)).unwrap().unwrap();
        assert_eq!(r.x, -1920);
        assert_eq!(r.y, -100);
    }

    #[test]
    fn preview_says_what_will_be_captured() {
        let tool = crate::find("screenshot").unwrap();
        let empty = json!({});
        assert!(preview(&Args::new(tool, &empty)).contains("整个屏幕"));
        let v = json!({ "x": 0, "y": 0, "width": 10, "height": 10 });
        assert!(
            preview(&Args::new(tool, &v)).contains("10×10"),
            "{}",
            preview(&Args::new(tool, &v))
        );
    }
