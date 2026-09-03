use crate::{
    config::{ColorMode, OutputFormat, PredictOptions, TuiOptions},
    error::{CliError, ErrorCategory},
    predict::{PredictionSnapshot, ReplaySampleFeed, load_live_snapshot_with_feed},
};
use chrono::{DateTime, SecondsFormat, Utc};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use hypercarry_core::{
    metrics::{FundingInterval, funding_apr},
    predictor::FUNDING_HOUR_MS,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use rust_decimal::Decimal;
use std::{
    cmp::Ordering,
    io::{self, IsTerminal, Stdout},
    time::{Duration, Instant, SystemTime},
};

pub fn run(options: &TuiOptions) -> Result<(), CliError> {
    if !io::stdout().is_terminal() {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "TUI requires an interactive terminal; use `predict --output json` for redirected output",
        ));
    }
    let mut feed = ReplaySampleFeed::open(&options.capture, options.network, options.coin.clone())?;
    let use_color = resolve_color(options.color, true, std::env::var_os("NO_COLOR").is_some());
    let mut session = TerminalSession::enter()?;
    let refresh = Duration::from_millis(options.refresh_ms);
    let mut next_refresh = Instant::now();
    let mut state = TuiState::Loading;

    loop {
        if Instant::now() >= next_refresh {
            state = refresh_state(options, &mut feed);
            next_refresh = Instant::now() + refresh;
        }
        session
            .terminal
            .draw(|frame| render(frame, &state, use_color))
            .map_err(terminal_error)?;

        let timeout = next_refresh.saturating_duration_since(Instant::now());
        if event::poll(timeout).map_err(terminal_error)? {
            let event = event::read().map_err(terminal_error)?;
            if should_quit(&event) {
                return Ok(());
            }
        }
    }
}

fn refresh_state(options: &TuiOptions, feed: &mut ReplaySampleFeed) -> TuiState {
    let result = current_prediction_options(options)
        .and_then(|predict_options| load_live_snapshot_with_feed(&predict_options, feed));
    match result {
        Ok(snapshot) => TuiState::Ready(TuiView::from_snapshot(&snapshot)),
        Err(error) => TuiState::Error {
            category: error.category().as_str(),
            message: error.to_string(),
        },
    }
}

fn current_prediction_options(options: &TuiOptions) -> Result<PredictOptions, CliError> {
    let as_of_ms = current_timestamp_ms()?;
    let settlement_ms = as_of_ms
        .div_euclid(FUNDING_HOUR_MS)
        .checked_add(1)
        .and_then(|hour| hour.checked_mul(FUNDING_HOUR_MS))
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Internal,
                "current UTC hour cannot be represented as Unix milliseconds",
            )
        })?;
    Ok(PredictOptions {
        network: options.network,
        coin: options.coin.clone(),
        capture: options.capture.clone(),
        settlement_ms,
        as_of_ms,
        dataset: options.dataset.clone(),
        official_rate: None,
        official_observed_at_ms: None,
        output: OutputFormat::Human,
        tracing: options.tracing,
    })
}

fn current_timestamp_ms() -> Result<i64, CliError> {
    let milliseconds = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(|error| {
            CliError::with_source(
                ErrorCategory::Internal,
                "system clock is before Unix epoch",
                error,
            )
        })?
        .as_millis();
    i64::try_from(milliseconds).map_err(|_| {
        CliError::new(
            ErrorCategory::Internal,
            "current Unix timestamp does not fit signed milliseconds",
        )
    })
}

fn should_quit(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && (matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)))
    )
}

fn resolve_color(mode: ColorMode, is_terminal: bool, no_color: bool) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => is_terminal && !no_color,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TuiState {
    Loading,
    Ready(TuiView),
    Error {
        category: &'static str,
        message: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct TuiView {
    heading: String,
    settlement_utc: String,
    cutoff_utc: String,
    direction: &'static str,
    hourly_rate_percent: String,
    simple_apr_percent: String,
    coverage_percent: String,
    confidence_percent: String,
    samples: String,
    raw_frames: u64,
}

impl TuiView {
    fn from_snapshot(snapshot: &PredictionSnapshot) -> Self {
        let prediction = &snapshot.prediction;
        let hourly = prediction.predicted_hourly_rate;
        let interval = FundingInterval::from_hours(1).expect("one hour is non-zero");
        Self {
            heading: format!(
                "{}-PERP · HYPERLIQUID {}",
                prediction.coin,
                snapshot.network.as_str().to_ascii_uppercase()
            ),
            settlement_utc: timestamp_utc(prediction.settlement_time_ms.as_i64()),
            cutoff_utc: timestamp_utc(prediction.generated_at_ms.as_i64()),
            direction: match hourly.cmp(&Decimal::ZERO) {
                Ordering::Greater => "POSITIVE",
                Ordering::Less => "NEGATIVE",
                Ordering::Equal => "ZERO",
            },
            hourly_rate_percent: signed_percent(hourly),
            simple_apr_percent: signed_percent(funding_apr(hourly, interval)),
            coverage_percent: percent(prediction.coverage.coverage_ratio),
            confidence_percent: percent(prediction.coverage.confidence_ratio),
            samples: format!(
                "{} / {} five-second slots",
                prediction.coverage.samples_used, prediction.coverage.expected_samples_so_far
            ),
            raw_frames: snapshot.raw_frames_replayed,
        }
    }

    fn rows(&self) -> Vec<String> {
        vec![
            format!("Settlement UTC   {}", self.settlement_utc),
            format!("Cutoff UTC       {}", self.cutoff_utc),
            format!(
                "Hourly funding    {}  ({})",
                self.hourly_rate_percent, self.direction
            ),
            format!("Simple APR       {}", self.simple_apr_percent),
            format!(
                "Coverage         {}  ({})",
                self.coverage_percent, self.samples
            ),
            format!("Confidence       {}", self.confidence_percent),
            format!("Raw frames       {}", self.raw_frames),
        ]
    }
}

fn timestamp_utc(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms).map_or_else(
        || format!("invalid ({timestamp_ms} ms)"),
        |timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}

fn signed_percent(value: Decimal) -> String {
    let value = (value * Decimal::from(100_u32)).normalize();
    if value.is_sign_negative() {
        format!("{value}%")
    } else {
        format!("+{value}%")
    }
}

fn percent(value: Decimal) -> String {
    format!("{}%", (value * Decimal::from(100_u32)).normalize())
}

fn render(frame: &mut Frame<'_>, state: &TuiState, use_color: bool) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    let title = match state {
        TuiState::Ready(view) => view.heading.as_str(),
        TuiState::Loading | TuiState::Error { .. } => "HYPERCARRY LIVE FUNDING",
    };
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title(" Market ")),
        header,
    );

    let content = match state {
        TuiState::Loading => vec![Line::from("Waiting for first replay refresh…")],
        TuiState::Error { category, message } => vec![
            Line::from(Span::styled(
                format!("ERROR [{category}]"),
                semantic_style("ERROR", use_color),
            )),
            Line::from(message.as_str()),
            Line::from("The TUI will retry; verify the capture is still being recorded."),
        ],
        TuiState::Ready(view) => view
            .rows()
            .into_iter()
            .map(|row| {
                let style = if row.contains("POSITIVE") {
                    semantic_style("POSITIVE", use_color)
                } else if row.contains("NEGATIVE") {
                    semantic_style("NEGATIVE", use_color)
                } else {
                    Style::default()
                };
                Line::styled(row, style)
            })
            .collect(),
    };
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Predictor ")),
        body,
    );
    frame.render_widget(
        Paragraph::new("q / Esc / Ctrl-C: exit safely · all timestamps UTC")
            .block(Block::default().borders(Borders::ALL).title(" Controls ")),
        footer,
    );
}

fn semantic_style(kind: &str, use_color: bool) -> Style {
    if !use_color {
        return Style::default().add_modifier(Modifier::BOLD);
    }
    let color = match kind {
        "POSITIVE" => Color::Green,
        "NEGATIVE" | "ERROR" => Color::Red,
        _ => Color::Cyan,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> Result<Self, CliError> {
        enable_raw_mode().map_err(terminal_error)?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(terminal_error(error));
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, LeaveAlternateScreen, Show);
                Err(terminal_error(error))
            }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen, Show);
        let _ = self.terminal.show_cursor();
    }
}

fn terminal_error(error: io::Error) -> CliError {
    CliError::with_source(
        ErrorCategory::Output,
        "interactive terminal operation failed; terminal state was restored",
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_core::{
        info::Network,
        predictor::{FundingPrediction, PredictionCoverage},
        types::TimestampMs,
    };

    fn snapshot(rate: &str) -> PredictionSnapshot {
        PredictionSnapshot {
            schema_version: 1,
            network: Network::Testnet,
            capture_path: "capture.jsonl".to_owned(),
            raw_frames_replayed: 42,
            premium_samples_replayed: 2,
            prediction: FundingPrediction {
                schema_version: 1,
                coin: "BTC".to_owned(),
                settlement_time_ms: TimestampMs::new(3_600_000),
                generated_at_ms: TimestampMs::new(1_800_000),
                average_premium: "0.001".parse().unwrap(),
                predicted_hourly_rate: rate.parse().unwrap(),
                coverage: PredictionCoverage {
                    samples_used: 2,
                    expected_samples_so_far: 360,
                    coverage_ratio: "0.5".parse().unwrap(),
                    hour_progress_ratio: "0.5".parse().unwrap(),
                    confidence_ratio: "0.25".parse().unwrap(),
                },
                official_benchmark_hourly_rate: None,
            },
            evaluation: None,
        }
    }

    #[test]
    fn view_snapshot_is_deterministic_and_never_relies_on_color_for_sign() {
        let view = TuiView::from_snapshot(&snapshot("0.001"));
        assert_eq!(
            view.rows(),
            [
                "Settlement UTC   1970-01-01T01:00:00.000Z",
                "Cutoff UTC       1970-01-01T00:30:00.000Z",
                "Hourly funding    +0.1%  (POSITIVE)",
                "Simple APR       +876%",
                "Coverage         50%  (2 / 360 five-second slots)",
                "Confidence       25%",
                "Raw frames       42",
            ]
        );
        assert_eq!(
            TuiView::from_snapshot(&snapshot("-0.001")).direction,
            "NEGATIVE"
        );
    }

    #[test]
    fn auto_color_respects_no_color_and_non_terminal_output() {
        assert!(resolve_color(ColorMode::Auto, true, false));
        assert!(!resolve_color(ColorMode::Auto, true, true));
        assert!(!resolve_color(ColorMode::Auto, false, false));
        assert!(resolve_color(ColorMode::Always, false, true));
        assert!(!resolve_color(ColorMode::Never, true, false));
    }

    #[test]
    fn quit_keys_include_accessible_escape_q_and_control_c() {
        use crossterm::event::{KeyEvent, KeyEventState};
        let key = |code, modifiers| {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            })
        };
        assert!(should_quit(&key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(should_quit(&key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(should_quit(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!should_quit(&key(KeyCode::Char('c'), KeyModifiers::NONE)));
    }
}
