/// Fast Fourier Transform (FFT) related functionality and state management.
use egui::{Context, Ui, Vec2};
use egui_plot::{Legend, Line, Plot, PlotPoints, Points};
use egui_remixicon::icons;
use enum_iterator::Sequence;
use num::{BigInt, BigUint, ToPrimitive, Zero};
use rustfft::{FftPlanner, num_complex::Complex};
use std::sync::Arc;

use crate::SystemState;
use crate::displayed_item::{DisplayedFieldRef, DisplayedItem, DisplayedItemRef};
use crate::message::Message;
use crate::time::{TimeInputState, TimeUnit};
use crate::translation::TranslatorList;
use crate::wave_container::VariableRefExt;
use crate::wave_data::WaveData;

/// Number of samples used to initialize the default sample interval.
const DEFAULT_SAMPLE_COUNT: BigInt = BigInt::new_const(1024);
/// Upper bound for the combined input samples and zero padding.
const MAX_FFT_SAMPLES: usize = 1_048_576;
/// Floor used when converting a zero or very small magnitude to decibels.
const MIN_MAGNITUDE_DB: f64 = -120.0;

#[derive(Clone, Copy, PartialEq, Eq, Sequence)]
/// Selects the pair of plots used for the frequency-domain result.
enum FrequencyPlot {
    MagnitudePhase,
    RealImaginary,
}

impl FrequencyPlot {
    fn label(self) -> &'static str {
        match self {
            Self::MagnitudePhase => "Magnitude / phase",
            Self::RealImaginary => "Real / imaginary",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Sequence)]
/// Selects how timestamps for the FFT input samples are generated.
enum SamplingMode {
    /// Use a uniformly spaced, user-provided sample interval.
    SampleTime,
    /// Sample on every configured rising edge, optionally skipping edges.
    RisingEdge,
    /// Sample on every configured falling edge, optionally skipping edges.
    FallingEdge,
}

impl SamplingMode {
    fn label(self) -> &'static str {
        match self {
            Self::SampleTime => "Sample time",
            Self::RisingEdge => "Rising edge",
            Self::FallingEdge => "Falling edge",
        }
    }

    fn edge_label(self) -> &'static str {
        match self {
            Self::RisingEdge => "rising edge",
            Self::FallingEdge => "falling edge",
            Self::SampleTime => unreachable!(),
        }
    }
}

/// Runtime-only configuration and presentation state for the FFT window.
pub(crate) struct FftState {
    start: TimeInputState,
    stop: TimeInputState,
    sample_time: TimeInputState,
    sampling_mode: SamplingMode,
    clock_signal: Option<DisplayedItemRef>,
    /// Offset applied to raw clock-edge timestamps before sampling signals.
    clock_offset: TimeInputState,
    /// Retain one matching edge out of this many in edge-triggered modes.
    edge_multiple: usize,
    /// Number of zero-valued samples appended to the input before the FFT.
    zero_padding: usize,
    real_signal: Option<DisplayedItemRef>,
    imaginary_signal: Option<DisplayedItemRef>,
    frequency_plot: FrequencyPlot,
    show_magnitude: bool,
    magnitude_db: bool,
    magnitude_stems: bool,
    show_phase: bool,
    unwrap_phase: bool,
    frequency_stems: bool,
    show_time: bool,
    time_stems: bool,
    cached_spectrum: Option<SpectrumCache>,
}

impl Default for FftState {
    fn default() -> Self {
        Self {
            start: TimeInputState::default(),
            stop: TimeInputState::default(),
            sample_time: TimeInputState::default(),
            sampling_mode: SamplingMode::SampleTime,
            clock_signal: None,
            clock_offset: TimeInputState::default(),
            edge_multiple: 1,
            zero_padding: 0,
            real_signal: None,
            imaginary_signal: None,
            frequency_plot: FrequencyPlot::MagnitudePhase,
            show_magnitude: true,
            magnitude_db: false,
            magnitude_stems: false,
            show_phase: true,
            unwrap_phase: false,
            frequency_stems: false,
            show_time: true,
            time_stems: false,
            cached_spectrum: None,
        }
    }
}

impl FftState {
    fn initialize(
        &mut self,
        waves: &WaveData,
        options: &[SignalOption],
        clock_options: &[SignalOption],
    ) {
        let timescale = waves.inner.metadata().timescale.clone();
        let stop = waves.safe_max_timestamp();
        let sample_time = if stop.clone() > DEFAULT_SAMPLE_COUNT {
            stop.clone() / DEFAULT_SAMPLE_COUNT
        } else {
            BigInt::ONE
        };

        if self.start.is_empty() {
            self.start.set_timescale_ticks(&BigInt::ZERO, &timescale);
        }
        if self.stop.is_empty() {
            self.stop.set_timescale_ticks(&stop, &timescale);
        }
        if self.sample_time.is_empty() {
            self.sample_time
                .set_timescale_ticks(&sample_time, &timescale);
        }
        if self.clock_offset.is_empty() {
            self.clock_offset
                .set_timescale_ticks(&BigInt::ZERO, &timescale);
        }
        self.edge_multiple = self.edge_multiple.max(1);

        if !options
            .iter()
            .any(|option| Some(option.id) == self.real_signal)
        {
            self.real_signal = options.first().map(|option| option.id);
        }
        if self
            .imaginary_signal
            .is_some_and(|id| !options.iter().any(|option| option.id == id))
        {
            self.imaginary_signal = None;
        }
        if !clock_options
            .iter()
            .any(|option| Some(option.id) == self.clock_signal)
        {
            self.clock_signal = clock_options.first().map(|option| option.id);
        }
    }

    pub(crate) fn invalidate_spectrum_cache(&mut self) {
        self.cached_spectrum = None;
    }
}

/// A visible signal that can be selected by the FFT controls.
struct SignalOption {
    id: DisplayedItemRef,
    name: String,
}

/// Sampled input data and the frequency-domain results used by the plots.
struct Spectrum {
    sample_times: Vec<f64>,
    real_samples: Vec<f64>,
    imaginary_samples: Option<Vec<f64>>,
    fft_real: Vec<[f64; 2]>,
    fft_imaginary: Vec<[f64; 2]>,
    magnitude: Vec<[f64; 2]>,
    phase: Vec<[f64; 2]>,
    sample_count: usize,
    fft_length: usize,
    time_scale: AxisScale,
    frequency_scale: AxisScale,
}

#[derive(Clone, PartialEq, Eq)]
struct SpectrumCacheKey {
    start: Option<BigInt>,
    stop: Option<BigInt>,
    sample_time: Option<BigInt>,
    sampling_mode: SamplingMode,
    clock_signal: Option<DisplayedItemRef>,
    clock_offset: Option<BigInt>,
    edge_multiple: usize,
    zero_padding: usize,
    real_signal: Option<DisplayedItemRef>,
    imaginary_signal: Option<DisplayedItemRef>,
    timescale_unit: TimeUnit,
    timescale_multiplier: Option<u32>,
    real_translator: Option<String>,
    imaginary_translator: Option<String>,
}

struct SpectrumCache {
    key: SpectrumCacheKey,
    spectrum: Arc<Spectrum>,
}

impl SpectrumCacheKey {
    fn new(waves: &WaveData, translators: &TranslatorList, state: &FftState) -> Self {
        let timescale = waves.inner.metadata().timescale.clone();
        let translator_name = |item_ref: Option<DisplayedItemRef>| {
            item_ref.and_then(|item_ref| {
                matches!(
                    waves.displayed_items.get(&item_ref),
                    Some(DisplayedItem::Variable(_))
                )
                .then(|| {
                    waves
                        .variable_translator(&DisplayedFieldRef::from(item_ref), translators)
                        .name()
                        .to_owned()
                })
            })
        };

        Self {
            start: state.start.to_timescale_ticks(&timescale),
            stop: state.stop.to_timescale_ticks(&timescale),
            sample_time: state.sample_time.to_timescale_ticks(&timescale),
            sampling_mode: state.sampling_mode,
            clock_signal: state.clock_signal,
            clock_offset: state.clock_offset.to_timescale_ticks(&timescale),
            edge_multiple: state.edge_multiple.max(1),
            zero_padding: state.zero_padding,
            real_signal: state.real_signal,
            imaginary_signal: state.imaginary_signal,
            timescale_unit: timescale.unit,
            timescale_multiplier: timescale.multiplier,
            real_translator: translator_name(state.real_signal),
            imaginary_translator: translator_name(state.imaginary_signal),
        }
    }
}

impl SystemState {
    pub(crate) fn draw_fft_window(&mut self, ctx: &Context, msgs: &mut Vec<Message>) {
        let Some(waves) = self.user.waves.as_ref() else {
            return;
        };

        let options = signal_options(waves);
        let clock_options = binary_signal_options(waves);
        let mut fft_state = std::mem::take(&mut self.fft);
        fft_state.initialize(waves, &options, &clock_options);
        let mut open = true;

        egui::Window::new("FFT")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .min_size((600., 400.))
            .default_size((900., 650.))
            .show(ctx, |ui| {
                self.draw_fft_contents(ui, waves, &options, &clock_options, &mut fft_state, msgs);
            });

        self.fft = fft_state;
        if !open {
            msgs.push(Message::SetFftVisible(false));
        }
    }

    fn draw_fft_contents(
        &self,
        ui: &mut Ui,
        waves: &WaveData,
        options: &[SignalOption],
        clock_options: &[SignalOption],
        state: &mut FftState,
        msgs: &mut Vec<Message>,
    ) {
        let mut zoom_to_fit = false;

        ui.horizontal_wrapped(|ui| {
            ui.label("Real signal");
            signal_combo(
                ui,
                "fft-real-signal",
                &mut state.real_signal,
                options,
                false,
            );
            ui.label("Imaginary signal");
            signal_combo(
                ui,
                "fft-imaginary-signal",
                &mut state.imaginary_signal,
                options,
                true,
            );
        });

        ui.horizontal_wrapped(|ui| {
            draw_time_input_row(
                self,
                ui,
                "Start",
                "fft-start",
                waves,
                &mut state.start,
                msgs,
                true,
            );
            draw_time_input_row(
                self,
                ui,
                "Stop",
                "fft-stop",
                waves,
                &mut state.stop,
                msgs,
                true,
            );
        });

        ui.horizontal_wrapped(|ui| {
            ui.horizontal(|ui| {
                ui.label("Sampling");
                egui::ComboBox::from_id_salt("fft-sampling-mode")
                    .selected_text(state.sampling_mode.label())
                    .show_ui(ui, |ui| {
                        for sampling_mode in enum_iterator::all::<SamplingMode>() {
                            ui.selectable_value(
                                &mut state.sampling_mode,
                                sampling_mode,
                                sampling_mode.label(),
                            );
                        }
                    });
                egui::CollapsingHeader::new("Configure")
                    .default_open(false)
                    .show(ui, |ui| match state.sampling_mode {
                        SamplingMode::SampleTime => {
                            draw_time_input_row(
                                self,
                                ui,
                                "Sample time",
                                "fft-sample-time",
                                waves,
                                &mut state.sample_time,
                                msgs,
                                false,
                            );
                        }
                        SamplingMode::RisingEdge | SamplingMode::FallingEdge => {
                            ui.horizontal(|ui| {
                                ui.label("Clock signal");
                                signal_combo(
                                    ui,
                                    "fft-clock-signal",
                                    &mut state.clock_signal,
                                    clock_options,
                                    false,
                                );
                            });
                            draw_time_input_row(
                                self,
                                ui,
                                "Time offset",
                                "fft-clock-offset",
                                waves,
                                &mut state.clock_offset,
                                msgs,
                                false,
                            );
                            ui.horizontal(|ui| {
                                ui.label("Every");
                                ui.add(
                                    egui::DragValue::new(&mut state.edge_multiple)
                                        .range(1..=MAX_FFT_SAMPLES)
                                        .speed(1),
                                );
                                ui.label(state.sampling_mode.edge_label());
                            });
                        }
                    });
            });
            ui.label("Zero padding");
            ui.add(
                egui::DragValue::new(&mut state.zero_padding)
                    .range(0..=MAX_FFT_SAMPLES)
                    .speed(1),
            );
        });

        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut state.show_time, "Time");
            ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                ui.checkbox(&mut state.time_stems, "Stem plot");
            })
            .response
            .on_hover_text("Time plot settings");
            ui.label("Frequency plot");
            egui::ComboBox::from_id_salt("fft-frequency-plot")
                .selected_text(state.frequency_plot.label())
                .show_ui(ui, |ui| {
                    for frequency_plot in enum_iterator::all::<FrequencyPlot>() {
                        ui.selectable_value(
                            &mut state.frequency_plot,
                            frequency_plot,
                            frequency_plot.label(),
                        );
                    }
                });
            match state.frequency_plot {
                FrequencyPlot::MagnitudePhase => {
                    ui.checkbox(&mut state.show_magnitude, "Magnitude");
                    ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                        ui.checkbox(&mut state.magnitude_db, "Magnitude in dB");
                        ui.checkbox(&mut state.magnitude_stems, "Stem plot");
                    })
                    .response
                    .on_hover_text("Magnitude plot settings");
                    ui.checkbox(&mut state.show_phase, "Phase");
                    ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                        ui.checkbox(&mut state.unwrap_phase, "Unwrap phase");
                    })
                    .response
                    .on_hover_text("Phase plot settings");
                }
                FrequencyPlot::RealImaginary => {
                    ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                        ui.checkbox(&mut state.frequency_stems, "Stem plot");
                    })
                    .response
                    .on_hover_text("Frequency plot settings");
                }
            }
        });

        ui.separator();

        match calculate_spectrum(waves, &self.translators, state) {
            Ok(spectrum) => {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} samples, FFT length {}",
                        spectrum.sample_count, spectrum.fft_length
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("Zoom to fit")
                            .on_hover_text("Reset the visible plots to fit all FFT bins")
                            .clicked()
                        {
                            zoom_to_fit = true;
                        }
                    });
                });
                let frequency_plot_count = match state.frequency_plot {
                    FrequencyPlot::MagnitudePhase => {
                        usize::from(state.show_magnitude) + usize::from(state.show_phase)
                    }
                    FrequencyPlot::RealImaginary => 1,
                };
                if !state.show_time && frequency_plot_count == 0 {
                    ui.label("Select a plot to display.");
                } else {
                    let plot_count = usize::from(state.show_time) + frequency_plot_count;
                    let inter_plot_spacing = ui.spacing().item_spacing.y;
                    let plot_height = ((ui.available_height()
                        - inter_plot_spacing * plot_count.saturating_sub(1) as f32)
                        / plot_count as f32)
                        .max(1.0);
                    if state.show_time {
                        draw_time_plot(ui, &spectrum, state, plot_height, zoom_to_fit);
                    }

                    match state.frequency_plot {
                        FrequencyPlot::MagnitudePhase => {
                            draw_magnitude_phase_plots(
                                ui,
                                &spectrum,
                                state,
                                plot_height,
                                zoom_to_fit,
                            );
                        }
                        FrequencyPlot::RealImaginary => {
                            draw_real_imaginary_plot(
                                ui,
                                &spectrum,
                                state,
                                plot_height,
                                zoom_to_fit,
                            );
                        }
                    }
                }
            }
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        }
    }
}

fn draw_time_plot(
    ui: &mut Ui,
    spectrum: &Spectrum,
    state: &FftState,
    plot_height: f32,
    zoom_to_fit: bool,
) {
    let real_points = spectrum
        .sample_times
        .iter()
        .copied()
        .zip(spectrum.real_samples.iter().copied())
        .map(|(time, value)| [time, value])
        .collect::<Vec<_>>();
    let imaginary_points = spectrum.imaginary_samples.as_ref().map(|samples| {
        spectrum
            .sample_times
            .iter()
            .copied()
            .zip(samples.iter().copied())
            .map(|(time, value)| [time, value])
            .collect::<Vec<_>>()
    });
    let mut time_plot = Plot::new("fft-time");
    if zoom_to_fit {
        time_plot = time_plot.reset();
    }
    time_plot = configure_plot(time_plot, spectrum.time_scale, None);
    time_plot
        .height(plot_height)
        .legend(Legend::default())
        .show_axes([true, true])
        .show_grid([true, true])
        .x_axis_label(format!(
            "Time from start ({})",
            spectrum.time_scale.unit_label("s")
        ))
        .y_axis_label("Sample value")
        .show(ui, |plot_ui| {
            if state.time_stems {
                plot_ui.points(
                    Points::new("Real", PlotPoints::new(real_points))
                        .stems(0.0)
                        .radius(1.5),
                );
                if let Some(imaginary_points) = imaginary_points {
                    plot_ui.points(
                        Points::new("Imaginary", PlotPoints::new(imaginary_points))
                            .stems(0.0)
                            .radius(1.5),
                    );
                }
            } else {
                plot_ui.line(Line::new("Real", PlotPoints::new(real_points)));
                if let Some(imaginary_points) = imaginary_points {
                    plot_ui.line(Line::new("Imaginary", PlotPoints::new(imaginary_points)));
                }
            }
        });
}

fn draw_magnitude_phase_plots(
    ui: &mut Ui,
    spectrum: &Spectrum,
    state: &FftState,
    plot_height: f32,
    zoom_to_fit: bool,
) {
    let magnitude_label = if state.magnitude_db {
        "Magnitude (dB)"
    } else {
        "Magnitude"
    };
    if state.show_magnitude {
        let magnitude_points = spectrum
            .magnitude
            .iter()
            .map(|[frequency, magnitude]| {
                [
                    *frequency,
                    if state.magnitude_db {
                        magnitude_to_db(*magnitude)
                    } else {
                        *magnitude
                    },
                ]
            })
            .collect::<Vec<_>>();
        let mut magnitude_plot = Plot::new("fft-magnitude");
        if zoom_to_fit {
            magnitude_plot = magnitude_plot.reset();
        }
        magnitude_plot = configure_plot(
            magnitude_plot,
            spectrum.frequency_scale,
            frequency_bounds(spectrum),
        );
        magnitude_plot
            .height(plot_height)
            .legend(Legend::default())
            .show_axes([true, true])
            .show_grid([true, true])
            .x_axis_label(format!(
                "Frequency ({})",
                spectrum.frequency_scale.unit_label("Hz")
            ))
            .y_axis_label(magnitude_label)
            .show(ui, |plot_ui| {
                if state.magnitude_stems {
                    let stem_reference = if state.magnitude_db {
                        MIN_MAGNITUDE_DB as f32
                    } else {
                        0.0
                    };
                    plot_ui.points(
                        Points::new(magnitude_label, PlotPoints::new(magnitude_points))
                            .stems(stem_reference)
                            .radius(1.5),
                    );
                } else {
                    plot_ui.line(Line::new(
                        magnitude_label,
                        PlotPoints::new(magnitude_points),
                    ));
                }
            });
    }

    if state.show_phase {
        let phase_points = if state.unwrap_phase {
            unwrap_phase(&spectrum.phase)
        } else {
            spectrum.phase.clone()
        };
        let phase_label = if state.unwrap_phase {
            "Unwrapped phase (rad)"
        } else {
            "Phase (rad)"
        };
        let mut phase_plot = Plot::new("fft-phase");
        if zoom_to_fit {
            phase_plot = phase_plot.reset();
        }
        phase_plot = configure_plot(
            phase_plot,
            spectrum.frequency_scale,
            frequency_bounds(spectrum),
        );
        phase_plot
            .height(plot_height)
            .legend(Legend::default())
            .show_axes([true, true])
            .show_grid([true, true])
            .x_axis_label(format!(
                "Frequency ({})",
                spectrum.frequency_scale.unit_label("Hz")
            ))
            .y_axis_label(phase_label)
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new(phase_label, PlotPoints::new(phase_points)));
            });
    }
}

fn draw_real_imaginary_plot(
    ui: &mut Ui,
    spectrum: &Spectrum,
    state: &FftState,
    plot_height: f32,
    zoom_to_fit: bool,
) {
    let mut frequency_plot = Plot::new("fft-real-imaginary");
    if zoom_to_fit {
        frequency_plot = frequency_plot.reset();
    }
    frequency_plot = configure_plot(
        frequency_plot,
        spectrum.frequency_scale,
        frequency_bounds(spectrum),
    );
    frequency_plot
        .height(plot_height)
        .legend(Legend::default())
        .show_axes([true, true])
        .show_grid([true, true])
        .x_axis_label(format!(
            "Frequency ({})",
            spectrum.frequency_scale.unit_label("Hz")
        ))
        .y_axis_label("FFT value")
        .show(ui, |plot_ui| {
            if state.frequency_stems {
                plot_ui.points(
                    Points::new("Real", PlotPoints::new(spectrum.fft_real.clone()))
                        .stems(0.0)
                        .radius(1.5),
                );
                plot_ui.points(
                    Points::new("Imaginary", PlotPoints::new(spectrum.fft_imaginary.clone()))
                        .stems(0.0)
                        .radius(1.5),
                );
            } else {
                plot_ui.line(Line::new(
                    "Real",
                    PlotPoints::new(spectrum.fft_real.clone()),
                ));
                plot_ui.line(Line::new(
                    "Imaginary",
                    PlotPoints::new(spectrum.fft_imaginary.clone()),
                ));
            }
        });
}

#[cfg(test)]
impl SystemState {
    pub(crate) fn configure_fft_for_snapshot(&mut self, variant: usize) {
        let Some(waves) = self.user.waves.as_ref() else {
            panic!("FFT snapshot requires a waveform source");
        };
        let timescale = waves.inner.metadata().timescale.clone();
        let options = signal_options(waves);
        let real_signal = options
            .iter()
            .find(|option| option.name == "top.sine_real")
            .map(|option| option.id)
            .expect("FFT snapshot signal top.sine_real is not visible");
        let imaginary_signal = if variant == 4 {
            options
                .iter()
                .find(|option| option.name == "top.sine_real" && option.id != real_signal)
                .map(|option| option.id)
                .expect("FFT complex snapshot requires two top.sine_real signals")
        } else {
            real_signal
        };

        let (start, stop, sample_time) = match variant {
            0 => (0_i64, 100_000_i64, 1_000_i64),
            1 => (10_000_i64, 90_000_i64, 2_000_i64),
            2 => (0_i64, 60_000_i64, 500_i64),
            3 => (0_i64, 100_000_i64, 1_000_i64),
            4 => (20_000_i64, 80_000_i64, 1_000_i64),
            _ => panic!("Unknown FFT snapshot variant: {variant}"),
        };

        self.fft
            .start
            .set_timescale_ticks(&BigInt::from(start), &timescale);
        self.fft
            .stop
            .set_timescale_ticks(&BigInt::from(stop), &timescale);
        self.fft
            .sample_time
            .set_timescale_ticks(&BigInt::from(sample_time), &timescale);
        self.fft.sampling_mode = SamplingMode::SampleTime;
        self.fft.real_signal = Some(real_signal);
        self.fft.imaginary_signal = (variant == 4).then_some(imaginary_signal);
        self.fft.frequency_plot = FrequencyPlot::MagnitudePhase;
        self.fft.show_time = true;
        self.fft.time_stems = false;
        self.fft.show_magnitude = true;
        self.fft.magnitude_db = false;
        self.fft.magnitude_stems = false;
        self.fft.show_phase = true;
        self.fft.unwrap_phase = false;
        self.fft.frequency_stems = false;
        self.fft.zero_padding = 0;

        match variant {
            1 => {
                self.fft.time_stems = true;
                self.fft.magnitude_stems = true;
            }
            2 => {
                self.fft.magnitude_db = true;
                self.fft.magnitude_stems = true;
                self.fft.unwrap_phase = true;
            }
            3 => {
                self.fft.frequency_plot = FrequencyPlot::RealImaginary;
                self.fft.frequency_stems = true;
                self.fft.show_time = false;
                self.fft.zero_padding = 99;
            }
            4 => {
                self.fft.show_magnitude = false;
                self.fft.time_stems = true;
                self.fft.unwrap_phase = true;
            }
            _ => {}
        }
    }
}

fn magnitude_to_db(magnitude: f64) -> f64 {
    if magnitude > 0.0 {
        (20.0 * magnitude.log10()).max(MIN_MAGNITUDE_DB)
    } else {
        MIN_MAGNITUDE_DB
    }
}

fn unwrap_phase(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut unwrapped = Vec::with_capacity(points.len());
    let mut phase_offset = 0.0;
    let mut previous_phase = None;

    for [frequency, phase] in points {
        if let Some(previous_phase) = previous_phase {
            let difference = phase - previous_phase;
            if difference > std::f64::consts::PI {
                phase_offset -= std::f64::consts::TAU;
            } else if difference < -std::f64::consts::PI {
                phase_offset += std::f64::consts::TAU;
            }
        }

        unwrapped.push([*frequency, phase + phase_offset]);
        previous_phase = Some(*phase);
    }

    unwrapped
}

fn signal_options(waves: &WaveData) -> Vec<SignalOption> {
    waves
        .items_tree
        .iter_visible()
        .filter_map(|node| match waves.displayed_items.get(&node.item_ref) {
            Some(DisplayedItem::Variable(variable)) if variable.analog.is_some() => {
                Some(SignalOption {
                    id: node.item_ref,
                    name: variable.variable_ref.full_path_string(),
                })
            }
            _ => None,
        })
        .collect()
}

fn binary_signal_options(waves: &WaveData) -> Vec<SignalOption> {
    let Some(wave_container) = waves.inner.as_waves() else {
        return vec![];
    };

    waves
        .items_tree
        .iter_visible()
        .filter_map(|node| match waves.displayed_items.get(&node.item_ref) {
            Some(DisplayedItem::Variable(variable)) => {
                let is_binary = wave_container
                    .variable_meta(&variable.variable_ref)
                    .ok()
                    .is_some_and(|meta| meta.is_bit());
                is_binary.then(|| SignalOption {
                    id: node.item_ref,
                    name: variable.variable_ref.full_path_string(),
                })
            }
            _ => None,
        })
        .collect()
}

fn signal_combo(
    ui: &mut Ui,
    id: &str,
    selected: &mut Option<DisplayedItemRef>,
    options: &[SignalOption],
    allow_none: bool,
) {
    let selected_text = selected
        .and_then(|id| options.iter().find(|option| option.id == id))
        .map_or_else(
            || {
                if allow_none {
                    "None".to_string()
                } else {
                    "Select a signal".to_string()
                }
            },
            |option| option.name.clone(),
        );

    egui::ComboBox::from_id_salt(id)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            if allow_none {
                ui.selectable_value(selected, None, "None");
            }
            for option in options {
                ui.selectable_value(selected, Some(option.id), &option.name);
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn draw_time_input_row(
    state_owner: &SystemState,
    ui: &mut Ui,
    label: &str,
    id: &str,
    waves: &WaveData,
    state: &mut TimeInputState,
    msgs: &mut Vec<Message>,
    show_set_from: bool,
) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(label);
            state_owner.time_input_controls(ui, state, id, waves, msgs, None);
            if show_set_from {
                ui.menu_button("Set from", |ui| {
                    let timescale = waves.inner.metadata().timescale.clone();
                    if let Some(cursor) = waves.cursor.as_ref()
                        && ui.button("Cursor").clicked()
                    {
                        state.set_timescale_ticks(cursor, &timescale);
                        ui.close();
                    }

                    let mut markers = waves
                        .markers
                        .iter()
                        .map(|(idx, time)| (*idx, time.clone(), marker_label(waves, *idx)))
                        .collect::<Vec<_>>();
                    markers.sort_by_key(|(idx, _, _)| *idx);
                    for (_, time, marker) in markers {
                        if ui.button(marker).clicked() {
                            state.set_timescale_ticks(&time, &timescale);
                            ui.close();
                        }
                    }
                });
            }
        });
        if let Some(error) = state.error_message() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    });
}

fn marker_label(waves: &WaveData, idx: u8) -> String {
    let name = waves.displayed_items.values().find_map(|item| match item {
        DisplayedItem::Marker(marker) if marker.idx == idx => marker.name.as_deref(),
        _ => None,
    });
    name.map_or_else(
        || format!("Marker {idx}"),
        |name| format!("Marker {idx} ({name})"),
    )
}

#[derive(Clone, Copy)]
struct AxisScale {
    factor: f64,
    prefix: &'static str,
}

impl AxisScale {
    fn unit_label(self, base_unit: &str) -> String {
        format!("{}{base_unit}", self.prefix)
    }
}

fn axis_scale_for_range(range: Option<(f64, f64)>) -> AxisScale {
    let maximum = range
        .map(|(min, max)| min.abs().max(max.abs()))
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0);
    let exponent = ((maximum.log10() as i32).div_euclid(3) * 3).clamp(-21, 12);

    AxisScale {
        factor: 10.0_f64.powi(exponent),
        prefix: match exponent {
            -21 => "z",
            -18 => "a",
            -15 => "f",
            -12 => "p",
            -9 => "n",
            -6 => "μ",
            -3 => "m",
            0 => "",
            3 => "k",
            6 => "M",
            9 => "G",
            12 => "T",
            _ => unreachable!("engineering exponent is clamped to a supported prefix"),
        },
    }
}

fn format_axis_tick(value: f64, step_size: f64, scale: AxisScale) -> String {
    if !value.is_finite() {
        return value.to_string();
    }

    let scaled_value = value / scale.factor;
    let mut scaled_step = step_size.abs() / scale.factor;
    let mut precision = 0;
    while precision < 6 && (scaled_step - scaled_step.round()).abs() > 1e-9 {
        scaled_step *= 10.0;
        precision += 1;
    }

    let mut formatted = format!("{scaled_value:.precision$}");
    if let Some(decimal_point) = formatted.find('.') {
        while formatted.ends_with('0') {
            formatted.pop();
        }
        if formatted.len() == decimal_point + 1 {
            formatted.pop();
        }
    }
    if formatted == "-0" {
        formatted.clear();
        formatted.push('0');
    }
    formatted
}

fn configure_plot(
    mut plot: Plot,
    x_scale: AxisScale,
    frequency_bounds: Option<(f64, f64)>,
) -> Plot {
    plot = plot.set_margin_fraction(Vec2::new(0.01, 0.05));
    plot = plot.x_axis_formatter(move |mark, _range| {
        format_axis_tick(mark.value, mark.step_size, x_scale)
    });
    if let Some((min_frequency, max_frequency)) = frequency_bounds {
        let padding = (max_frequency - min_frequency) * 0.01;
        plot = plot.default_x_bounds(min_frequency - padding, max_frequency + padding);
    }
    plot
}

fn frequency_range(spectrum: &Spectrum) -> Option<(f64, f64)> {
    spectrum
        .magnitude
        .first()
        .zip(spectrum.magnitude.last())
        .map(|([min_frequency, _], [max_frequency, _])| (*min_frequency, *max_frequency))
}

fn frequency_bounds(spectrum: &Spectrum) -> Option<(f64, f64)> {
    if spectrum.imaginary_samples.is_none() {
        return None;
    }

    let (min_frequency, max_frequency) = frequency_range(spectrum)?;
    (min_frequency < max_frequency).then_some((min_frequency, max_frequency))
}

/// Build the sampled input and its FFT according to the current window settings.
///
/// Edge-triggered sampling finds clock transitions in the raw range obtained by
/// subtracting the configured offset. The selected edge timestamps are then shifted
/// forward by that offset before the real and optional imaginary signals are queried.
/// The first selected interval supplies the frequency spacing used by the FFT plots.
fn calculate_spectrum(
    waves: &WaveData,
    translators: &TranslatorList,
    state: &mut FftState,
) -> Result<Arc<Spectrum>, String> {
    let key = SpectrumCacheKey::new(waves, translators, state);
    if let Some(cache) = state.cached_spectrum.as_ref()
        && cache.key == key
    {
        return Ok(Arc::clone(&cache.spectrum));
    }

    let spectrum = Arc::new(build_spectrum(waves, translators, state)?);
    state.cached_spectrum = Some(SpectrumCache {
        key,
        spectrum: Arc::clone(&spectrum),
    });
    Ok(spectrum)
}

fn build_spectrum(
    waves: &WaveData,
    translators: &TranslatorList,
    state: &FftState,
) -> Result<Spectrum, String> {
    let timescale = waves.inner.metadata().timescale.clone();
    let start = state
        .start
        .to_timescale_ticks(&timescale)
        .and_then(|value| BigUint::try_from(value).ok())
        .ok_or_else(|| "Enter a valid non-negative start time.".to_string())?;
    let stop = state
        .stop
        .to_timescale_ticks(&timescale)
        .and_then(|value| BigUint::try_from(value).ok())
        .ok_or_else(|| "Enter a valid non-negative stop time.".to_string())?;
    if stop < start {
        return Err("Stop time must not be before start time.".to_string());
    }

    let (sample_ticks, sample_interval) = match state.sampling_mode {
        SamplingMode::SampleTime => {
            let sample_time = state
                .sample_time
                .to_timescale_ticks(&timescale)
                .and_then(|value| BigUint::try_from(value).ok())
                .ok_or_else(|| "Enter a valid positive sample time.".to_string())?;
            if sample_time.is_zero() {
                return Err("Sample time must be greater than zero.".to_string());
            }

            let sample_count = (&stop - &start) / &sample_time + BigUint::from(1u8);
            let fft_length = &sample_count + BigUint::from(state.zero_padding);
            if fft_length > BigUint::from(MAX_FFT_SAMPLES) {
                return Err(format!(
                    "FFT length must be between 1 and {MAX_FFT_SAMPLES} samples."
                ));
            }
            let sample_count = sample_count
                .to_usize()
                .ok_or_else(|| "The selected range contains too many samples.".to_string())?;
            let sample_ticks = (0..sample_count)
                .map(|index| &start + &sample_time * BigUint::from(index))
                .collect::<Vec<_>>();
            (sample_ticks, sample_time)
        }
        SamplingMode::RisingEdge | SamplingMode::FallingEdge => {
            let Some(clock_signal) = state.clock_signal else {
                return Err("Select a clock signal.".to_string());
            };
            let Some(DisplayedItem::Variable(clock_variable)) =
                waves.displayed_items.get(&clock_signal)
            else {
                return Err("The selected clock is not a signal.".to_string());
            };
            let offset = state
                .clock_offset
                .to_timescale_ticks(&timescale)
                .ok_or_else(|| "Enter a valid time offset.".to_string())?;
            // Query the clock in raw time, then shift selected edges into the FFT range.
            let raw_start = (BigInt::from(start.clone()) - &offset).max(BigInt::ZERO);
            let raw_stop = BigInt::from(stop.clone()) - &offset;
            let raw_start = BigUint::try_from(raw_start)
                .map_err(|_| "The time offset is outside the supported range.".to_string())?;
            let raw_stop = BigUint::try_from(raw_stop).map_err(|_| {
                "The time offset leaves no samples in the selected range.".to_string()
            })?;
            let wave_container = waves
                .inner
                .as_waves()
                .ok_or_else(|| "FFT is only available for waveform signals.".to_string())?;
            let rising = matches!(state.sampling_mode, SamplingMode::RisingEdge);
            let edge_name = state.sampling_mode.edge_label();
            let edges = wave_container
                .edge_times(&clock_variable.variable_ref, &raw_start, &raw_stop, rising)
                .map_err(|error| format!("Could not read clock signal: {error}"))?;
            let edge_multiple = state.edge_multiple.max(1);
            let sample_ticks = edges
                .into_iter()
                .enumerate()
                .filter_map(|(index, edge)| {
                    if (index + 1) % edge_multiple != 0 {
                        return None;
                    }
                    let adjusted = BigUint::try_from(BigInt::from(edge) + &offset).ok()?;
                    if adjusted < start || adjusted > stop {
                        return None;
                    }
                    Some(adjusted)
                })
                .collect::<Vec<_>>();
            // The FFT operates on the ordered sample sequence. Use the first selected
            // edge interval as the effective spacing for the frequency-axis labels.
            let sample_interval = sample_ticks
                .windows(2)
                .next()
                .map(|times| &times[1] - &times[0])
                .filter(|interval| !interval.is_zero())
                .ok_or_else(|| {
                    format!("At least two {edge_name}s are required for FFT sampling.")
                })?;
            (sample_ticks, sample_interval)
        }
    };

    let sample_count = sample_ticks.len();
    let fft_length = sample_count
        .checked_add(state.zero_padding)
        .ok_or_else(|| "FFT length is too large.".to_string())?;
    if sample_count == 0 || fft_length > MAX_FFT_SAMPLES {
        return Err(format!(
            "FFT length must be between 1 and {MAX_FFT_SAMPLES} samples."
        ));
    }

    let Some(real_signal) = state.real_signal else {
        return Err("Select a real signal.".to_string());
    };
    let real_samples = signal_samples(waves, translators, real_signal, &sample_ticks)?;
    let imaginary_samples = match state.imaginary_signal {
        Some(imaginary_signal) => Some(signal_samples(
            waves,
            translators,
            imaginary_signal,
            &sample_ticks,
        )?),
        None => None,
    };

    let seconds_per_tick = timescale.seconds_per_tick();
    // Keep timestamp arithmetic in BigUint until conversion to the floating-point
    // representation required by rustfft and egui_plot.
    let sample_seconds = sample_interval
        .to_f64()
        .ok_or_else(|| "Sample time is outside the supported frequency range.".to_string())?
        * seconds_per_tick;
    if !sample_seconds.is_finite() || sample_seconds <= 0.0 {
        return Err("Sample time is outside the supported frequency range.".to_string());
    }

    // Zero-initializing the full buffer provides the requested zero padding.
    let mut buffer = vec![Complex::new(0.0, 0.0); fft_length];
    for index in 0..sample_count {
        let imaginary = imaginary_samples
            .as_ref()
            .map_or(0.0, |samples| samples[index]);
        buffer[index] = Complex::new(real_samples[index], imaginary);
    }

    let mut planner = FftPlanner::<f64>::new();
    planner.plan_fft_forward(fft_length).process(&mut buffer);

    let sample_times = sample_ticks
        .iter()
        .map(|time| (time - &start).to_f64().map(|time| time * seconds_per_tick))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "Sample times are outside the supported plotting range.".to_string())?;
    // Real-valued input has conjugate symmetry, so only its non-negative bins
    // are plotted. Complex input has independent negative-frequency bins, which
    // are plotted too and ordered from the lowest to the highest frequency.
    let real_input = imaginary_samples.is_none();
    let negative_start = (fft_length + 1) / 2;
    let bin_count = if real_input {
        fft_length / 2 + 1
    } else {
        fft_length
    };
    let component_scale = 1.0 / sample_count as f64;
    let mut fft_real = Vec::with_capacity(bin_count);
    let mut fft_imaginary = Vec::with_capacity(bin_count);
    let mut magnitude = Vec::with_capacity(bin_count);
    let mut phase = Vec::with_capacity(bin_count);
    for output_index in 0..bin_count {
        let index = if real_input {
            output_index
        } else {
            (negative_start + output_index) % fft_length
        };
        let value = buffer[index];
        let frequency_index = if !real_input && index >= negative_start {
            index as isize - fft_length as isize
        } else {
            index as isize
        };
        let frequency = frequency_index as f64 / (fft_length as f64 * sample_seconds);
        let one_sided_scale = if real_input && index > 0 && index * 2 < fft_length {
            2.0
        } else {
            1.0
        };
        fft_real.push([frequency, value.re * component_scale * one_sided_scale]);
        fft_imaginary.push([frequency, value.im * component_scale * one_sided_scale]);
        let value_magnitude = value.norm() * component_scale * one_sided_scale;
        magnitude.push([frequency, value_magnitude]);
        phase.push([frequency, value.im.atan2(value.re)]);
    }

    let time_scale = axis_scale_for_range(
        sample_times
            .first()
            .copied()
            .zip(sample_times.last().copied()),
    );
    let frequency_axis_range = magnitude
        .first()
        .zip(magnitude.last())
        .map(|([min_frequency, _], [max_frequency, _])| (*min_frequency, *max_frequency));
    let frequency_scale = axis_scale_for_range(frequency_axis_range);

    Ok(Spectrum {
        sample_times,
        real_samples,
        imaginary_samples,
        fft_real,
        fft_imaginary,
        magnitude,
        phase,
        sample_count,
        fft_length,
        time_scale,
        frequency_scale,
    })
}

/// Query and translate each selected signal at the requested sample timestamps.
///
/// A missing value is treated as zero, while a loaded value must be supported by
/// the signal's configured numeric translator.
fn signal_samples(
    waves: &WaveData,
    translators: &TranslatorList,
    item_ref: DisplayedItemRef,
    sample_ticks: &[BigUint],
) -> Result<Vec<f64>, String> {
    let Some(DisplayedItem::Variable(variable)) = waves.displayed_items.get(&item_ref) else {
        return Err("The selected item is not a signal.".to_string());
    };
    let wave_container = waves
        .inner
        .as_waves()
        .ok_or_else(|| "FFT is only available for waveform signals.".to_string())?;
    let meta = wave_container
        .variable_meta(&variable.variable_ref)
        .map_err(|error| format!("Could not read signal metadata: {error}"))?;
    let field = DisplayedFieldRef::from(item_ref);
    let translator = waves.variable_translator(&field, translators);
    let translator_name = translator.name();
    let mut values = Vec::with_capacity(sample_ticks.len());

    for time in sample_ticks {
        let value = wave_container
            .query_variable(&variable.variable_ref, time)
            .map_err(|error| format!("Could not read signal value: {error}"))?
            .and_then(|result| result.current.map(|(_, value)| value));

        let numeric = match value {
            Some(value) => translator.translate_numeric(&meta, &value).ok_or_else(|| {
                format!("Translator '{translator_name}' has no numeric value for this signal.")
            })?,
            None => 0.0,
        };
        values.push(numeric);
    }

    Ok(values)
}
