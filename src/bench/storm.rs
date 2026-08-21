//! Search-dispatch probe: counts full-corpus searches dispatched versus results
//! actually applied while the initial background diff load is running.
//!
//! Runs the real `AppState` against a real (windowless) `egui::Context`, so the
//! frame cadence comes from the same `request_repaint`/`request_repaint_after`
//! calls the GUI uses rather than from an assumed rate. Only the compositor is
//! simulated: `--fps` caps how soon a requested repaint can become a frame.

use crate::app::{AppState, FontVariants};
use crate::domain::file_pair;
use crate::domain::review_state::ReviewState;
use crate::domain::settings::{RenameLimit, Settings};
use std::time::{Duration, Instant};

/// Stretch with no applied result that counts as the search having settled.
const QUIESCE: Duration = Duration::from_millis(500);

pub struct Options {
    pub scale: usize,
    pub seed: u64,
    pub query: String,
    pub fps: f64,
    /// Delay before opening search, emulating a user reacting to the load.
    pub delay_ms: u64,
}

pub fn run(opts: &Options) {
    let corpus = match super::corpus::generate(opts.scale, opts.seed) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error generating corpus: {e}");
            std::process::exit(1);
        }
    };

    let pairs = file_pair::walk_and_pair(&corpus.left, &corpus.right, false, RenameLimit::Fixed(0))
        .unwrap_or_else(|e| {
            eprintln!("Error scanning corpus: {e}");
            std::process::exit(1);
        });

    let review_state = ReviewState::new(pairs.iter().map(|p| p.relative_path.clone()).collect());
    let ctx = eframe::egui::Context::default();
    let total_files = pairs.len();

    let start = Instant::now();
    let mut state = AppState::new(
        pairs,
        review_state,
        None,
        ctx.clone(),
        Settings::default(),
        FontVariants {
            has_bold: false,
            has_italic: false,
            has_bold_italic: false,
        },
    );
    let ctor = start.elapsed();

    let mut opened = opts.delay_ms == 0;
    if opened {
        open_search(&mut state, &opts.query);
    }

    let frame_interval = Duration::from_secs_f64(1.0 / opts.fps);
    let mut frames = 0u32;
    let mut load_done_at = None;
    let mut peak_matches = 0usize;
    let mut regressions = 0u32;
    let mut prev_matches = 0usize;
    let mut last_applies = 0u32;
    let mut in_flight_time = Duration::ZERO;
    let mut in_flight_spans = 0u32;
    let mut was_searching = false;
    let loop_start = Instant::now();
    let mut last_change = loop_start;

    loop {
        let frame_start = Instant::now();

        if !opened
            && frame_start.duration_since(loop_start).as_millis() >= u128::from(opts.delay_ms)
        {
            open_search(&mut state, &opts.query);
            opened = true;
        }

        let output = ctx.run_ui(eframe::egui::RawInput::default(), |_ui| {
            state.poll_background();
        });
        frames += 1;

        // Sampled per frame, so resolution is the frame interval — enough to tell
        // an idle-machine search from one contending with the compose pool.
        if state.search.searching {
            in_flight_time += frame_start.elapsed().max(frame_interval);
            if !was_searching {
                in_flight_spans += 1;
            }
        }
        was_searching = state.search.searching;

        let matches = state.search.total_matches();
        if matches < prev_matches {
            regressions += 1;
        }
        peak_matches = peak_matches.max(matches);
        prev_matches = matches;

        if load_done_at.is_none() && state.files_computed >= total_files {
            load_done_at = Some(loop_start.elapsed());
        }
        if state.search_applies != last_applies {
            last_applies = state.search_applies;
            last_change = Instant::now();
        }
        // `is_searching()` is not a reliable quiescence signal in the unserialised
        // build: the first result to land clears it while others are still running.
        // Wait out a stretch with no applies instead, so the final count is real.
        if load_done_at.is_some() && last_change.elapsed() > QUIESCE {
            break;
        }

        let requested = output
            .viewport_output
            .get(&eframe::egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        // A frame can't land sooner than the next compositor tick. Clamp the upper
        // bound so a quiet stretch still polls instead of parking forever.
        let wait = requested
            .max(frame_interval.saturating_sub(frame_start.elapsed()))
            .min(Duration::from_millis(50));
        std::thread::sleep(wait);
    }

    let wall = loop_start.elapsed();
    let load = load_done_at.unwrap_or(wall);
    let settled = last_change.duration_since(loop_start);
    let dispatches = state.search_dispatches;
    let applies = state.search_applies;
    let discarded = dispatches.saturating_sub(applies);

    println!(
        "corpus:            scale {} / {total_files} files",
        opts.scale
    );
    println!("query:             {:?}", opts.query);
    println!("fps cap:           {}", opts.fps);
    println!("search opens at:   {} ms", opts.delay_ms);
    println!("ctor (eager+walk): {:.0} ms", ctor.as_secs_f64() * 1000.0);
    println!("load complete:     {:.0} ms", load.as_secs_f64() * 1000.0);
    println!(
        "last apply:        {:.0} ms",
        settled.as_secs_f64() * 1000.0
    );
    println!("probe wall:        {:.0} ms", wall.as_secs_f64() * 1000.0);
    println!(
        "frames:            {frames} ({:.0}/s)",
        f64::from(frames) / wall.as_secs_f64()
    );
    println!(
        "dispatches:        {dispatches} ({:.0}/s)",
        f64::from(dispatches) / wall.as_secs_f64()
    );
    println!(
        "applies:           {applies} ({:.0}/s)",
        f64::from(applies) / wall.as_secs_f64()
    );
    println!(
        "discarded:         {discarded} ({:.0}% of dispatches)",
        if dispatches == 0 {
            0.0
        } else {
            100.0 * f64::from(discarded) / f64::from(dispatches)
        }
    );
    println!(
        "dispatch:apply     {:.2}:1",
        if applies == 0 {
            0.0
        } else {
            f64::from(dispatches) / f64::from(applies)
        }
    );
    println!(
        "search in flight:  {:.0} ms total over {in_flight_spans} spans ({:.0} ms each, {:.0}% of load)",
        in_flight_time.as_secs_f64() * 1000.0,
        if in_flight_spans == 0 {
            0.0
        } else {
            in_flight_time.as_secs_f64() * 1000.0 / f64::from(in_flight_spans)
        },
        100.0 * in_flight_time.as_secs_f64() / load.as_secs_f64()
    );
    println!("match count:       {peak_matches} peak, {prev_matches} final");
    println!("count regressions: {regressions}");
}

fn open_search(state: &mut AppState, query: &str) {
    state.search.open = true;
    state.search.query = query.to_string();
    state.search.mark_query_changed();
}
