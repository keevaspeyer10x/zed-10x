use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use client::Client;
use gpui::{AppContext, TasksIncluded, profiler};
use parking_lot::Mutex;
use ui::App;

mod liveness;
mod logging;
mod task_traces;
mod telemetry;

gpui::actions!(
    dev,
    [
        /// Causes a performance hang to test performance monitoring
        HangAction,
        /// Causes a performance hang to test performance monitoring
        HangBackground,
        /// Causes a performance hang to test performance monitoring
        HangForeground,
    ]
);

pub(crate) fn start(client: Arc<Client>, cx: &mut App) {
    let hang_time = if cfg!(debug_assertions) {
        if cfg!(windows) {
            // yes windows debug builds are horribly slow
            Duration::from_secs(30)
        } else {
            Duration::from_secs(5)
        }
    } else {
        // will be lowered over time or turned into a setting
        Duration::from_millis(100)
    };

    if cfg!(debug_assertions) {
        log::warn!("debug build, only reporting hangs longer then {hang_time:?}");
    }

    let liveness = liveness::start(cx);
    start_hang_detection(hang_time, client, liveness, cx);
    let foreground_uat_hang_time = hang_time
        .max(liveness::UAT_FOREGROUND_HANG_DURATION)
        .saturating_add(Duration::from_micros(1));

    cx.on_action(move |_: &HangAction, _| {
        log::warn!(
            "Hanging the foreground for {hang_time:?} by blocking in an action. \
            Zed will be unresponsive for that time. This should trigger a report in the log",
        );
        thread::sleep(hang_time + Duration::from_micros(1));
        log::warn!("Hang ended");
    });
    cx.on_action(move |_: &HangBackground, cx| {
        cx.background_spawn(async move {
            log::warn!(
                "Hanging one background executor for {hang_time:?}. \
                This should trigger a report in the log",
            );
            thread::sleep(hang_time + Duration::from_micros(1));
            log::warn!("Hang ended");
        })
        .detach();
    });
    cx.on_action(move |_: &HangForeground, cx| {
        cx.spawn(async move |_| {
            log::warn!(
                "Hanging the foreground executor for {foreground_uat_hang_time:?} to test \
                performance monitoring! Zed will be unresponsive for that time. \
                This should trigger a report in the log"
            );
            thread::sleep(foreground_uat_hang_time);
            log::warn!("Hang ended");
        })
        .detach();
    });
}

fn start_hang_detection(
    report_longer_then: Duration,
    client: Arc<Client>,
    liveness: Option<liveness::Monitor>,
    cx: &App,
) {
    let foreground_thread = thread::current().id();
    let monitor_interval = Duration::from_secs(1);
    if let Some(liveness) = liveness {
        start_liveness_detection(liveness, monitor_interval);
    }
    let telemetry = Arc::new(Mutex::new(telemetry::Reporter::new(foreground_thread)));
    let mut log = logging::Reporter::new(monitor_interval, report_longer_then, foreground_thread);

    let telemetry2 = Arc::clone(&telemetry);
    cx.on_app_quit({
        move |_| {
            telemetry2.lock().send();
            client.telemetry().flush_events()
        }
    })
    .detach();

    // an OS thread to insulate detection and reporting from hangs on the fore
    // or background.
    thread::Builder::new()
        .name("HangDetection".to_string())
        .spawn(move || {
            // allow "bad" tasks during startup. Not because we should but since here
            // they are not observed by the user and to lower on clutter from the reporter
            thread::sleep(Duration::from_millis(200));
            loop {
                thread::sleep(monitor_interval);
                // TODO(yara) the telemetry should not include still running tasks while the
                // reports being logged should.
                let task_stats = profiler::take_all_stats(TasksIncluded::CompletedAndRunning);
                let action_stats = profiler::take_action_stats();

                {
                    let mut telemetry = telemetry.lock();
                    telemetry.update(&task_stats, &action_stats);
                    telemetry.send_periodically();
                }

                let should_write_trace = log.check_and_report(&task_stats, &action_stats);
                if should_write_trace {
                    if let Some(path) = task_traces::save_any(foreground_thread) {
                        log::info!("Task trace has been saved to: {}", path.display());
                    }
                }
            }
        })
        .expect("App can always spawn threads");
}

fn start_liveness_detection(mut liveness: liveness::Monitor, interval: Duration) {
    thread::Builder::new()
        .name("Zed10xLiveness".to_string())
        .spawn(move || {
            let mut next_tick = Instant::now() + interval;
            loop {
                thread::sleep(next_tick.saturating_duration_since(Instant::now()));
                liveness.poll();

                let now = Instant::now();
                next_tick += interval;
                if next_tick <= now {
                    // Do not create a catch-up storm after suspension or scheduler starvation.
                    next_tick = now + interval;
                }
            }
        })
        .expect("App can always spawn threads");
}
