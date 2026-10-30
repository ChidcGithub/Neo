use super::*;

fn theme() -> Theme {
    Theme::new(
        neo_theme::ThemeMode::Dark,
        1080.0,
        neo_theme::Distance::Standard,
    )
}

fn card() -> neo_tools::tools::present_result::Card {
    neo_tools::tools::present_result::Card {
        title: "勾股定理证明".into(),
        content: "## 结论\n\n$a^2+b^2=c^2$\n\n- 第一步\n- 第二步".into(),
    }
}

#[test]
fn presented_card_opens_centered_interactive_viewport() {
    let ctx = Context::default();
    ctx.set_embed_viewports(false);
    let mut state = AppState::default();
    state.presented = Some(Arc::new(card()));
    let mut win = ResultWin::default();
    let theme = theme();

    ctx.begin_pass(egui::RawInput::default());
    win.tick(&ctx, &mut state, theme, None);
    let mut output = ctx.end_pass();
    output.textures_delta.clear();

    let monitor = super::super::miniwin::screen_geometry(&ctx, false).monitor;
    let target = ResultWin::target_rect(theme, monitor);
    let viewport = &output.viewport_output[&viewport_id()];
    assert_eq!(viewport.builder.position, Some(target.min));
    assert_eq!(viewport.builder.inner_size, Some(target.size()));
    assert_eq!(viewport.builder.mouse_passthrough, Some(false));
    assert_eq!(viewport.builder.transparent, Some(false));
    assert_eq!(viewport.builder.active, Some(false));
    assert_eq!(viewport.builder.visible, Some(true));
    assert!(viewport.viewport_ui_cb.is_some());
    assert!(
        viewport
            .commands
            .iter()
            .any(|cmd| matches!(cmd, ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop)))
    );
    assert!(
        !viewport
            .commands
            .iter()
            .any(|cmd| matches!(cmd, ViewportCommand::Close | ViewportCommand::Focus))
    );
}

#[test]
fn closing_clears_presentation_and_moves_viewport_offscreen() {
    let ctx = Context::default();
    ctx.set_embed_viewports(false);
    let mut state = AppState::default();
    state.presented = Some(Arc::new(card()));
    let mut win = ResultWin::default();
    let theme = theme();

    ctx.begin_pass(egui::RawInput::default());
    win.tick(&ctx, &mut state, theme, None);
    ctx.end_pass().textures_delta.clear();
    assert!(state.presented.is_some());

    win.close_wanted.store(true, Ordering::Release);
    ctx.begin_pass(egui::RawInput::default());
    win.tick(&ctx, &mut state, theme, None);
    let mut output = ctx.end_pass();
    output.textures_delta.clear();

    assert!(state.presented.is_none());
    let viewport = &output.viewport_output[&viewport_id()];
    assert_eq!(viewport.builder.position, Some(OFFSCREEN));
    assert_eq!(viewport.builder.inner_size, Some(Vec2::splat(1.0)));
    assert!(
        viewport
            .commands
            .iter()
            .any(|cmd| matches!(cmd, ViewportCommand::OuterPosition(pos) if *pos == OFFSCREEN))
    );
}

#[test]
fn callback_renders_markdown_and_close_button() {
    let ctx = Context::default();
    neo_theme::fonts::install(&ctx);
    ctx.set_embed_viewports(false);
    let mut state = AppState::default();
    state.presented = Some(Arc::new(card()));
    let mut win = ResultWin::default();
    let theme = theme();
    theme.apply(&ctx);

    ctx.begin_pass(egui::RawInput::default());
    win.tick(&ctx, &mut state, theme, None);
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    let viewport = &output.viewport_output[&viewport_id()];
    let callback = viewport.viewport_ui_cb.as_ref().unwrap();
    let size = viewport.builder.inner_size.unwrap();

    let mut input = egui::RawInput {
        viewport_id: viewport_id(),
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        ..Default::default()
    };
    input.viewports.insert(
        viewport_id(),
        egui::ViewportInfo {
            parent: Some(ViewportId::ROOT),
            ..Default::default()
        },
    );
    let mut output = ctx.run_ui(input, |ui| callback(ui));
    output.textures_delta.clear();
    let texts: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        })
        .collect();
    assert!(texts.iter().any(|text| text.contains("勾股定理证明")));
    assert!(texts.iter().any(|text| text.contains("第一步")));
    let close: Rect = ctx.data(|data| {
        data.get_temp(egui::Id::new("neo-resultwin-close-probe"))
            .expect("close button probe")
    });
    assert!(Rect::from_min_size(Pos2::ZERO, size).contains_rect(close));
}

#[test]
fn closed_state_keeps_viewport_registered_offscreen() {
    let ctx = Context::default();
    ctx.set_embed_viewports(false);
    let mut state = AppState::default();
    let mut win = ResultWin::default();
    let theme = theme();

    for _ in 0..2 {
        ctx.begin_pass(egui::RawInput::default());
        win.tick(&ctx, &mut state, theme, None);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let viewport = &output.viewport_output[&viewport_id()];
        assert!(viewport.viewport_ui_cb.is_some());
        assert_eq!(viewport.builder.position, Some(OFFSCREEN));
        assert_eq!(viewport.builder.inner_size, Some(Vec2::splat(1.0)));
        assert_eq!(viewport.builder.visible, Some(true));
        assert!(
            !viewport
                .commands
                .iter()
                .any(|cmd| matches!(cmd, ViewportCommand::Close | ViewportCommand::Focus))
        );
    }
}
