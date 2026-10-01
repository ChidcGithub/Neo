
    use super::*;

    #[test]
    fn model_image_formats_dimensions_and_corruption() {
        use base64::Engine;
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg,
            image::ImageFormat::Bmp, image::ImageFormat::Gif, image::ImageFormat::WebP] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, format).unwrap();
            let url = model_image(bytes.get_ref(), true).unwrap();
            assert!(url.starts_with("data:image/png;base64,") || url.starts_with("data:image/jpeg;base64,"));
            let data = base64::engine::general_purpose::STANDARD.decode(url.split_once(',').unwrap().1).unwrap();
            let image = image::load_from_memory(&data).unwrap();
            assert_eq!((image.width(), image.height()), (2, 3));
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4100, 2).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        assert!(model_image(bytes.get_ref(), false).is_err());
        let url = model_image(bytes.get_ref(), true).unwrap();
        let data = base64::engine::general_purpose::STANDARD.decode(url.split_once(',').unwrap().1).unwrap();
        assert_eq!(image::load_from_memory(&data).unwrap().width(), 1536);
        assert!(model_image(&bytes.get_ref()[..24], true).is_err());
    }

    #[test]
    fn view_image_reports_exact_source_and_sent_sizes_without_desktop_origin() {
        use base64::Engine;
        let root = std::env::temp_dir().join(format!("neo-image-sizes-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2001, 1003).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        std::fs::write(root.join("shot.png"), bytes.into_inner()).unwrap();
        let out = crate::dispatch(&Scope::new(&root), "view_image", &json!({"path": "shot.png", "include_data": true}));
        assert!(out.is_ok());
        assert_eq!(out.data["source_size"], json!({"width": 2001, "height": 1003}));
        let bytes = base64::engine::general_purpose::STANDARD.decode(out.images[0].split_once(',').unwrap().1).unwrap();
        let sent = image::load_from_memory(&bytes).unwrap();
        assert_eq!(sent.width(), PREVIEW_EDGE);
        assert_eq!(out.data["sent_size"], json!({"width": sent.width(), "height": sent.height()}));
        assert_eq!(out.data["source_to_sent_scale"], json!({
            "x": {"numerator": sent.width(), "denominator": 2001},
            "y": {"numerator": sent.height(), "denominator": 1003},
        }));
        assert_eq!(out.data["desktop_locatable"], false);
        assert!(out.data.get("region").is_none());
        assert!(out.data.get("origin").is_none());
        let info = crate::dispatch(&Scope::new(&root), "view_image", &json!({"path": "shot.png", "include_data": false}));
        assert!(info.images.is_empty());
        assert_eq!(info.data["image_attached"], false);
        assert!(info.data["sent_size"].is_null());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn view_image_input_budget_is_32_mib_not_legacy_8_mib() {
        let root = std::env::temp_dir().join(format!("neo-image-input-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let mut bytes = bytes.into_inner();
        bytes.resize(8 * 1024 * 1024 + 1, 0);
        let path = root.join("large.png");
        std::fs::write(&path, &bytes).unwrap();
        let args = json!({"path": "large.png", "include_data": true});
        assert!(crate::dispatch(&Scope::new(&root), "view_image", &args).is_ok());
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(MAX_SOURCE_IMAGE_BYTES as u64 + 1).unwrap();
        let out = crate::dispatch(&Scope::new(&root), "view_image", &args);
        assert_eq!(out.error.unwrap().kind, ErrorKind::TooLarge);
        assert!(out.images.is_empty());
        bytes.resize(MAX_SOURCE_IMAGE_BYTES + 1, 0);
        assert!(model_image(&bytes, false).unwrap_err().contains("32 MiB"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_image_jpeg_fallback_keeps_coordinate_dimensions() {
        use base64::Engine;
        let mut seed = 1u32;
        let image = image::RgbImage::from_fn(1800, 1000, |_, _| {
            let mut pixel = [0; 3];
            for channel in &mut pixel {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *channel = seed as u8;
            }
            image::Rgb(pixel)
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        assert!(bytes.get_ref().len() > MAX_MODEL_IMAGE_BYTES);
        let prepared = model_image_with_sizes(bytes.get_ref(), false).unwrap();
        assert!(prepared.url.starts_with("data:image/jpeg;base64,"));
        assert_eq!(prepared.source_size, (1800, 1000));
        assert_eq!(prepared.sent_size, prepared.source_size);
        let sent = base64::engine::general_purpose::STANDARD.decode(prepared.url.split_once(',').unwrap().1).unwrap();
        assert!(sent.len() <= MAX_MODEL_IMAGE_BYTES);
        let decoded = image::load_from_memory(&sent).unwrap();
        assert_eq!((decoded.width(), decoded.height()), prepared.source_size);
    }

    #[test]
    fn sniffs_png_header() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        let info = sniff(&png).unwrap();
        assert_eq!((info.format, info.width, info.height), ("png", 1920, 1080));
    }

    #[test]
    fn top_down_bmp_has_positive_height() {
        let mut bmp = vec![0; 54];
        bmp[..2].copy_from_slice(b"BM");
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&320i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-240i32).to_le_bytes());
        let info = sniff(&bmp).unwrap();
        assert_eq!((info.width, info.height), (320, 240));
    }

    #[test]
    fn oversized_pixel_header_is_not_attached() {
        let root = std::env::temp_dir().join(format!("neo-image-budget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&5000u32.to_be_bytes());
        png.extend_from_slice(&5000u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(root.join("超大.png"), png).unwrap();
        let out = crate::dispatch(&Scope::new(&root), "view_image", &json!({"path": "超大.png", "include_data": true}));
        assert_eq!(out.error.unwrap().kind, ErrorKind::TooLarge);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_image() {
        assert!(sniff(b"hello world, not an image at all").is_none());
    }
