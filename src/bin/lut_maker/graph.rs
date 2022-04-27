use sensor_analysis::utils::lerp_slice;

use crate::egui::{
    self,
    widgets::plot::{Line, Plot, Points, Value, Values},
    Ui,
};

use crate::AppMode;

#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PreviewMode {
    ToLinear,
    FromLinear,
    ExposureMappings,
}

pub fn graph_ui(ui: &mut Ui, app: &mut crate::AppMain) {
    // "To linear" / "From linear" / "Exposures Plot" view switch.
    ui.horizontal(|ui| {
        ui.label("Preview: ");

        let mode = &mut app.ui_data.lock_mut().preview_mode;
        ui.radio_value(mode, PreviewMode::ToLinear, "To Linear");
        ui.radio_value(mode, PreviewMode::FromLinear, "From Linear");
        ui.radio_value(
            mode,
            PreviewMode::ExposureMappings,
            "Bracketed Exposures Plot",
        );
    });

    let ui_data = app.ui_data.lock();

    let floor = ui_data.sensor_floor;
    let ceiling = ui_data.sensor_ceiling;

    match (ui_data.preview_mode, ui_data.mode) {
        (PreviewMode::ExposureMappings, AppMode::Generate) => {
            // Normalized to-linear luts.
            let luts: Vec<Vec<f32>> = {
                let res = ui_data.transfer_function_resolution;
                let res_norm = 1.0 / (res - 1) as f32;
                (0..3)
                    .map(|chan| {
                        (0..res)
                            .map(|i| {
                                let x = i as f32 * res_norm;
                                ui_data.transfer_function_type.to_linear_fc(
                                    x,
                                    floor[chan],
                                    ceiling[chan],
                                    true,
                                )
                            })
                            .collect()
                    })
                    .collect()
            };

            exposure_mappings_graph(ui, &ui_data, &luts);
        }

        (PreviewMode::ExposureMappings, AppMode::Estimate) => {
            // Normalized to-linear luts.
            let luts: Vec<Vec<f32>> = {
                let simple = [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]];
                let luts = if let Some((luts, _)) = &ui_data.transfer_function_preview {
                    luts
                } else {
                    &simple
                };

                (0..3)
                    .map(|chan| {
                        let out_floor = lerp_slice(&luts[chan], floor[chan]);
                        let out_ceil = lerp_slice(&luts[chan], ceiling[chan]);
                        let out_norm = 1.0 / (out_ceil - out_floor);
                        luts[chan]
                            .iter()
                            .map(|y| (y - out_floor) * out_norm)
                            .collect()
                    })
                    .collect()
            };

            exposure_mappings_graph(ui, &ui_data, &luts);
        }

        (PreviewMode::FromLinear, AppMode::Estimate)
        | (PreviewMode::ToLinear, AppMode::Estimate) => {
            if let Some((luts, err)) = &ui_data.transfer_function_preview {
                let show_from_linear_graph = ui_data.preview_mode == PreviewMode::FromLinear;
                transfer_function_graph(ui, Some(&format!("Average error: {}", err)), |chan| {
                    let out_floor = lerp_slice(&luts[chan], floor[chan]);
                    let out_ceil = lerp_slice(&luts[chan], ceiling[chan]);
                    let out_norm = 1.0 / (out_ceil - out_floor);
                    let x_norm = 1.0 / (luts[chan].len() - 1) as f32;

                    luts[chan].iter().enumerate().map(move |(idx, y)| {
                        if show_from_linear_graph {
                            ((y - out_floor) * out_norm, idx as f32 * x_norm)
                        } else {
                            (idx as f32 * x_norm, (y - out_floor) * out_norm)
                        }
                    })
                });
            } else {
                Plot::new("Transfer Function Graph")
                    .data_aspect(1.0)
                    .show(ui, |plot| {
                        plot.text(egui::widgets::plot::Text::new(
                            egui::widgets::plot::Value { x: 0.5, y: 0.5 },
                            "No estimated transfer function.",
                        ));
                    });
            }
        }

        (PreviewMode::FromLinear, AppMode::Generate) => {
            let normalize = ui_data.normalize_transfer_function;
            let res = ui_data.transfer_function_resolution;
            let res_norm = 1.0 / (res - 1) as f32;
            let function = ui_data.transfer_function_type;

            let range_min = (0..3).fold(std::f32::INFINITY, |a, i| {
                a.min(function.to_linear_fc(0.0, floor[i], ceiling[i], normalize))
            });
            let range_max = (0..3).fold(-std::f32::INFINITY, |a, i| {
                a.max(function.to_linear_fc(1.0, floor[i], ceiling[i], normalize))
            });
            let extent = range_max - range_min;
            transfer_function_graph(ui, None, |chan| {
                (0..res).map(move |i| {
                    let x = range_min + (i as f32 * res_norm * extent);
                    (
                        x,
                        function
                            .from_linear_fc(x, floor[chan], ceiling[chan], normalize)
                            .max(0.0)
                            .min(1.0),
                    )
                })
            });
        }

        (PreviewMode::ToLinear, AppMode::Generate) => {
            let normalize = ui_data.normalize_transfer_function;
            let res = ui_data.transfer_function_resolution;
            let res_norm = 1.0 / (res - 1) as f32;
            let function = ui_data.transfer_function_type;

            transfer_function_graph(ui, None, |chan| {
                (0..res).map(move |i| {
                    let x = i as f32 * res_norm;
                    (
                        x,
                        function.to_linear_fc(x, floor[chan], ceiling[chan], normalize),
                    )
                })
            });
        }

        (_, AppMode::Modify) => todo!(),
    }
}

fn exposure_mappings_graph(ui: &mut Ui, ui_data: &crate::UIData, luts: &Vec<Vec<f32>>) {
    // The graph plot.
    Plot::new("Exposure mappings Graph")
        .data_aspect(1.0)
        .show(ui, |plot| {
            if ui_data.exposure_mappings[0].is_empty() {
                plot.text(egui::widgets::plot::Text::new(
                    egui::widgets::plot::Value { x: 0.5, y: 0.5 },
                    "Two or more bracketed exposure images needed to generate data.",
                ));
            } else {
                plot.line(
                    Line::new(Values::from_values_iter(
                        [Value::new(0.0, 0.0), Value::new(1.0, 1.0)].iter().copied(),
                    ))
                    .color(lib::colors::GRAY),
                );
                for chan in 0..3 {
                    plot.points(Points::new(Values::from_values_iter(
                        ui_data.exposure_mappings[chan]
                            .iter()
                            .map(|m| {
                                let norm = m.exposure_ratio;
                                m.curve.iter().map(move |(x, y)| {
                                    Value::new(
                                        lerp_slice(&luts[chan], *x) * norm,
                                        lerp_slice(&luts[chan], *y),
                                    )
                                })
                            })
                            .flatten(),
                    )));
                }
            }
        });
}

fn transfer_function_graph<I: Iterator<Item = (f32, f32)>, F: Fn(usize) -> I>(
    ui: &mut Ui,
    label: Option<&str>,
    channel_points: F,
) {
    let colors = &[lib::colors::RED, lib::colors::GREEN, lib::colors::BLUE];

    Plot::new("Transfer Function Graph")
        .data_aspect(1.0)
        .show(ui, |plot| {
            if let Some(text) = label {
                plot.text(egui::widgets::plot::Text::new(
                    egui::widgets::plot::Value { x: 0.5, y: -0.05 },
                    text,
                ));
            }
            for chan in 0..3 {
                plot.line(
                    Line::new(Values::from_values_iter(
                        channel_points(chan).map(|(x, y)| Value::new(x, y)),
                    ))
                    .color(colors[chan]),
                );
            }
        });
}
