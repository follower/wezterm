use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::render::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::DimensionContext;
use crate::utilsprites::RenderMetrics;
use crate::TermWindow;
use config::keyassignment::{
    CharSelectArguments, CharSelectGroup, ClipboardCopyDestination, KeyAssignment,
};
use config::Dimension;
use emojis::{Emoji, Group};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;

struct MatchResults {
    selection: String,
    matches: Vec<usize>,
    group: CharSelectGroup,
}

pub struct CharSelector {
    group: RefCell<CharSelectGroup>,
    element: RefCell<Option<Vec<ComputedElement>>>,
    selection: RefCell<String>,
    aliases: Vec<Alias>,
    matches: RefCell<Option<MatchResults>>,
    selected_row: RefCell<usize>,
    top_row: RefCell<usize>,
    max_rows_on_screen: RefCell<usize>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Character {
    Unicode { name: &'static str, value: char },
    Emoji(&'static Emoji),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    name: Cow<'static, str>,
    character: Character,
    group: CharSelectGroup,
}

impl Alias {
    fn name(&self) -> &str {
        &self.name
    }

    fn glyph(&self) -> String {
        match &self.character {
            Character::Unicode { value, .. } => value.to_string(),
            Character::Emoji(emoji) => emoji.as_str().to_string(),
        }
    }

    fn codepoints(&self) -> String {
        let mut res = String::new();
        for c in self.glyph().chars() {
            if !res.is_empty() {
                res.push(' ');
            }
            res.push_str(&format!("U+{:X}", c as u32));
        }
        res
    }
}

fn build_aliases() -> Vec<Alias> {
    let mut aliases = vec![];
    let start = std::time::Instant::now();

    fn push(aliases: &mut Vec<Alias>, alias: Alias) {
        aliases.push(alias);
    }

    for emoji in emojis::iter() {
        let group = match emoji.group() {
            Group::SmileysAndEmotion => CharSelectGroup::SmileysAndEmotion,
            Group::PeopleAndBody => CharSelectGroup::PeopleAndBody,
            Group::AnimalsAndNature => CharSelectGroup::AnimalsAndNature,
            Group::FoodAndDrink => CharSelectGroup::FoodAndDrink,
            Group::TravelAndPlaces => CharSelectGroup::TravelAndPlaces,
            Group::Activities => CharSelectGroup::Activities,
            Group::Objects => CharSelectGroup::Objects,
            Group::Symbols => CharSelectGroup::Symbols,
            Group::Flags => CharSelectGroup::Flags,
        };
        push(
            &mut aliases,
            Alias {
                name: Cow::Borrowed(emoji.name()),
                character: Character::Emoji(emoji),
                group,
            },
        );
        if let Some(short) = emoji.shortcode() {
            if short != emoji.name() {
                push(
                    &mut aliases,
                    Alias {
                        name: Cow::Borrowed(short),
                        character: Character::Emoji(emoji),
                        group,
                    },
                );
            }
        }
    }

    for (name, value) in crate::unicode_names::NAMES {
        push(
            &mut aliases,
            Alias {
                name: Cow::Borrowed(name),
                character: Character::Unicode {
                    name,
                    value: char::from_u32(*value).unwrap(),
                },
                group: CharSelectGroup::UnicodeNames,
            },
        );
    }

    for (name, value) in termwiz::nerdfonts::NERD_FONT_GLYPHS {
        push(
            &mut aliases,
            Alias {
                name: Cow::Borrowed(name),
                character: Character::Unicode {
                    name,
                    value: *value,
                },
                group: CharSelectGroup::NerdFonts,
            },
        );
    }

    log::trace!(
        "Took {:?} to build {} aliases",
        start.elapsed(),
        aliases.len()
    );

    aliases
}

#[derive(Debug)]
struct MatchResult {
    row_idx: usize,
    score: i64,
}

impl MatchResult {
    fn new(row_idx: usize, score: i64, selection: &str, aliases: &[Alias]) -> Self {
        Self {
            row_idx,
            score: if aliases[row_idx].name == selection {
                // Pump up the score for an exact match, otherwise
                // the order may be undesirable if there are a lot
                // of candidates with the same score
                i64::max_value()
            } else {
                score
            },
        }
    }
}

fn compute_matches(selection: &str, aliases: &[Alias], group: CharSelectGroup) -> Vec<usize> {
    if selection.is_empty() {
        aliases
            .iter()
            .enumerate()
            .filter(|(_idx, a)| a.group == group)
            .map(|(idx, _a)| idx)
            .collect()
    } else {
        let matcher = SkimMatcherV2::default();

        let numeric_selection = if selection.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(format!("U+{selection}"))
        } else if selection.starts_with("U+") {
            Some(selection.to_string())
        } else {
            None
        };

        let start = std::time::Instant::now();
        let mut scores: Vec<MatchResult> = aliases
            .iter()
            .enumerate()
            .filter_map(|(row_idx, entry)| {
                let alias_result = matcher
                    .fuzzy_match(&entry.name, selection)
                    .map(|score| MatchResult::new(row_idx, score, selection, aliases));
                match &numeric_selection {
                    Some(sel) => {
                        let codepoints = entry.codepoints();
                        if codepoints == *sel {
                            Some(MatchResult {
                                row_idx,
                                score: i64::max_value(),
                            })
                        } else {
                            let number_result = matcher
                                .fuzzy_match(&codepoints, &sel)
                                .map(|score| MatchResult::new(row_idx, score, sel, aliases));

                            match (alias_result, number_result) {
                                (
                                    Some(MatchResult { score: a, .. }),
                                    Some(MatchResult { score: b, .. }),
                                ) => Some(MatchResult {
                                    row_idx,
                                    score: a.max(b),
                                }),
                                (Some(a), None) | (None, Some(a)) => Some(a),
                                (None, None) => None,
                            }
                        }
                    }
                    None => alias_result,
                }
            })
            .collect();
        scores.sort_by(|a, b| a.score.cmp(&b.score).reverse());
        log::trace!("matching took {:?}", start.elapsed());

        scores.iter().map(|result| result.row_idx).collect()
    }
}

impl CharSelector {
    pub fn new(_term_window: &mut TermWindow, args: &CharSelectArguments) -> Self {
        Self {
            element: RefCell::new(None),
            selection: RefCell::new(String::new()),
            group: RefCell::new(args.group),
            aliases: build_aliases(),
            matches: RefCell::new(None),
            selected_row: RefCell::new(0),
            top_row: RefCell::new(0),
            max_rows_on_screen: RefCell::new(0),
        }
    }

    fn compute(
        term_window: &mut TermWindow,
        selection: &str,
        aliases: &[Alias],
        matches: &MatchResults,
        max_rows_on_screen: usize,
        selected_row: usize,
        top_row: usize,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window
            .fonts
            .char_select_font()
            .expect("to resolve char selection font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        let top_bar_height = if term_window.show_tab_bar && !term_window.config.tab_bar_at_bottom {
            term_window.tab_bar_pixel_height().unwrap()
        } else {
            0.
        };
        let (padding_left, padding_top) = term_window.padding_left_top();
        let border = term_window.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;
        let mut elements =
            vec![
                Element::new(&font, ElementContent::Text(format!("Select: {selection}_")))
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: LinearRgba::TRANSPARENT.into(),
                        text: term_window.config.pane_select_fg_color.to_linear().into(),
                    })
                    .display(DisplayType::Block),
            ];

        for (display_idx, alias) in matches
            .matches
            .iter()
            .map(|&idx| &aliases[idx])
            .enumerate()
            .skip(top_row)
            .take(max_rows_on_screen)
        {
            let (bg, text) = if display_idx == selected_row {
                (
                    term_window.config.pane_select_fg_color.to_linear().into(),
                    term_window.config.pane_select_bg_color.to_linear().into(),
                )
            } else {
                (
                    LinearRgba::TRANSPARENT.into(),
                    term_window.config.pane_select_fg_color.to_linear().into(),
                )
            };
            elements.push(
                Element::new(
                    &font,
                    ElementContent::Text(format!(
                        "{} {} ({})",
                        alias.glyph(),
                        alias.name(),
                        alias.codepoints()
                    )),
                )
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg,
                    text,
                })
                .padding(BoxDimension {
                    left: Dimension::Cells(0.25),
                    right: Dimension::Cells(0.25),
                    top: Dimension::Cells(0.),
                    bottom: Dimension::Cells(0.),
                })
                .display(DisplayType::Block),
            );
        }

        let element = Element::new(&font, ElementContent::Children(elements))
            .colors(ElementColors {
                border: BorderColor::new(
                    term_window.config.pane_select_bg_color.to_linear().into(),
                ),
                bg: term_window.config.pane_select_bg_color.to_linear().into(),
                text: term_window.config.pane_select_fg_color.to_linear().into(),
            })
            .margin(BoxDimension {
                left: Dimension::Cells(1.25),
                right: Dimension::Cells(1.25),
                top: Dimension::Cells(1.25),
                bottom: Dimension::Cells(1.25),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.25),
                bottom: Dimension::Cells(0.25),
            })
            .border(BoxDimension::new(Dimension::Pixels(1.)))
            .border_corners(Some(Corners {
                top_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_LEFT_ROUNDED_CORNER,
                },
                top_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_RIGHT_ROUNDED_CORNER,
                },
                bottom_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_LEFT_ROUNDED_CORNER,
                },
                bottom_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_RIGHT_ROUNDED_CORNER,
                },
            }));

        let dimensions = term_window.dimensions;
        let size = term_window.terminal_size;

        let computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: euclid::rect(
                    padding_left,
                    top_pixel_y,
                    size.cols as f32 * term_window.render_metrics.cell_size.width as f32,
                    size.rows as f32 * term_window.render_metrics.cell_size.height as f32,
                ),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &element,
        )?;

        Ok(vec![computed])
    }

    fn updated_input(&self) {
        *self.selected_row.borrow_mut() = 0;
        *self.top_row.borrow_mut() = 0;
    }

    fn move_up(&self) {
        let mut row = self.selected_row.borrow_mut();
        *row = row.saturating_sub(1);

        let mut top_row = self.top_row.borrow_mut();
        if *row < *top_row {
            *top_row = *row;
        }

        log::info!("selected_row={} top_row={}", *row, *top_row);
    }

    fn move_down(&self) {
        let max_rows_on_screen = *self.max_rows_on_screen.borrow();
        let limit = self
            .matches
            .borrow()
            .as_ref()
            .map(|m| m.matches.len())
            .unwrap_or_else(|| self.aliases.len())
            .saturating_sub(1);
        let mut row = self.selected_row.borrow_mut();
        *row = row.saturating_add(1).min(limit);
        let mut top_row = self.top_row.borrow_mut();
        if *row + *top_row > max_rows_on_screen - 1 {
            *top_row = row.saturating_sub(max_rows_on_screen - 1);
        }
        log::info!("selected_row={} top_row={}", *row, *top_row);
    }
}

impl Modal for CharSelector {
    fn perform_assignment(
        &self,
        _assignment: &KeyAssignment,
        _term_window: &mut TermWindow,
    ) -> bool {
        false
    }

    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        Ok(())
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<()> {
        match (key, mods) {
            (KeyCode::Escape, KeyModifiers::NONE) | (KeyCode::Char('g'), KeyModifiers::CTRL) => {
                term_window.cancel_modal();
            }
            (KeyCode::Char('r'), KeyModifiers::CTRL) => {
                // Cycle the selected group
                let mut group = self.group.borrow_mut();
                *group = group.next();
                self.selection.borrow_mut().clear();
                self.updated_input();
            }
            (KeyCode::UpArrow, KeyModifiers::NONE) => {
                self.move_up();
            }
            (KeyCode::DownArrow, KeyModifiers::NONE) => {
                self.move_down();
            }
            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                // Type to add to the selection
                let mut selection = self.selection.borrow_mut();
                selection.push(c);
                self.updated_input();
            }
            (KeyCode::Backspace, KeyModifiers::NONE) => {
                // Backspace to edit the selection
                let mut selection = self.selection.borrow_mut();
                selection.pop();
                self.updated_input();
            }
            (KeyCode::Char('u'), KeyModifiers::CTRL) => {
                // CTRL-u to clear the selection
                let mut selection = self.selection.borrow_mut();
                selection.clear();
                self.updated_input();
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                // Enter the selected character to the current pane
                let selected_idx = *self.selected_row.borrow();
                let alias_idx = self
                    .matches
                    .borrow()
                    .as_ref()
                    .map_or(selected_idx, |m| m.matches[selected_idx]);
                let glyph = self.aliases[alias_idx].glyph();
                log::trace!("selected: {glyph}");
                term_window.copy_to_clipboard(
                    ClipboardCopyDestination::ClipboardAndPrimarySelection,
                    glyph.clone(),
                );
                if let Some(pane) = term_window.get_active_pane_or_overlay() {
                    pane.writer().write_all(glyph.as_bytes()).ok();
                }
                term_window.cancel_modal();
                return Ok(());
            }
            _ => return Ok(()),
        }
        term_window.invalidate_modal();
        Ok(())
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<[ComputedElement]>> {
        let selection = self.selection.borrow();
        let selection = selection.as_str();

        let group = *self.group.borrow();

        let mut results = self.matches.borrow_mut();

        let font = term_window
            .fonts
            .char_select_font()
            .expect("to resolve char selection font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        let max_rows_on_screen = ((term_window.dimensions.pixel_height * 8 / 10)
            / metrics.cell_size.height as usize)
            - 2;
        *self.max_rows_on_screen.borrow_mut() = max_rows_on_screen;

        let rebuild_matches = results
            .as_ref()
            .map(|m| m.selection != selection || m.group != group)
            .unwrap_or(true);
        if rebuild_matches {
            results.replace(MatchResults {
                selection: selection.to_string(),
                matches: compute_matches(selection, &self.aliases, group),
                group,
            });
        };
        let matches = results.as_ref().unwrap();

        if self.element.borrow().is_none() {
            let element = Self::compute(
                term_window,
                selection,
                &self.aliases,
                matches,
                max_rows_on_screen,
                *self.selected_row.borrow(),
                *self.top_row.borrow(),
            )?;
            self.element.borrow_mut().replace(element);
        }
        Ok(Ref::map(self.element.borrow(), |v| {
            v.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }
}
