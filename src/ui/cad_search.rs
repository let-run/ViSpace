//! The application header is the single search entry point for all CAD tabs.
use super::*;
#[derive(Clone)]
enum Found {
    Schematic(SchematicMatch),
    Board(super::super::pcb::PcbMatch),
}
impl Found {
    fn fields(&self) -> (&str, &str, &str) {
        match self {
            Self::Schematic(m) => (&m.title, &m.value, &m.package),
            Self::Board(m) => (&m.title, &m.value, &m.package),
        }
    }
}
#[derive(Default)]
pub(super) struct CadSearch {
    query: String,
    open: bool,
    scope: String,
    stamp: Option<(String, String)>,
    results: Vec<Found>,
}
impl KicadState {
    fn cad_search_results(&mut self, project: &str, tab: usize) {
        let (scope, results) = if tab == 3 {
            if let Some(v) = self
                .viewer
                .as_ref()
                .filter(|v| key(&v.project) == key(project))
            {
                let scope = format!(
                    "{project}:{tab}:{}:{}",
                    Arc::as_ptr(&v.bundle) as usize,
                    v.instance
                );
                if self.cad_search.stamp.as_ref()
                    == Some(&(scope.clone(), self.cad_search.query.clone()))
                {
                    return;
                }
                (
                    scope,
                    schematic_matches(&v.bundle, &v.instance, &self.cad_search.query)
                        .into_iter()
                        .map(Found::Schematic)
                        .collect(),
                )
            } else {
                (format!("{project}:{tab}:loading"), vec![])
            }
        } else if let Some(v) = self
            .pcb_viewer
            .as_ref()
            .filter(|v| key(&v.project) == key(project))
        {
            let scope = format!("{project}:{tab}:{}", Arc::as_ptr(&v.board) as usize);
            if self.cad_search.stamp.as_ref()
                == Some(&(scope.clone(), self.cad_search.query.clone()))
            {
                return;
            }
            (
                scope,
                super::super::pcb::pcb_matches(&v.board, &self.cad_search.query)
                    .into_iter()
                    .map(Found::Board)
                    .collect(),
            )
        } else {
            (format!("{project}:{tab}:loading"), vec![])
        };
        if self.cad_search.scope != scope {
            self.cad_search.open = false;
            self.cad_search.scope = scope.clone();
        }
        self.cad_search.stamp = Some((scope, self.cad_search.query.clone()));
        self.cad_search.results = results;
    }
    fn apply_cad_search(&mut self, found: Found, ctx: &egui::Context) {
        match found {
            Found::Schematic(found) => {
                if let Some(viewer) = &mut self.viewer {
                    viewer.navigate(found.instance.clone());
                    if let Some(net) = found.net {
                        viewer.select_net(net, ctx.input(|i| i.time));
                    } else if let (Some(point), Some(id)) = (found.point, found.component) {
                        viewer.select_component(found.instance, id, point, true);
                    }
                }
                self.sync_schematic_selection();
            }
            Found::Board(found) => {
                if let Some(viewer) = &mut self.pcb_viewer {
                    let reference = found.reference.clone();
                    let net = found.title.clone();
                    let key = viewer.key.clone();
                    let project = viewer.project.clone();
                    viewer.apply_search_match(found, ctx.content_rect(), ctx);
                    if let Some(reference) = reference {
                        self.select_pcb_footprint(&key, &project, Some(&reference));
                    } else {
                        self.select_pcb_net(&key, &project, Some(&net), ctx.input(|i| i.time));
                    }
                }
            }
        }
        self.cad_search.open = false;
        ctx.request_repaint();
    }
    pub(in crate::ui) fn global_search(
        &mut self,
        ui: &mut egui::Ui,
        width: f32,
        project: &str,
        tab: usize,
    ) {
        self.cad_search_results(project, tab);
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.cad_search.query)
                .id(egui::Id::new("global_cad_search"))
                .frame(false)
                .desired_width((width - 22.).max(40.))
                .hint_text("Designator, part, SPN, value, package or net…"),
        );
        if response.changed() {
            self.cad_search_results(project, tab);
        }
        if response.gained_focus() || response.changed() || response.clicked() {
            self.cad_search.open = true;
        }
        let mut chosen = None;
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            chosen = self.cad_search.results.first().cloned();
            self.cad_search.open = false;
        }
        if !self.cad_search.query.is_empty()
            && theme::tool_sized(ui, Icon::Close, "Clear search", egui::vec2(16., 16.), false)
                .clicked()
        {
            self.cad_search.query.clear();
            self.cad_search.open = false;
            self.cad_search.stamp = None;
        }
        let results = &self.cad_search.results;
        let height = results
            .iter()
            .map(|r| if r.fields().2.is_empty() { 30. } else { 44. })
            .sum::<f32>()
            .clamp(28., 360.);
        egui::Popup::from_response(&response)
            .open_bool(&mut self.cad_search.open)
            .width(width.max(300.))
            .frame(
                egui::Frame::popup(ui.style())
                    .fill(theme::PANEL)
                    .corner_radius(8)
                    .inner_margin(5),
            )
            .show(|ui| {
                ui.set_width((width - 10.).max(290.));
                ui.set_height(height);
                ui.spacing_mut().item_spacing.y = 2.;
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.));
                egui::ScrollArea::vertical()
                    .max_height(360.)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if results.is_empty() {
                            ui.label(RichText::new("No matches").color(theme::MUTED));
                        }
                        for found in results {
                            let (title, value, package) = found.fields();
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(
                                    ui.available_width(),
                                    if package.is_empty() { 28. } else { 42. },
                                ),
                                egui::Sense::click(),
                            );
                            if response.hovered() {
                                ui.painter().rect_filled(rect, 4., theme::SELECTION);
                            }
                            let painter = ui.painter().with_clip_rect(rect.shrink(3.));
                            let title = painter.layout_no_wrap(
                                title.into(),
                                egui::FontId::proportional(12.),
                                theme::YELLOW,
                            );
                            painter.galley(
                                rect.min + egui::vec2(4., 4.),
                                title.clone(),
                                theme::YELLOW,
                            );
                            painter.text(
                                rect.min + egui::vec2(title.size().x + 12., 4.),
                                egui::Align2::LEFT_TOP,
                                value,
                                egui::FontId::proportional(11.),
                                theme::MUTED,
                            );
                            if !package.is_empty() {
                                painter.text(
                                    rect.min + egui::vec2(4., 23.),
                                    egui::Align2::LEFT_TOP,
                                    package,
                                    egui::FontId::proportional(10.),
                                    theme::MUTED,
                                );
                            }
                            if response.clicked() {
                                chosen = Some(found.clone());
                            }
                        }
                    });
            });
        if let Some(found) = chosen {
            self.apply_cad_search(found, ui.ctx());
            response.surrender_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> KicadState {
        let mut state = KicadState::default();
        let mut board=inventory_kicad::Pcb::parse(r#"(kicad_pcb (layers (0 "F.Cu" signal))
            (footprint "Demo" (layer "F.Cu") (at 5 5) (property "Reference" "U1") (property "Value" "CHIP")
                (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu"))))"#).unwrap();
        board.net_names = (1..=8)
            .map(|i| (i.to_string(), format!("SIG{i}")))
            .collect();
        state.pcb_viewer = Some(super::super::super::pcb::PcbViewer::new(
            "Demo",
            "test",
            Arc::new(board),
        ));
        state
    }
    #[test]
    fn shared_field_enter_selects_pcb_and_3d_designators_without_switching_tabs() {
        for tab in [4, 5] {
            let mut state = state();
            state.cad_search.query = "U1".into();
            state.opened_pcb = tab == 4;
            state.opened_3d = tab == 5;
            state.open_3d = Some(("Demo".into(), "test".into()));
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let draw = |state: &mut KicadState, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800., 600.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.horizontal(|ui| state.global_search(ui, 280., "Demo", tab));
                        });
                    },
                )
            };
            draw(&mut state, vec![]);
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("global_cad_search")));
            draw(&mut state, vec![]);
            draw(
                &mut state,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
            assert_eq!(
                state.pcb_viewer.as_ref().unwrap().selected_footprint(),
                Some("U1")
            );
            assert_eq!(state.opened_3d, tab == 5);
            assert!(!state.cad_search.open);
            state.cad_search.query = "SIG1".into();
            state.cad_search_results("Demo", tab);
            state.cad_search.open = true;
            draw(&mut state, vec![]);
            let output = draw(&mut state, vec![]);
            let position = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == "SIG1" && t.pos.y > 35. => {
                        Some(t.pos + egui::vec2(8., 6.))
                    }
                    _ => None,
                })
                .next_back()
                .expect("header net result");
            draw(
                &mut state,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            draw(
                &mut state,
                vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            assert_eq!(
                state.pcb_viewer.as_ref().unwrap().selected_net(),
                Some("SIG1")
            );
            assert_eq!(state.opened_3d, tab == 5);
            assert!(!state.cad_search.open);
        }
    }
    #[test]
    fn shared_autocomplete_expands_after_short_results() {
        let mut state = state();
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let draw = |state: &mut KicadState| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800., 600.),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.horizontal(|ui| state.global_search(ui, 280., "Demo", 4));
                    });
                },
            )
        };
        state.cad_search.query = "SIG1".into();
        state.cad_search_results("Demo", 4);
        state.cad_search.open = true;
        draw(&mut state);
        draw(&mut state);
        assert_eq!(state.cad_search.results.len(), 1);
        state.cad_search.query = "SIG".into();
        state.cad_search_results("Demo", 4);
        state.cad_search.open = true;
        draw(&mut state);
        let output = draw(&mut state);
        assert_eq!(state.cad_search.results.len(), 8);
        let visible=output.shapes.iter().filter(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.text().starts_with("SIG") && t.galley.text()!="SIG" && s.clip_rect.contains(t.pos+t.galley.size()*0.5))).count();
        assert_eq!(
            visible, 8,
            "all results must remain visible after popup expansion"
        );
    }
    #[test]
    fn schematic_results_use_the_same_field_and_navigate_to_the_symbol() {
        let bundle=Arc::new(inventory_kicad::load_schematics("demo.kicad_pro",|path|match path {
            "demo.kicad_pro"=>Ok("{}".into()),
            "demo.kicad_sch"=>Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (symbol (lib_id "Device:R") (at 5 5) (uuid sym) (property "Reference" "R1") (property "Value" "1k")))"#.into()),
            _=>Err(inventory_kicad::Error::new("missing")),
        }).unwrap());
        let mut state = KicadState {
            viewer: Some(Viewer::new(
                "Demo",
                "test",
                bundle.clone(),
                &bundle.root.instance_path,
            )),
            ..Default::default()
        };
        state.cad_search.query = "R1".into();
        state.cad_search_results("Demo", 3);
        let found = state.cad_search.results.first().unwrap().clone();
        state.apply_cad_search(found, &egui::Context::default());
        assert_eq!(
            state
                .viewer
                .as_ref()
                .unwrap()
                .selected_component
                .as_ref()
                .map(|(_, id)| id.as_str()),
            Some("sym")
        );
        assert!(state.viewer.as_ref().unwrap().focus_target.is_some());
    }
}
