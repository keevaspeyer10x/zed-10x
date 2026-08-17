#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    };
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use chrono::{DateTime, Days, NaiveDate, Utc};
    use gpui::TestAppContext;
    use serde_json::{Map, Value, json};

    use super::{
        AcknowledgementSample, AppendOutcome, AtomicAcknowledgement, ClockSample, CustodyFacts,
        DropReason, EventFact, EventName, FileIdentity, LaunchIdentity, LifecycleCoverage,
        LifecycleOffer, LifecycleProducer, LivenessMonitor, LivenessTransition,
        LivenessTransitions, ManualClock, PendingEvent, RecorderConfig, RecorderIngress,
        RetentionPolicy, SinkOffer, SourceIdentity, StoreEntryKind, StoreOpenError, StoreRole,
        TestRecorder, TryEventSink, UAT_FOREGROUND_HANG_DURATION, WriterCompletion, WriterThread,
        admit_store_custody, day_directory_name, encode_event_line,
        open_or_create_store_nofollow_with_missing_hook,
        open_private_directory_at_with_missing_hook, owned_shard_name, restart_telemetry_disabled,
        retention_scan_quota, retention_slot_index, slot_directory_name, spawn_writer,
        spawn_writer_with_clock, start_configured, start_foreground_acknowledger, try_send_probe,
        validate_event_line,
    };

    const INTERVAL: Duration = Duration::from_secs(1);
    const THRESHOLD: Duration = Duration::from_secs(5);
    const TRACE_ID: &str = "0123456789abcdef0123456789abcdef";
    const SPAN_ID: &str = "0123456789abcdef";
    const WRITER_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_WRITER_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const BASE_UNIX_SECONDS: u64 = 1_785_715_200;
    const TEST_RETENTION_DAYS: usize = 14;

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            // The recorder intentionally rejects world-writable ancestors such as Linux /tmp.
            let private_parent = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| {
                    std::env::current_dir().expect("test process must have an absolute directory")
                });
            let path = private_parent.join(format!(
                "zed-10x-s1-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("test root must be created");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .expect("test root must be private");
            }
            Self { path }
        }

        fn store(&self) -> PathBuf {
            self.path.join("events")
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Default)]
    struct CapturingSink {
        events: Vec<PendingEvent>,
        scripted_drops: VecDeque<Option<DropReason>>,
    }

    impl CapturingSink {
        fn with_drops(drops: impl IntoIterator<Item = DropReason>) -> Self {
            Self {
                events: Vec::new(),
                scripted_drops: drops.into_iter().map(Some).collect(),
            }
        }

        fn with_script(script: impl IntoIterator<Item = Option<DropReason>>) -> Self {
            Self {
                events: Vec::new(),
                scripted_drops: script.into_iter().collect(),
            }
        }
    }

    impl TryEventSink for CapturingSink {
        fn try_offer(&mut self, event: PendingEvent) -> SinkOffer {
            match self.scripted_drops.pop_front() {
                Some(Some(reason)) => SinkOffer::Dropped(reason),
                Some(None) | None => {
                    self.events.push(event);
                    SinkOffer::Accepted
                }
            }
        }
    }

    fn source_identity() -> SourceIdentity {
        SourceIdentity::for_test(
            "local",
            "1.14.0-10x",
            "20260807.1",
            "aa3f7283d5b4dd9112c8c87dd8200486626aa76e",
        )
        .expect("fixed source identity must be valid")
    }

    fn launch_identity() -> LaunchIdentity {
        LaunchIdentity::from_parts_for_test(TRACE_ID, SPAN_ID, WRITER_ID)
            .expect("fixed launch identity must be valid")
    }

    fn other_launch_identity() -> LaunchIdentity {
        LaunchIdentity::from_parts_for_test(
            "fedcba9876543210fedcba9876543210",
            "fedcba9876543210",
            OTHER_WRITER_ID,
        )
        .expect("second fixed launch identity must be valid")
    }

    fn at(second_offset: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(BASE_UNIX_SECONDS + second_offset)
    }

    fn clock(monotonic_millis: u64, suspend_aware_millis: u64) -> ClockSample {
        ClockSample::for_test(
            Duration::from_millis(monotonic_millis),
            UNIX_EPOCH + Duration::from_millis(suspend_aware_millis),
        )
    }

    fn acknowledgement(sequence: u64, at: ClockSample) -> AcknowledgementSample {
        if sequence == 0 {
            AcknowledgementSample::initial()
        } else {
            AcknowledgementSample::for_test(sequence, at)
        }
    }

    fn recorder_config(root: &TestRoot) -> RecorderConfig {
        RecorderConfig::for_test(
            root.store(),
            source_identity(),
            false,
            8 * 1024 * 1024,
            RetentionPolicy {
                retained_days: TEST_RETENTION_DAYS as u64,
                scan_cap: 64,
            },
        )
        .expect("fixed recorder configuration must be valid")
    }

    fn event_facts() -> [EventFact; 5] {
        [
            EventFact::AppLaunch,
            EventFact::AppProcessExit,
            EventFact::LivenessReady {
                baseline_latency: Duration::from_millis(12),
                threshold: THRESHOLD,
            },
            EventFact::Hang {
                duration: THRESHOLD,
                threshold: THRESHOLD,
                missed_intervals: 5,
            },
            EventFact::HangRecovered {
                duration: Duration::from_secs(7),
                threshold: THRESHOLD,
                missed_intervals: 7,
            },
        ]
    }

    fn encoded(fact: &EventFact) -> Vec<u8> {
        encode_event_line(
            &source_identity(),
            &launch_identity(),
            1,
            fact,
            at(0),
            at(1),
        )
        .expect("fixed event must encode")
    }

    fn parsed(line: &[u8]) -> Value {
        assert_eq!(line.last(), Some(&b'\n'));
        serde_json::from_slice(&line[..line.len() - 1]).expect("event must contain one JSON object")
    }

    fn event_names(events: &[PendingEvent]) -> Vec<EventName> {
        events.iter().map(|event| event.fact().name()).collect()
    }

    fn day(time: SystemTime) -> NaiveDate {
        DateTime::<Utc>::from(time).date_naive()
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("fixture must be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("fixture must be private");
        }
    }

    #[cfg(unix)]
    fn create_private_directory(path: &Path) {
        fs::create_dir(path).expect("fixture directory must be created");
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("fixture directory must be private");
    }

    #[cfg(unix)]
    fn nested_shard_path(root: &TestRoot, shard_day: NaiveDate, writer_id: &str) -> PathBuf {
        if !root.store().exists() {
            create_private_directory(&root.store());
        }
        let slot = root.store().join(slot_directory_name(retention_slot_index(
            shard_day,
            TEST_RETENTION_DAYS,
        )));
        if !slot.exists() {
            create_private_directory(&slot);
        }
        let day_directory = slot.join(day_directory_name(shard_day));
        if !day_directory.exists() {
            create_private_directory(&day_directory);
        }
        day_directory.join(owned_shard_name(shard_day, writer_id))
    }

    #[cfg(unix)]
    fn native_shards(root: &TestRoot) -> Vec<PathBuf> {
        let mut shards = Vec::new();
        if !root.store().exists() {
            return shards;
        }
        for slot in fs::read_dir(root.store()).unwrap().flatten() {
            let Ok(slot_type) = slot.file_type() else {
                continue;
            };
            if !slot_type.is_dir() {
                continue;
            }
            for day_directory in fs::read_dir(slot.path()).unwrap().flatten() {
                let Ok(day_type) = day_directory.file_type() else {
                    continue;
                };
                if !day_type.is_dir() {
                    continue;
                }
                for shard in fs::read_dir(day_directory.path()).unwrap().flatten() {
                    if shard.file_type().is_ok_and(|kind| kind.is_file()) {
                        shards.push(shard.path());
                    }
                }
            }
        }
        shards.sort();
        shards
    }

    #[test]
    fn s1_contract_01_exact_closed_envelopes_cover_all_five_events() {
        let expected = [
            ("app.launch", "INFO", json!({"lifecycle.state": "started"})),
            (
                "app.process_exit",
                "INFO",
                json!({"lifecycle.state": "clean_exit"}),
            ),
            (
                "app.liveness.ready",
                "INFO",
                json!({
                    "liveness.baseline_latency_ms": 12,
                    "liveness.probe": "gpui_main_queue",
                    "liveness.threshold_ms": 5000,
                }),
            ),
            (
                "app.hang",
                "WARN",
                json!({
                    "duration.ms": 5000,
                    "failure.class": "main_thread_unresponsive",
                    "liveness.missed_intervals": 5,
                    "liveness.probe": "gpui_main_queue",
                    "liveness.threshold_ms": 5000,
                }),
            ),
            (
                "app.hang.recovered",
                "INFO",
                json!({
                    "duration.ms": 7000,
                    "liveness.missed_intervals": 7,
                    "liveness.probe": "gpui_main_queue",
                    "liveness.threshold_ms": 5000,
                }),
            ),
        ];

        for (fact, (body, severity, event_attributes)) in event_facts().iter().zip(expected) {
            let line = encoded(fact);
            assert!(validate_event_line(&line).is_ok());
            let value = parsed(&line);
            let top_fields = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                top_fields,
                [
                    "attributes",
                    "body",
                    "observed_time_unix_nano",
                    "severity_text",
                    "span_id",
                    "time_unix_nano",
                    "trace_id",
                ]
                .into_iter()
                .collect()
            );
            assert_eq!(value["body"], body);
            assert_eq!(value["severity_text"], severity);
            assert_eq!(value["trace_id"], TRACE_ID);
            assert_eq!(value["span_id"], SPAN_ID);
            assert_eq!(
                value["time_unix_nano"],
                ((BASE_UNIX_SECONDS as u128) * 1_000_000_000).to_string()
            );
            assert_eq!(
                value["observed_time_unix_nano"],
                (((BASE_UNIX_SECONDS + 1) as u128) * 1_000_000_000).to_string()
            );

            let mut attributes = Map::new();
            attributes.insert("event.sequence".into(), json!(1));
            attributes.insert("event.schema_version".into(), json!(1));
            attributes.insert("service.name".into(), json!("zed-10x"));
            attributes.insert("service.version".into(), json!("1.14.0-10x"));
            attributes.insert(
                "vcs.ref.head.revision".into(),
                json!("aa3f7283d5b4dd9112c8c87dd8200486626aa76e"),
            );
            attributes.insert("zed.build_version".into(), json!("20260807.1"));
            attributes.insert("zed.cohort".into(), json!("zed10x"));
            attributes.insert("zed.lane".into(), json!("local"));
            attributes.insert("zed.source".into(), json!("in_process"));
            attributes.insert("zed.writer_id".into(), json!(WRITER_ID));
            attributes.extend(event_attributes.as_object().unwrap().clone());
            assert_eq!(value["attributes"], Value::Object(attributes));
        }
    }

    #[test]
    fn s1_contract_02_malformed_or_unknown_envelope_data_rejects_before_use() {
        let valid = encoded(&EventFact::Hang {
            duration: THRESHOLD,
            threshold: THRESHOLD,
            missed_intervals: 5,
        });
        let base = parsed(&valid);
        let mut mutations = Vec::new();

        let mut unknown_top = base.clone();
        unknown_top["unknown"] = json!(true);
        mutations.push(unknown_top);
        let mut unknown_event = base.clone();
        unknown_event["body"] = json!("app.hung-ish");
        mutations.push(unknown_event);
        let mut wrong_severity = base.clone();
        wrong_severity["severity_text"] = json!(7);
        mutations.push(wrong_severity);
        let mut invalid_trace = base.clone();
        invalid_trace["trace_id"] = json!("00000000000000000000000000000000");
        mutations.push(invalid_trace);
        let mut invalid_span = base.clone();
        invalid_span["span_id"] = json!("0000000000000000");
        mutations.push(invalid_span);
        let mut invalid_time = base.clone();
        invalid_time["time_unix_nano"] = json!("not-a-number");
        mutations.push(invalid_time);
        let mut invalid_observed_time = base.clone();
        invalid_observed_time["observed_time_unix_nano"] = json!(-1);
        mutations.push(invalid_observed_time);
        let mut unknown_attribute = base.clone();
        unknown_attribute["attributes"]["extra"] = json!("not closed");
        mutations.push(unknown_attribute);
        let mut zero_threshold = base.clone();
        zero_threshold["attributes"]["liveness.threshold_ms"] = json!(0);
        mutations.push(zero_threshold);
        let mut negative_missed = base.clone();
        negative_missed["attributes"]["liveness.missed_intervals"] = json!(-1);
        mutations.push(negative_missed);
        let mut zero_sequence = base;
        zero_sequence["attributes"]["event.sequence"] = json!(0);
        mutations.push(zero_sequence);

        for mutation in mutations {
            let mut bytes = serde_json::to_vec(&mutation).unwrap();
            bytes.push(b'\n');
            assert!(validate_event_line(&bytes).is_err(), "accepted {mutation}");
        }
        assert!(validate_event_line(&valid[..valid.len() - 1]).is_err());
        let mut doubled = valid.clone();
        doubled.extend_from_slice(&valid);
        assert!(validate_event_line(&doubled).is_err());
    }

    #[test]
    fn s1_contract_03_privacy_table_and_serialized_bytes_exclude_forbidden_data() {
        assert_eq!(
            UAT_FOREGROUND_HANG_DURATION,
            THRESHOLD + INTERVAL.saturating_mul(3),
            "the foreground UAT must span threshold plus three complete poll cadences"
        );
        let valid = encoded(&EventFact::AppLaunch);
        let serialized = String::from_utf8(valid.clone()).unwrap();
        for forbidden_sample in [
            "secret prompt body",
            "model response body",
            "tool payload body",
            "/Users/keeva/private.rs",
            "owner/private-repository",
            "account@example.invalid",
            "ghp_examplecredential",
            "--access-token",
            "PROVIDER_KEY=secret",
        ] {
            assert!(!serialized.contains(forbidden_sample));
        }

        for (key, value) in [
            ("prompt", "secret prompt body"),
            ("response", "model response body"),
            ("tool.payload", "tool payload body"),
            ("file.path", "/Users/keeva/private.rs"),
            ("repository", "owner/private-repository"),
            ("account", "account@example.invalid"),
            ("token", "ghp_examplecredential"),
            ("credential", "provider-secret"),
            ("argv", "--access-token"),
            ("environment", "PROVIDER_KEY=secret"),
        ] {
            let mut mutated = parsed(&valid);
            mutated["attributes"][key] = json!(value);
            let mut bytes = serde_json::to_vec(&mutated).unwrap();
            bytes.push(b'\n');
            assert!(validate_event_line(&bytes).is_err(), "accepted {key}");
        }
    }

    #[test]
    fn s1_contract_04_trace_is_shared_within_launch_and_fresh_across_launches() {
        let launch_a = LaunchIdentity::fresh().expect("first identity must be generated");
        let launch_b = LaunchIdentity::fresh().expect("second identity must be generated");
        assert_ne!(launch_a.trace_id(), launch_b.trace_id());
        assert_ne!(launch_a.writer_id(), launch_b.writer_id());

        let first = encode_event_line(
            &source_identity(),
            &launch_a,
            1,
            &EventFact::AppLaunch,
            at(0),
            at(0),
        )
        .unwrap();
        let later = encode_event_line(
            &source_identity(),
            &launch_a,
            2,
            &EventFact::LivenessReady {
                baseline_latency: Duration::from_millis(8),
                threshold: THRESHOLD,
            },
            at(1),
            at(1),
        )
        .unwrap();
        assert_eq!(parsed(&first)["trace_id"], parsed(&later)["trace_id"]);
        assert_eq!(
            parsed(&first)["attributes"]["zed.writer_id"],
            parsed(&later)["attributes"]["zed.writer_id"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_05_jsonl_append_is_atomic_and_short_write_poisons_shard() {
        let root = TestRoot::new("short-write");
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(0), at(0)),
            AppendOutcome::Appended
        );
        let shard = writer.current_shard_path().unwrap().to_path_buf();
        let initial = fs::read(&shard).unwrap();
        assert_eq!(initial.last(), Some(&b'\n'));
        assert_eq!(initial.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(validate_event_line(&initial).is_ok());

        writer.inject_short_write_once(7);
        assert_eq!(
            writer.append(
                EventFact::LivenessReady {
                    baseline_latency: Duration::from_millis(12),
                    threshold: THRESHOLD,
                },
                at(1),
                at(1),
            ),
            AppendOutcome::Dropped(DropReason::ShortWrite)
        );
        let after_short = fs::read(&shard).unwrap();
        assert!(after_short.len() > initial.len());
        assert_eq!(
            writer.append(
                EventFact::Hang {
                    duration: THRESHOLD,
                    threshold: THRESHOLD,
                    missed_intervals: 5,
                },
                at(6),
                at(6),
            ),
            AppendOutcome::Dropped(DropReason::PoisonedShard)
        );
        assert_eq!(fs::read(&shard).unwrap(), after_short);
        assert_eq!(
            writer.append(EventFact::AppProcessExit, at(86_400), at(86_400)),
            AppendOutcome::Appended
        );
        assert_ne!(writer.current_shard_path(), Some(shard.as_path()));
    }

    #[test]
    fn s1_contract_06_capacity_one_probe_queue_coalesces_via_try_send() {
        let (sender, receiver) = async_channel::bounded(1);
        assert!(try_send_probe(&sender, 41));
        assert!(!try_send_probe(&sender, 42));
        assert_eq!(receiver.try_recv(), Ok(41));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn s1_contract_07_recorder_capacity_is_exact_and_full_offer_is_immediate() {
        let (mut ingress, _receiver) = RecorderIngress::bounded(2);
        let first = PendingEvent::new(1, EventFact::AppLaunch, at(0));
        let second = PendingEvent::new(
            2,
            EventFact::LivenessReady {
                baseline_latency: Duration::from_millis(8),
                threshold: THRESHOLD,
            },
            at(1),
        );
        let third = PendingEvent::new(
            3,
            EventFact::Hang {
                duration: THRESHOLD,
                threshold: THRESHOLD,
                missed_intervals: 5,
            },
            at(6),
        );

        assert_eq!(ingress.try_offer(first), SinkOffer::Accepted);
        assert_eq!(ingress.try_offer(second), SinkOffer::Accepted);
        assert_eq!(
            ingress.try_offer(third),
            SinkOffer::Dropped(DropReason::QueueFull)
        );
    }

    #[test]
    fn s1_contract_08_blocked_writer_cannot_stop_ack_detection_or_recovery() {
        let mut monitor = LivenessMonitor::new(INTERVAL, THRESHOLD);
        let mut producer = LifecycleProducer::new();
        let mut sink = CapturingSink::with_drops([
            DropReason::StorageUnavailable,
            DropReason::QueueFull,
            DropReason::StorageUnavailable,
            DropReason::QueueFull,
        ]);

        assert!(matches!(
            producer.offer_launch(&mut sink, at(0)),
            LifecycleOffer::Dropped(_)
        ));
        assert_eq!(
            monitor.tick(clock(0, 0), AcknowledgementSample::initial(), |_| true),
            LivenessTransitions::None
        );
        let ready = monitor
            .tick(
                clock(1_000, 1_000),
                acknowledgement(1, clock(1_000, 1_000)),
                |_| true,
            )
            .into_single()
            .unwrap();
        assert!(matches!(ready, LivenessTransition::Ready { .. }));
        assert!(matches!(
            producer.offer_liveness(&mut sink, ready, at(1)),
            LifecycleOffer::Dropped(_)
        ));

        let mut hang = None;
        for second in 2..=6 {
            hang = monitor
                .tick(
                    clock(second * 1_000, second * 1_000),
                    acknowledgement(1, clock(1_000, 1_000)),
                    |_| true,
                )
                .into_single();
        }
        let hang = hang.expect("threshold crossing must be detected");
        assert!(matches!(hang, LivenessTransition::Hung { .. }));
        assert!(matches!(
            producer.offer_liveness(&mut sink, hang, at(6)),
            LifecycleOffer::Dropped(_)
        ));

        let recovered = monitor
            .tick(
                clock(7_000, 7_000),
                acknowledgement(2, clock(7_000, 7_000)),
                |_| true,
            )
            .into_single()
            .unwrap();
        assert!(matches!(recovered, LivenessTransition::Recovered { .. }));
        assert!(matches!(
            producer.offer_liveness(&mut sink, recovered, at(7)),
            LifecycleOffer::Dropped(_)
        ));
        assert_eq!(producer.dropped_offer_count(), 4);
    }

    #[gpui::test]
    fn s1_contract_09_foreground_dispatch_drives_one_ready_hang_and_recovery(
        cx: &mut TestAppContext,
    ) {
        let (sender, receiver) = async_channel::bounded(1);
        let atomic_acknowledgement = Arc::new(AtomicAcknowledgement::new());
        cx.update(|cx| {
            start_foreground_acknowledger(
                receiver,
                atomic_acknowledgement.clone(),
                Arc::new(ManualClock::new(clock(0, 0))),
                cx,
            );
        });
        assert!(try_send_probe(&sender, 1));
        assert_eq!(atomic_acknowledgement.try_load().unwrap().sequence, 0);
        cx.run_until_parked();
        let foreground_ack = atomic_acknowledgement.try_load().unwrap();
        assert_eq!(foreground_ack.sequence, 1);
        assert!(foreground_ack.acknowledged_at.is_some());

        let mut monitor = LivenessMonitor::new(INTERVAL, THRESHOLD);
        assert_eq!(
            monitor.tick(clock(0, 0), AcknowledgementSample::initial(), |_| true),
            LivenessTransitions::None
        );
        let ready = monitor.tick(
            clock(1_000, 1_000),
            acknowledgement(1, clock(1_000, 1_000)),
            |_| true,
        );
        assert!(matches!(
            ready,
            LivenessTransitions::One(LivenessTransition::Ready { .. })
        ));

        for second in 2..=5 {
            assert_eq!(
                monitor.tick(
                    clock(second * 1_000, second * 1_000),
                    acknowledgement(1, clock(1_000, 1_000)),
                    |_| true,
                ),
                LivenessTransitions::None
            );
        }

        let late_ack = acknowledgement(2, clock(6_100, 6_100));
        let transitions = monitor.tick(clock(6_200, 6_200), late_ack, |_| true);
        let LivenessTransitions::Pair(hang, recovery) = transitions else {
            panic!("a late acknowledgement must close one contiguous hang episode");
        };
        assert!(matches!(hang, LivenessTransition::Hung { .. }));
        assert_eq!(hang.event_time(), clock(6_000, 6_000).suspend_aware);
        assert!(matches!(recovery, LivenessTransition::Recovered { .. }));
        assert_eq!(recovery.event_time(), clock(6_100, 6_100).suspend_aware);
        assert_eq!(
            monitor.tick(clock(7_200, 7_200), late_ack, |_| true),
            LivenessTransitions::None
        );

        let stable_ack = acknowledgement(7, clock(7_000, 7_000));
        assert!(
            atomic_acknowledgement.record(stable_ack.sequence, stable_ack.acknowledged_at.unwrap())
        );
        let cached_ack = atomic_acknowledgement.try_load().unwrap();
        atomic_acknowledgement
            .version
            .fetch_add(1, Ordering::AcqRel);
        assert_eq!(atomic_acknowledgement.try_load(), None);

        let mut stalled_publication_monitor = LivenessMonitor::new(INTERVAL, THRESHOLD);
        assert_eq!(
            stalled_publication_monitor.tick(clock(7_000, 7_000), cached_ack, |_| true),
            LivenessTransitions::None
        );
        let installed = acknowledgement(8, clock(8_000, 8_000));
        let ready = stalled_publication_monitor.tick(clock(8_000, 8_000), installed, |_| true);
        assert!(matches!(
            ready,
            LivenessTransitions::One(LivenessTransition::Ready { .. })
        ));
        let mut detected = LivenessTransitions::None;
        for second in 9..=13 {
            detected = stalled_publication_monitor.tick(
                clock(second * 1_000, second * 1_000),
                installed,
                |_| true,
            );
        }
        assert!(matches!(
            detected,
            LivenessTransitions::One(LivenessTransition::Hung { .. })
        ));
        atomic_acknowledgement.sequence.store(9, Ordering::Relaxed);
        atomic_acknowledgement
            .monotonic_nanos
            .store(14_000_000_000, Ordering::Relaxed);
        atomic_acknowledgement
            .unix_nanos
            .store(14_000_000_000, Ordering::Relaxed);
        atomic_acknowledgement
            .version
            .fetch_add(1, Ordering::Release);
        assert_eq!(atomic_acknowledgement.try_load().unwrap().sequence, 9);
    }

    #[test]
    fn s1_contract_38_acknowledgement_snapshot_never_mixes_publications() {
        let acknowledgement = AtomicAcknowledgement::new();
        let writer_done = AtomicBool::new(false);
        let first_published = AtomicBool::new(false);
        let first_observed = AtomicBool::new(false);
        let midpoint_publication_started = AtomicBool::new(false);
        let in_progress_snapshot_rejected = AtomicBool::new(false);
        let midpoint_observed = AtomicBool::new(false);
        let start = Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                assert!(acknowledgement.record(
                    1,
                    ClockSample {
                        monotonic: Duration::from_nanos(3),
                        suspend_aware: UNIX_EPOCH + Duration::from_nanos(7),
                    },
                ));
                first_published.store(true, Ordering::Release);
                while !first_observed.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                for sequence in 2..=100_000_u64 {
                    let sample = ClockSample {
                        monotonic: Duration::from_nanos(sequence * 3),
                        suspend_aware: UNIX_EPOCH + Duration::from_nanos(sequence * 7),
                    };
                    if sequence == 50_000 {
                        assert!(acknowledgement.record_with_in_progress_hook(
                            sequence,
                            sample,
                            || {
                                midpoint_publication_started.store(true, Ordering::Release);
                                while !in_progress_snapshot_rejected.load(Ordering::Acquire) {
                                    std::thread::yield_now();
                                }
                            },
                        ));
                    } else {
                        assert!(acknowledgement.record(sequence, sample));
                    }
                    if sequence.is_multiple_of(64) {
                        std::thread::yield_now();
                    }
                    if sequence == 50_000 {
                        while !midpoint_observed.load(Ordering::Acquire) {
                            std::thread::yield_now();
                        }
                    }
                }
                writer_done.store(true, Ordering::Release);
            });
            scope.spawn(|| {
                start.wait();
                let assert_consistent = |sample: AcknowledgementSample| {
                    if sample.sequence == 0 {
                        assert_eq!(sample, AcknowledgementSample::initial());
                        return;
                    }
                    let observed = sample
                        .acknowledged_at
                        .expect("a positive sequence must carry both clocks");
                    assert_eq!(
                        observed.monotonic,
                        Duration::from_nanos(sample.sequence * 3)
                    );
                    assert_eq!(
                        observed.suspend_aware.duration_since(UNIX_EPOCH).unwrap(),
                        Duration::from_nanos(sample.sequence * 7)
                    );
                };
                while !first_published.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                loop {
                    if let Some(sample) = acknowledgement.try_load()
                        && sample.sequence == 1
                    {
                        assert_consistent(sample);
                        break;
                    }
                }
                first_observed.store(true, Ordering::Release);
                while !writer_done.load(Ordering::Acquire) {
                    if midpoint_publication_started.load(Ordering::Acquire)
                        && !in_progress_snapshot_rejected.load(Ordering::Acquire)
                    {
                        assert!(acknowledgement.try_load().is_none());
                        in_progress_snapshot_rejected.store(true, Ordering::Release);
                        continue;
                    }
                    if let Some(sample) = acknowledgement.try_load() {
                        assert_consistent(sample);
                        if sample.sequence == 50_000 {
                            midpoint_observed.store(true, Ordering::Release);
                        }
                    }
                }
                assert_consistent(
                    acknowledgement
                        .try_load()
                        .expect("the terminal publication must be stable"),
                );
            });
        });
        assert!(midpoint_publication_started.load(Ordering::Acquire));
        assert!(in_progress_snapshot_rejected.load(Ordering::Acquire));
        assert!(midpoint_observed.load(Ordering::Acquire));
    }

    #[test]
    fn s1_contract_10_gap_sleep_regression_and_sequence_mismatch_reset_epoch() {
        let reset_cases = [
            (clock(10_000, 10_000), 1),
            (clock(2_000, 10_000), 1),
            (clock(500, 2_000), 1),
            (clock(2_000, 2_000), 999),
        ];

        for (reset_at, acknowledgement_sequence) in reset_cases {
            let mut monitor = LivenessMonitor::new(INTERVAL, THRESHOLD);
            assert_eq!(
                monitor.tick(clock(0, 0), AcknowledgementSample::initial(), |_| true),
                LivenessTransitions::None
            );
            assert!(matches!(
                monitor.tick(
                    clock(1_000, 1_000),
                    acknowledgement(1, clock(1_000, 1_000)),
                    |_| true
                ),
                LivenessTransitions::One(LivenessTransition::Ready { .. })
            ));
            let reset_acknowledgement = if acknowledgement_sequence == 1 {
                acknowledgement(1, clock(1_000, 1_000))
            } else {
                acknowledgement(acknowledgement_sequence, reset_at)
            };
            assert_eq!(
                monitor.tick(reset_at, reset_acknowledgement, |_| true),
                LivenessTransitions::None
            );
            assert_eq!(
                monitor.tick(clock(16_000, 16_000), reset_acknowledgement, |_| true,),
                LivenessTransitions::None,
                "reset case emitted a false hang"
            );
        }
    }

    #[test]
    fn s1_contract_11_lifecycle_rejects_prelaunch_and_postterminal_facts() {
        let mut producer = LifecycleProducer::new();
        let mut sink = CapturingSink::default();
        let ready = LivenessTransition::Ready {
            baseline_latency: Duration::from_millis(12),
            threshold: THRESHOLD,
            event_time: at(0),
        };

        assert_eq!(
            producer.offer_liveness(&mut sink, ready.clone(), at(0)),
            LifecycleOffer::RejectedTransition
        );
        assert_eq!(
            producer.offer_launch(&mut sink, at(0)),
            LifecycleOffer::Accepted
        );
        assert_eq!(
            producer.offer_liveness(&mut sink, ready, at(1)),
            LifecycleOffer::Accepted
        );
        assert_eq!(
            producer.offer_clean_exit(&mut sink, at(2)),
            LifecycleOffer::Accepted
        );
        assert_eq!(
            producer.offer_liveness(
                &mut sink,
                LivenessTransition::Hung {
                    duration: THRESHOLD,
                    threshold: THRESHOLD,
                    missed_intervals: 5,
                    event_time: at(3),
                },
                at(3),
            ),
            LifecycleOffer::RejectedTransition
        );
        assert_eq!(
            event_names(&sink.events),
            [
                EventName::AppLaunch,
                EventName::LivenessReady,
                EventName::AppProcessExit,
            ]
        );

        let mut ordered = LifecycleProducer::new();
        let mut scripted =
            CapturingSink::with_script([None, None, Some(DropReason::QueueFull), None]);
        assert_eq!(
            ordered.offer_launch(&mut scripted, at(0)),
            LifecycleOffer::Accepted
        );
        let outcomes = ordered.offer_liveness_batch(
            &mut scripted,
            LivenessTransitions::Pair(
                LivenessTransition::Hung {
                    duration: THRESHOLD,
                    threshold: THRESHOLD,
                    missed_intervals: 5,
                    event_time: at(5),
                },
                LivenessTransition::Recovered {
                    duration: Duration::from_secs(6),
                    threshold: THRESHOLD,
                    missed_intervals: 6,
                    event_time: at(6),
                },
            ),
        );
        assert_eq!(
            outcomes,
            [
                Some(LifecycleOffer::Accepted),
                Some(LifecycleOffer::Dropped(DropReason::QueueFull)),
            ]
        );
        assert_eq!(
            ordered.offer_clean_exit(&mut scripted, at(7)),
            LifecycleOffer::Accepted
        );
        assert_eq!(
            scripted
                .events
                .iter()
                .map(PendingEvent::sequence)
                .collect::<Vec<_>>(),
            [1, 2, 4],
            "every pair member consumes sequence before a terminal can follow"
        );
    }

    #[test]
    fn s1_contract_12_quit_offer_is_bounded_and_attempted_at_most_once() {
        let mut producer = LifecycleProducer::new();
        let mut accepting_sink = CapturingSink::default();
        assert_eq!(
            producer.offer_launch(&mut accepting_sink, at(0)),
            LifecycleOffer::Accepted
        );
        let mut full_sink = CapturingSink::with_drops([DropReason::QueueFull]);
        assert_eq!(
            producer.offer_clean_exit(&mut full_sink, at(1)),
            LifecycleOffer::Dropped(DropReason::QueueFull)
        );
        assert_eq!(
            producer.offer_clean_exit(&mut full_sink, at(2)),
            LifecycleOffer::RejectedTransition
        );
        assert_eq!(producer.terminal_offer_count(), 1);
    }

    #[test]
    fn s1_contract_13_duplicate_quit_and_restart_callbacks_cannot_duplicate_terminal() {
        let mut producer = LifecycleProducer::new();
        let mut sink = CapturingSink::default();
        assert_eq!(
            producer.offer_launch(&mut sink, at(0)),
            LifecycleOffer::Accepted
        );
        assert_eq!(
            producer.offer_clean_exit(&mut sink, at(1)),
            LifecycleOffer::Accepted
        );
        for _ in 0..4 {
            assert_eq!(
                producer.offer_clean_exit(&mut sink, at(2)),
                LifecycleOffer::RejectedTransition
            );
            assert_eq!(
                producer.offer_launch(&mut sink, at(2)),
                LifecycleOffer::RejectedTransition
            );
        }
        assert_eq!(
            event_names(&sink.events)
                .into_iter()
                .filter(|name| *name == EventName::AppProcessExit)
                .count(),
            1
        );
    }

    #[test]
    fn s1_contract_13a_shutdown_never_waits_for_queue_space() {
        let mut producer = LifecycleProducer::new();
        let mut launch_sink = CapturingSink::default();
        assert_eq!(
            producer.offer_launch(&mut launch_sink, at(0)),
            LifecycleOffer::Accepted
        );
        let (mut ingress, receiver) = RecorderIngress::bounded(1);
        assert_eq!(
            ingress.try_offer(PendingEvent::new(99, EventFact::AppLaunch, at(0))),
            SinkOffer::Accepted
        );

        assert_eq!(
            producer.offer_clean_exit(&mut ingress, at(1)),
            LifecycleOffer::Dropped(DropReason::QueueFull)
        );
        assert_eq!(producer.terminal_offer_count(), 1);
        assert!(matches!(
            receiver.recv().unwrap().fact(),
            EventFact::AppLaunch
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn s1_contract_13b_shutdown_deadline_detaches_a_stalled_writer() {
        let (release_sender, release_receiver) = mpsc::channel();
        let (completed_sender, completed) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _completion = WriterCompletion(completed_sender);
            release_receiver.recv().unwrap();
        });
        let writer = WriterThread {
            handle: Some(handle),
            completed,
        };

        assert!(!writer.join_until(Instant::now()));
        release_sender.send(()).unwrap();
    }

    #[test]
    fn s1_contract_14_lost_lifecycle_offers_are_partial_and_never_fabricated() {
        for loss in [DropReason::QueueFull, DropReason::StorageUnavailable] {
            let mut producer = LifecycleProducer::new();
            let mut launch_drop = CapturingSink::with_drops([loss]);
            assert!(matches!(
                producer.offer_launch(&mut launch_drop, at(0)),
                LifecycleOffer::Dropped(_)
            ));
            let mut admitted_liveness = CapturingSink::default();
            assert_eq!(
                producer.offer_liveness(
                    &mut admitted_liveness,
                    LivenessTransition::Ready {
                        baseline_latency: Duration::from_millis(12),
                        threshold: THRESHOLD,
                        event_time: at(1),
                    },
                    at(1),
                ),
                LifecycleOffer::Accepted
            );
            let mut terminal_drop = CapturingSink::with_drops([loss]);
            assert!(matches!(
                producer.offer_clean_exit(&mut terminal_drop, at(2)),
                LifecycleOffer::Dropped(_)
            ));
            assert_eq!(producer.coverage(), LifecycleCoverage::Partial);
            assert_eq!(
                event_names(&admitted_liveness.events),
                [EventName::LivenessReady]
            );
            assert_eq!(admitted_liveness.events[0].sequence(), 2);
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_15_new_launch_preserves_torn_predecessor_without_inference() {
        let root = TestRoot::new("torn-predecessor");
        let config = recorder_config(&root);
        let first_launch = launch_identity();
        let (mut first_ingress, first_writer) = spawn_writer(config.clone(), first_launch).unwrap();
        let mut first_producer = LifecycleProducer::new();
        assert_eq!(
            first_producer.offer_launch(&mut first_ingress, at(0)),
            LifecycleOffer::Accepted
        );
        drop(first_ingress);
        first_writer.join().unwrap();
        let first_path = native_shards(&root)
            .into_iter()
            .next()
            .expect("background writer must persist launch");
        let persisted_launch = fs::read(&first_path).unwrap();
        assert!(validate_event_line(&persisted_launch).is_ok());
        assert_eq!(parsed(&persisted_launch)["attributes"]["event.sequence"], 1);
        use std::io::Write as _;
        fs::OpenOptions::new()
            .append(true)
            .open(&first_path)
            .unwrap()
            .write_all(b"{\"torn\":")
            .unwrap();
        let predecessor = fs::read(&first_path).unwrap();

        let second_launch = other_launch_identity();
        let (mut second_ingress, second_writer) = spawn_writer(config, second_launch).unwrap();
        let mut second_producer = LifecycleProducer::new();
        assert_eq!(
            second_producer.offer_launch(&mut second_ingress, at(10)),
            LifecycleOffer::Accepted
        );
        drop(second_ingress);
        second_writer.join().unwrap();
        let second_path = native_shards(&root)
            .into_iter()
            .find(|path| path != &first_path)
            .expect("second background writer must use a fresh shard");
        assert_ne!(first_path, second_path);
        assert_eq!(fs::read(&first_path).unwrap(), predecessor);
        let all_bytes = fs::read(&first_path)
            .unwrap()
            .into_iter()
            .chain(fs::read(&second_path).unwrap())
            .collect::<Vec<_>>();
        assert!(!String::from_utf8_lossy(&all_bytes).contains("app.process_exit"));
    }

    #[test]
    fn s1_contract_16_all_restart_disable_spellings_prevent_worker_and_store_creation() {
        for spelling in ["1", "true", "TRUE", "yes", "Yes", "on", "ON"] {
            assert!(restart_telemetry_disabled(Some(spelling)));
            let root = TestRoot::new("restart-disabled");
            let mut disabled_config = recorder_config(&root);
            disabled_config.set_disabled_at_restart_for_test(true);
            assert!(matches!(
                spawn_writer(disabled_config, launch_identity()),
                Err(StoreOpenError::DisabledAtRestart)
            ));
            assert!(!root.store().exists());
        }

        for enabled in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
            Some("unexpected"),
        ] {
            assert!(!restart_telemetry_disabled(enabled));
        }

        #[cfg(unix)]
        {
            let root = TestRoot::new("restart-disabled-then-enabled-retention");
            let observed = at(20 * 86_400);
            let expired = day(observed).checked_sub_days(Days::new(15)).unwrap();
            let expired_path = nested_shard_path(&root, expired, WRITER_ID);
            write_private(&expired_path, b"expired\n");
            let before = fs::read(&expired_path).unwrap();

            let mut disabled_config = recorder_config(&root);
            disabled_config.set_disabled_at_restart_for_test(true);
            assert!(matches!(
                TestRecorder::open(disabled_config, launch_identity()),
                Err(StoreOpenError::DisabledAtRestart)
            ));
            assert_eq!(fs::read(&expired_path).unwrap(), before);

            let mut enabled =
                TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            assert!(enabled.run_retention_for_test(observed).removed >= 1);
            assert!(!expired_path.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_16a_shared_disabled_sentinel_stops_and_resumes_live_writer() {
        let root = TestRoot::new("shared-disabled-sentinel");
        let sentinel = root.path.join("DISABLED");
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();

        write_private(&sentinel, b"disabled by operator\n");
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(1), at(1)),
            AppendOutcome::Dropped(DropReason::Disabled)
        );
        assert!(!root.store().exists());

        fs::remove_file(&sentinel).unwrap();
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(2), at(2)),
            AppendOutcome::Appended
        );
        let shard = writer.current_shard_path().unwrap().to_path_buf();
        let before = fs::read(&shard).unwrap();

        write_private(&sentinel, b"disabled by operator\n");
        assert_eq!(
            writer.append(EventFact::AppProcessExit, at(3), at(3)),
            AppendOutcome::Dropped(DropReason::Disabled)
        );
        assert_eq!(fs::read(&shard).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_17_restart_flag_is_snapshotted_and_cannot_reconfigure_writer() {
        let build_identity = SourceIdentity::from_build().expect("compile-time identity is valid");
        assert_eq!(build_identity.service_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            build_identity.build_version,
            option_env!("ZED_BUILD_ID").unwrap_or("local")
        );
        assert_eq!(
            build_identity.commit_sha.as_deref(),
            option_env!("ZED_COMMIT_SHA")
        );
        let app_config = RecorderConfig::for_app(build_identity, false);
        assert_eq!(
            app_config
                .store_path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("zed10x-in-process")
        );
        assert_eq!(
            app_config
                .store_path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some("dogfood-canary")
        );
        assert_eq!(app_config.source.service_version, env!("CARGO_PKG_VERSION"));

        let root = TestRoot::new("restart-snapshot");
        let mut later_ambient_value = "off";
        assert!(!restart_telemetry_disabled(Some(later_ambient_value)));
        let mut later_config = recorder_config(&root);
        let mut writer = TestRecorder::open(later_config.clone(), launch_identity()).unwrap();
        let admitted_writer_id = writer.writer_id().to_owned();

        later_ambient_value = "true";
        later_config.set_disabled_at_restart_for_test(restart_telemetry_disabled(Some(
            later_ambient_value,
        )));
        assert!(restart_telemetry_disabled(Some(later_ambient_value)));
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(1), at(1)),
            AppendOutcome::Appended
        );
        assert_eq!(writer.writer_id(), admitted_writer_id);
        let line = fs::read(writer.current_shard_path().unwrap()).unwrap();
        assert_eq!(parsed(&line)["attributes"]["zed.lane"], "local");
        assert_eq!(parsed(&line)["attributes"]["zed.source"], "in_process");
        assert!(
            !String::from_utf8(line)
                .unwrap()
                .contains(later_ambient_value)
        );
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_18_private_creation_and_custody_reject_owner_mode_or_type_drift() {
        let private_root = CustodyFacts {
            kind: StoreEntryKind::Directory,
            owner_matches: true,
            mode: 0o700,
            link_count: 1,
        };
        assert!(admit_store_custody(StoreRole::Root, private_root));
        assert!(admit_store_custody(
            StoreRole::Shard,
            CustodyFacts {
                kind: StoreEntryKind::RegularFile,
                owner_matches: true,
                mode: 0o600,
                link_count: 1,
            }
        ));
        for unsafe_facts in [
            CustodyFacts {
                owner_matches: false,
                ..private_root
            },
            CustodyFacts {
                mode: 0o755,
                ..private_root
            },
            CustodyFacts {
                kind: StoreEntryKind::Symlink,
                ..private_root
            },
            CustodyFacts {
                kind: StoreEntryKind::RegularFile,
                ..private_root
            },
            CustodyFacts {
                link_count: 2,
                ..private_root
            },
        ] {
            assert!(!admit_store_custody(StoreRole::Root, unsafe_facts));
            assert!(!admit_store_custody(
                StoreRole::PrivateDirectory,
                unsafe_facts
            ));
        }

        let root = TestRoot::new("private-create");
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(0), at(0)),
            AppendOutcome::Appended
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(root.store()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(writer.current_shard_path().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    #[test]
    fn s1_contract_18_private_creation_and_custody_reject_owner_mode_or_type_drift() {
        let root = TestRoot::new("unsupported-platform");
        assert!(matches!(
            TestRecorder::open(recorder_config(&root), launch_identity()),
            Err(StoreOpenError::UnsupportedPlatform)
        ));
        assert!(!root.store().exists());
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_19_symlink_hardlink_and_replacement_revoke_append_custody() {
        #[cfg(unix)]
        {
            let root = TestRoot::new("static-symlink");
            let victim = root.path.join("victim");
            fs::create_dir(&victim).unwrap();
            std::os::unix::fs::symlink(&victim, root.store()).unwrap();
            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            assert_eq!(
                writer.append(EventFact::AppLaunch, at(0), at(0)),
                AppendOutcome::Dropped(DropReason::UnsafeCustody)
            );
            assert_eq!(fs::read_dir(victim).unwrap().count(), 0);
        }

        let hardlink_root = TestRoot::new("hardlink");
        let mut hardlink_writer =
            TestRecorder::open(recorder_config(&hardlink_root), launch_identity()).unwrap();
        assert_eq!(
            hardlink_writer.append(EventFact::AppLaunch, at(0), at(0)),
            AppendOutcome::Appended
        );
        let shard = hardlink_writer.current_shard_path().unwrap().to_path_buf();
        fs::hard_link(&shard, hardlink_root.path.join("second-link")).unwrap();
        let before = fs::read(&shard).unwrap();
        assert_eq!(
            hardlink_writer.append(
                EventFact::LivenessReady {
                    baseline_latency: Duration::from_millis(12),
                    threshold: THRESHOLD,
                },
                at(1),
                at(1),
            ),
            AppendOutcome::Dropped(DropReason::UnsafeCustody)
        );
        assert_eq!(fs::read(&shard).unwrap(), before);

        let replacement_root = TestRoot::new("replacement");
        let mut replacement_writer =
            TestRecorder::open(recorder_config(&replacement_root), launch_identity()).unwrap();
        assert_eq!(
            replacement_writer.append(EventFact::AppLaunch, at(0), at(0)),
            AppendOutcome::Appended
        );
        let shard = replacement_writer
            .current_shard_path()
            .unwrap()
            .to_path_buf();
        let displaced = replacement_root.path.join("displaced");
        fs::rename(&shard, &displaced).unwrap();
        write_private(&shard, b"replacement\n");
        let displaced_before = fs::read(&displaced).unwrap();
        let replacement_before = fs::read(&shard).unwrap();
        assert_eq!(
            replacement_writer.append(EventFact::AppLaunch, at(2), at(2)),
            AppendOutcome::Dropped(DropReason::UnsafeCustody)
        );
        assert_eq!(fs::read(displaced).unwrap(), displaced_before);
        assert_eq!(fs::read(shard).unwrap(), replacement_before);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_20_observation_day_routes_shard_and_regression_never_backroutes() {
        let root = TestRoot::new("observation-day");
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        let observed_day = day(at(86_400));
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(0), at(86_400)),
            AppendOutcome::Appended
        );
        assert_eq!(writer.current_shard_day(), Some(observed_day));
        let first_path = writer.current_shard_path().unwrap().to_path_buf();
        let first_bytes = fs::read(&first_path).unwrap();

        assert_eq!(
            writer.append(
                EventFact::LivenessReady {
                    baseline_latency: Duration::from_millis(12),
                    threshold: THRESHOLD,
                },
                at(172_800),
                at(172_800),
            ),
            AppendOutcome::Appended
        );
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(10), at(10)),
            AppendOutcome::Dropped(DropReason::ObservationTimeRegression)
        );
        assert_eq!(fs::read(first_path).unwrap(), first_bytes);
        assert_eq!(writer.current_shard_day(), Some(day(at(172_800))));
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_20a_observation_time_never_precedes_occurrence_after_clock_rollback() {
        let root = TestRoot::new("observation-clock-rollback");
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();

        assert_eq!(
            writer.append(EventFact::AppLaunch, at(10), at(5)),
            AppendOutcome::Appended
        );

        let shard = native_shards(&root).into_iter().next().unwrap();
        let event = parsed(&fs::read(shard).unwrap());
        let expected = ((BASE_UNIX_SECONDS as u128 + 10) * 1_000_000_000).to_string();
        assert_eq!(event["time_unix_nano"], expected);
        assert_eq!(event["observed_time_unix_nano"], expected);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_21_rollover_closes_sleeping_descriptor_before_append() {
        let root = TestRoot::new("sleep-rollover");
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(0), at(0)),
            AppendOutcome::Appended
        );
        let old_path = writer.current_shard_path().unwrap().to_path_buf();
        let old_bytes = fs::read(&old_path).unwrap();
        let old_generation = writer.open_shard_generation();

        assert_eq!(
            writer.append(
                EventFact::LivenessReady {
                    baseline_latency: Duration::from_millis(12),
                    threshold: THRESHOLD,
                },
                at(3 * 86_400),
                at(3 * 86_400),
            ),
            AppendOutcome::Appended
        );
        assert_eq!(writer.closed_shard_count(), 1);
        assert_eq!(writer.open_shard_generation(), old_generation + 1);
        assert_ne!(writer.current_shard_path(), Some(old_path.as_path()));
        assert_eq!(fs::read(old_path).unwrap(), old_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_22_startup_and_rollover_retention_are_once_and_fail_open() {
        let root = TestRoot::new("retention-cadence");
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        writer.inject_retention_failure_once();
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(0), at(0)),
            AppendOutcome::Appended
        );
        assert_eq!(writer.retention_run_count(), 1);
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(1), at(1)),
            AppendOutcome::Appended
        );
        assert_eq!(writer.retention_run_count(), 1);

        assert_eq!(
            writer.append(EventFact::AppLaunch, at(86_400), at(86_400)),
            AppendOutcome::Appended
        );
        assert_eq!(writer.retention_run_count(), 2);
        assert_eq!(
            writer.append(EventFact::AppLaunch, at(86_401), at(86_401)),
            AppendOutcome::Appended
        );
        assert_eq!(writer.retention_run_count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_23_retention_is_inclusive_capped_source_scoped_and_nonblocking() {
        let root = TestRoot::new("retention-scope");
        create_private_directory(&root.store());
        for index in 0..100 {
            write_private(
                &root.store().join(format!("unowned-root-{index:03}")),
                b"root-junk\n",
            );
        }

        let observed = at(80 * 86_400);
        let today = day(observed);
        let legacy_flat_path = root.store().join(owned_shard_name(
            today.checked_sub_days(Days::new(30)).unwrap(),
            WRITER_ID,
        ));
        write_private(&legacy_flat_path, b"legacy-flat-observer\n");
        let retained = today.checked_sub_days(Days::new(13)).unwrap();
        let current_paths = (0..100)
            .map(|index| {
                let writer = format!("{index:032x}");
                let path = nested_shard_path(&root, today, &writer);
                write_private(&path, b"current\n");
                path
            })
            .collect::<Vec<_>>();
        let retained_paths = (100..200)
            .map(|index| {
                let writer = format!("{index:032x}");
                let path = nested_shard_path(&root, retained, &writer);
                write_private(&path, b"retained\n");
                path
            })
            .collect::<Vec<_>>();
        let expired_days = [14, 15, 16].map(|age| today.checked_sub_days(Days::new(age)).unwrap());
        let expired_day_paths = expired_days.map(|expired_day| {
            root.store()
                .join(slot_directory_name(retention_slot_index(
                    expired_day,
                    TEST_RETENTION_DAYS,
                )))
                .join(day_directory_name(expired_day))
        });
        let mut expired_paths = Vec::new();
        for (day_index, expired_day) in expired_days.into_iter().enumerate() {
            for index in 0..(if day_index == 0 { 9 } else { 2 }) {
                let writer = format!("{:032x}", 300 + day_index * 16 + index);
                let path = nested_shard_path(&root, expired_day, &writer);
                write_private(&path, b"expired\n");
                expired_paths.push(path);
            }
        }
        let future = today.succ_opt().unwrap();
        let future_path = nested_shard_path(&root, future, WRITER_ID);
        write_private(&future_path, b"future\n");

        let mut remaining = expired_paths.iter().filter(|path| path.exists()).count();
        for _ in 0..64 {
            if remaining == 0 {
                break;
            }
            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            let report = writer.run_retention_for_test(observed);
            assert!(report.scanned <= 64);
            assert_eq!(report.slot_scanned.iter().sum::<usize>(), report.scanned);
            assert!(report.slot_scanned.iter().all(|scanned| *scanned <= 5));
            let next = expired_paths.iter().filter(|path| path.exists()).count();
            assert!(
                next < remaining,
                "each fresh launch must remove expired work"
            );
            remaining = next;
        }
        assert_eq!(
            remaining, 0,
            "fresh launches must converge without a cursor"
        );
        assert!(current_paths.iter().all(|path| path.exists()));
        assert!(retained_paths.iter().all(|path| path.exists()));
        assert!(future_path.exists());
        assert_eq!(
            fs::read(&legacy_flat_path).unwrap(),
            b"legacy-flat-observer\n"
        );
        assert_eq!(
            fs::read_dir(root.store())
                .unwrap()
                .flatten()
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("unowned-root-"))
                .count(),
            100
        );

        let crash_cut_day = today.checked_sub_days(Days::new(18)).unwrap();
        let crash_cut_leaf = nested_shard_path(&root, crash_cut_day, WRITER_ID);
        write_private(&crash_cut_leaf, b"deleted-before-crash\n");
        fs::remove_file(&crash_cut_leaf).unwrap();
        let crash_cut_directory = crash_cut_leaf.parent().unwrap().to_path_buf();
        assert!(crash_cut_directory.exists());
        assert_eq!(fs::read_dir(&crash_cut_directory).unwrap().count(), 0);

        for _ in 0..64 {
            if expired_day_paths.iter().all(|path| !path.exists()) && !crash_cut_directory.exists()
            {
                break;
            }
            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            writer.run_retention_for_test(observed);
        }
        assert!(
            expired_day_paths.iter().all(|path| !path.exists()),
            "a fresh process must finish empty expired-day removal after the leaf crash cut"
        );
        assert!(
            !crash_cut_directory.exists(),
            "a fresh process must remove the explicit empty-day crash-cut fixture"
        );

        let reaged_observed = observed + Duration::from_secs(86_400);
        let mut reaged_remaining = retained_paths.iter().filter(|path| path.exists()).count();
        for _ in 0..128 {
            if reaged_remaining == 0 {
                break;
            }
            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            let report = writer.run_retention_for_test(reaged_observed);
            assert!(report.scanned <= 64);
            let next = retained_paths.iter().filter(|path| path.exists()).count();
            assert!(
                next < reaged_remaining,
                "a retained day must make progress after it expires"
            );
            reaged_remaining = next;
        }
        assert_eq!(reaged_remaining, 0);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_24_observed_pre_unlink_replacement_revokes_deletion() {
        let root = TestRoot::new("unlink-race");
        let today = day(at(20 * 86_400));
        let old_day = today.checked_sub_days(Days::new(15)).unwrap();
        let candidate = nested_shard_path(&root, old_day, WRITER_ID);
        write_private(&candidate, b"original\n");

        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        writer.inject_unlink_replacement_race(candidate.clone(), b"replacement\n".to_vec());
        let report = writer.run_retention_for_test(at(20 * 86_400));
        let displaced = candidate.with_extension("jsonl.displaced");
        assert_eq!(report.removed, 0);
        assert_eq!(report.revoked, 1);
        assert_eq!(fs::read(candidate).unwrap(), b"replacement\n");
        assert_eq!(fs::read(displaced).unwrap(), b"original\n");
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_24a_final_pre_unlink_replacement_revokes_deletion() {
        let root = TestRoot::new("final-unlink-race");
        let today = day(at(20 * 86_400));
        let old_day = today.checked_sub_days(Days::new(15)).unwrap();
        let candidate = nested_shard_path(&root, old_day, WRITER_ID);
        write_private(&candidate, b"original\n");

        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        writer.inject_final_unlink_replacement_race(candidate.clone(), b"replacement\n".to_vec());
        let report = writer.run_retention_for_test(at(20 * 86_400));
        let displaced = candidate.with_extension("jsonl.final-displaced");
        assert_eq!(report.removed, 0);
        assert_eq!(report.revoked, 1);
        assert_eq!(fs::read(candidate).unwrap(), b"replacement\n");
        assert_eq!(fs::read(displaced).unwrap(), b"original\n");
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_24b_final_day_replacement_revokes_directory_deletion() {
        let root = TestRoot::new("final-day-removal-race");
        let today = day(at(20 * 86_400));
        let old_day = today.checked_sub_days(Days::new(15)).unwrap();
        let candidate = nested_shard_path(&root, old_day, WRITER_ID);
        write_private(&candidate, b"original\n");
        let day_directory = candidate.parent().unwrap().to_path_buf();
        let displaced = day_directory.with_file_name(format!(
            "{}-displaced",
            day_directory.file_name().unwrap().to_string_lossy()
        ));

        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        writer.inject_final_directory_removal_replacement_race(day_directory.clone());
        let report = writer.run_retention_for_test(at(20 * 86_400));

        assert_eq!(report.removed, 1);
        assert_eq!(report.revoked, 1);
        assert!(day_directory.exists());
        assert!(displaced.exists());
        assert_eq!(fs::read_dir(day_directory).unwrap().count(), 0);
        assert_eq!(fs::read_dir(displaced).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_24c_restart_completes_file_quarantine_deletion() {
        let root = TestRoot::new("file-quarantine-recovery");
        let observed = at(20 * 86_400);
        let expired_day = day(observed).checked_sub_days(Days::new(14)).unwrap();
        let candidate = nested_shard_path(&root, expired_day, WRITER_ID);
        write_private(&candidate, b"expired\n");
        let quarantine = candidate.with_file_name(format!(
            "{}{}",
            super::RETENTION_FILE_QUARANTINE_PREFIX,
            "c".repeat(32)
        ));
        fs::rename(&candidate, &quarantine).unwrap();

        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        let report = writer.run_retention_for_test(observed);

        assert!(report.removed >= 2);
        assert_eq!(report.revoked, 0);
        assert!(!quarantine.exists());
        assert!(!candidate.parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_24d_restart_completes_directory_quarantine_deletion() {
        let root = TestRoot::new("directory-quarantine-recovery");
        let observed = at(20 * 86_400);
        let expired_day = day(observed).checked_sub_days(Days::new(14)).unwrap();
        let candidate = nested_shard_path(&root, expired_day, WRITER_ID);
        write_private(&candidate, b"expired\n");
        fs::remove_file(&candidate).unwrap();
        let day_directory = candidate.parent().unwrap().to_path_buf();
        let quarantine = day_directory.with_file_name(format!(
            "{}{}",
            super::RETENTION_DIRECTORY_QUARANTINE_PREFIX,
            "d".repeat(32)
        ));
        fs::rename(&day_directory, &quarantine).unwrap();

        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        let report = writer.run_retention_for_test(observed);

        assert_eq!(report.removed, 1);
        assert_eq!(report.revoked, 0);
        assert!(!quarantine.exists());
    }

    #[cfg(unix)]
    #[gpui::test]
    fn s1_contract_25_configured_production_assembly_persists_contiguous_lifecycle(
        cx: &mut TestAppContext,
    ) {
        let root = TestRoot::new("production-assembly");
        let manual_clock = Arc::new(ManualClock::new(clock(0, 0)));
        let mut monitor = cx.update(|cx| {
            start_configured(
                cx,
                recorder_config(&root),
                launch_identity(),
                manual_clock.clone(),
            )
            .unwrap()
        });

        monitor.poll();
        manual_clock.set(clock(10, 10));
        cx.run_until_parked();

        manual_clock.set(clock(1_000, 1_000));
        monitor.poll();
        for second in 2..=6 {
            manual_clock.set(clock(second * 1_000, second * 1_000));
            monitor.poll();
        }

        manual_clock.set(clock(6_100, 6_100));
        cx.run_until_parked();
        manual_clock.set(clock(6_200, 6_200));
        monitor.poll();

        manual_clock.set(clock(7_000, 7_000));
        cx.update(|cx| cx.shutdown());
        // The quit future owns the writer and cannot resolve until the terminal event has been
        // drained and the writer joined. Monitor deliberately remains alive while we inspect the
        // completed shard, proving cleanup does not rely on dropping its producer handle.

        let shards = native_shards(&root);
        assert_eq!(shards.len(), 1);
        let bytes = fs::read(&shards[0]).unwrap();
        let events = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .map(parsed)
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .map(|event| event["body"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "app.launch",
                "app.liveness.ready",
                "app.hang",
                "app.hang.recovered",
                "app.process_exit",
            ]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event["attributes"]["event.sequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        for event in &events {
            assert_eq!(event["trace_id"], TRACE_ID);
            assert_eq!(event["span_id"], SPAN_ID);
            assert_eq!(event["attributes"]["zed.writer_id"], WRITER_ID);
            let occurrence = event["time_unix_nano"]
                .as_str()
                .unwrap()
                .parse::<u128>()
                .unwrap();
            let observation = event["observed_time_unix_nano"]
                .as_str()
                .unwrap()
                .parse::<u128>()
                .unwrap();
            assert!(observation >= occurrence);
        }
        assert_eq!(
            events.last().unwrap()["attributes"]["lifecycle.state"],
            "clean_exit"
        );
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_26_real_writer_preserves_sequence_occurrence_and_observation_clocks() {
        let root = TestRoot::new("real-writer-dual-clock");
        let manual_clock = Arc::new(ManualClock::new(clock(10_000, 10_000)));
        let launch = launch_identity();
        let (mut ingress, writer) =
            spawn_writer_with_clock(recorder_config(&root), launch, manual_clock).unwrap();
        let mut producer = LifecycleProducer::new();
        let mut launch_sink = CapturingSink::default();
        assert_eq!(
            producer.offer_launch(&mut launch_sink, clock(1_000, 1_000).suspend_aware),
            LifecycleOffer::Accepted
        );
        assert_eq!(
            producer.offer_liveness(
                &mut ingress,
                LivenessTransition::Ready {
                    baseline_latency: Duration::from_millis(10),
                    threshold: THRESHOLD,
                    event_time: clock(2_000, 2_000).suspend_aware,
                },
                clock(2_000, 2_000).suspend_aware,
            ),
            LifecycleOffer::Accepted
        );
        drop(ingress);
        writer.join().unwrap();

        let shard = native_shards(&root).into_iter().next().unwrap();
        let event = parsed(&fs::read(shard).unwrap());
        assert_eq!(event["attributes"]["event.sequence"], 2);
        assert_eq!(event["time_unix_nano"], "2000000000");
        assert_eq!(event["observed_time_unix_nano"], "10000000000");
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_27_native_shards_are_partitioned_by_fixed_slot_and_utc_day() {
        let root = TestRoot::new("fixed-slot-layout");
        let observed = at(20 * 86_400);
        let observed_day = day(observed);
        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        assert_eq!(
            writer.append(EventFact::AppLaunch, observed, observed),
            AppendOutcome::Appended
        );

        let shard = writer.current_shard_path().unwrap();
        let expected_day = format!("day-{}", observed_day.format("%Y%m%d"));
        let expected_slot =
            slot_directory_name(retention_slot_index(observed_day, TEST_RETENTION_DAYS));
        assert_eq!(
            shard
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some(expected_day.as_str())
        );
        assert_eq!(
            shard
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some(expected_slot.as_str())
        );
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(shard.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(shard.parent().unwrap().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(shard).unwrap().permissions().mode() & 0o777,
            0o600
        );

        for today in [
            NaiveDate::from_ymd_opt(2024, 3, 1).unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
        ] {
            let retained_slots = (0..TEST_RETENTION_DAYS)
                .map(|age| {
                    retention_slot_index(
                        today.checked_sub_days(Days::new(age as u64)).unwrap(),
                        TEST_RETENTION_DAYS,
                    )
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(retained_slots.len(), TEST_RETENTION_DAYS);
            assert_eq!(
                retention_slot_index(today, TEST_RETENTION_DAYS),
                retention_slot_index(
                    today
                        .checked_sub_days(Days::new(TEST_RETENTION_DAYS as u64))
                        .unwrap(),
                    TEST_RETENTION_DAYS,
                )
            );
        }
    }

    #[test]
    fn s1_contract_28_stale_acknowledgement_cannot_recover_before_hang_boundary() {
        let mut monitor = LivenessMonitor::new(INTERVAL, THRESHOLD);
        assert_eq!(
            monitor.tick(clock(0, 0), AcknowledgementSample::initial(), |_| true),
            LivenessTransitions::None
        );
        assert!(matches!(
            monitor.tick(
                clock(1_000, 1_000),
                acknowledgement(1, clock(10, 10)),
                |_| true,
            ),
            LivenessTransitions::One(LivenessTransition::Ready { .. })
        ));
        for second in 2..6 {
            assert_eq!(
                monitor.tick(
                    clock(second * 1_000, second * 1_000),
                    acknowledgement(1, clock(10, 10)),
                    |_| true,
                ),
                LivenessTransitions::None
            );
        }
        let hang = monitor
            .tick(
                clock(6_500, 6_500),
                acknowledgement(1, clock(10, 10)),
                |_| true,
            )
            .into_single()
            .unwrap();
        assert_eq!(hang.event_time(), clock(6_000, 6_000).suspend_aware);

        let recovery = monitor
            .tick(
                clock(6_700, 6_700),
                acknowledgement(2, clock(5_900, 5_900)),
                |_| true,
            )
            .into_single()
            .unwrap();
        let LivenessTransition::Recovered {
            duration,
            missed_intervals,
            event_time,
            ..
        } = recovery
        else {
            panic!("the stable acknowledgement must close the active episode");
        };
        assert_eq!(duration, THRESHOLD);
        assert_eq!(missed_intervals, 5);
        assert_eq!(event_time, clock(6_000, 6_000).suspend_aware);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_29_contaminated_slot_cannot_spend_a_clean_slot_quota() {
        let root = TestRoot::new("retention-slot-isolation");
        create_private_directory(&root.store());
        let observed = at(80 * 86_400);
        let today = day(observed);
        let dirty_slot_index = retention_slot_index(today, TEST_RETENTION_DAYS);
        let dirty_slot = root.store().join(slot_directory_name(dirty_slot_index));
        create_private_directory(&dirty_slot);
        let malformed_entries = (0..12)
            .map(|index| {
                let path = dirty_slot.join(format!("malformed-{index:02}"));
                let bytes = format!("malformed-{index:02}-sentinel\n").into_bytes();
                write_private(&path, &bytes);
                (path, bytes)
            })
            .collect::<Vec<_>>();
        let mis_slotted_day = today.checked_sub_days(Days::new(1)).unwrap();
        assert_ne!(
            retention_slot_index(mis_slotted_day, TEST_RETENTION_DAYS),
            dirty_slot_index
        );
        let mis_slotted_path = dirty_slot.join(day_directory_name(mis_slotted_day));
        create_private_directory(&mis_slotted_path);

        let clean_day = today.checked_sub_days(Days::new(16)).unwrap();
        let clean_slot_index = retention_slot_index(clean_day, TEST_RETENTION_DAYS);
        assert_ne!(dirty_slot_index, clean_slot_index);
        let clean_path = nested_shard_path(&root, clean_day, WRITER_ID);
        write_private(&clean_path, b"expired\n");

        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        let report = writer.run_retention_for_test(observed);
        assert!(!clean_path.exists());
        assert!(report.scanned <= 64);
        assert!(report.slot_scanned[dirty_slot_index] >= 4);
        assert!(report.slot_scanned[dirty_slot_index] <= 5);
        assert!(report.slot_scanned[clean_slot_index] <= 5);
        assert!(report.revoked >= report.slot_scanned[dirty_slot_index]);
        assert!(mis_slotted_path.exists());
        for (path, bytes) in malformed_entries {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_30_active_writer_and_fifo_are_nonblocking_and_restart_recoverable() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let active_root = TestRoot::new("retention-active-directory-lock");
        let old_observed = at(5 * 86_400);
        let current_observed = at(20 * 86_400);
        let mut active =
            TestRecorder::open(recorder_config(&active_root), other_launch_identity()).unwrap();
        assert_eq!(
            active.append(EventFact::AppLaunch, old_observed, old_observed),
            AppendOutcome::Appended
        );
        let active_path = active.current_shard_path().unwrap().to_path_buf();
        let mut cleaner =
            TestRecorder::open(recorder_config(&active_root), launch_identity()).unwrap();
        let active_report = cleaner.run_retention_for_test(current_observed);
        assert!(active_path.exists());
        assert!(active_report.revoked >= 1);
        drop(active);
        let mut restarted =
            TestRecorder::open(recorder_config(&active_root), launch_identity()).unwrap();
        let released_report = restarted.run_retention_for_test(current_observed);
        assert!(!active_path.exists());
        assert!(released_report.removed >= 1);

        let fifo_root = TestRoot::new("retention-fifo");
        let current_day = day(current_observed);
        let old_day = current_day.checked_sub_days(Days::new(15)).unwrap();
        let fifo_path = nested_shard_path(&fifo_root, old_day, WRITER_ID);
        let fifo_c = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let mut fifo_writer =
            TestRecorder::open(recorder_config(&fifo_root), launch_identity()).unwrap();
        let fifo_report = fifo_writer.run_retention_for_test(current_observed);
        assert_eq!(fifo_report.removed, 0);
        assert!(fifo_report.revoked >= 1);
        assert!(fifo_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_31_slot_or_day_replacement_revokes_append_custody() {
        for replace_slot in [true, false] {
            let root = TestRoot::new(if replace_slot {
                "slot-replacement"
            } else {
                "day-replacement"
            });
            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            assert_eq!(
                writer.append(EventFact::AppLaunch, at(0), at(0)),
                AppendOutcome::Appended
            );
            let shard = writer.current_shard_path().unwrap().to_path_buf();
            let day_directory = shard.parent().unwrap().to_path_buf();
            let slot = day_directory.parent().unwrap().to_path_buf();
            let replaced = if replace_slot { &slot } else { &day_directory };
            let displaced = replaced.with_file_name(format!(
                "{}-displaced",
                replaced.file_name().unwrap().to_string_lossy()
            ));
            fs::rename(replaced, &displaced).unwrap();
            create_private_directory(replaced);

            let displaced_shard = if replace_slot {
                displaced
                    .join(day_directory.file_name().unwrap())
                    .join(shard.file_name().unwrap())
            } else {
                displaced.join(shard.file_name().unwrap())
            };
            let before = fs::read(&displaced_shard).unwrap();
            assert_eq!(
                writer.append(EventFact::AppLaunch, at(1), at(1)),
                AppendOutcome::Dropped(DropReason::UnsafeCustody)
            );
            assert_eq!(fs::read(displaced_shard).unwrap(), before);
            assert_eq!(fs::read_dir(replaced).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_32_concurrent_first_writers_share_private_directories() {
        let root = TestRoot::new("concurrent-directory-create");
        let store_path = root.store();
        let root_start = Arc::new(Barrier::new(2));
        let [first_root, second_root] = std::thread::scope(|scope| {
            let first_path = store_path.clone();
            let second_path = store_path.clone();
            let first_start = root_start.clone();
            let second_start = root_start.clone();
            let first = scope.spawn(move || {
                open_or_create_store_nofollow_with_missing_hook(&first_path, || {
                    first_start.wait();
                })
            });
            let second = scope.spawn(move || {
                open_or_create_store_nofollow_with_missing_hook(&second_path, || {
                    second_start.wait();
                })
            });
            [
                first.join().unwrap().unwrap(),
                second.join().unwrap().unwrap(),
            ]
        });
        assert_eq!(
            FileIdentity::from_metadata(&first_root.metadata().unwrap()),
            FileIdentity::from_metadata(&second_root.metadata().unwrap())
        );

        let observed_day = day(at(0));
        let slot_name =
            slot_directory_name(retention_slot_index(observed_day, TEST_RETENTION_DAYS));
        let slot_start = Arc::new(Barrier::new(2));
        let [first_slot, second_slot] = std::thread::scope(|scope| {
            let first_parent_path = store_path.clone();
            let second_parent_path = store_path.clone();
            let first_name = slot_name.clone();
            let second_name = slot_name.clone();
            let first_start = slot_start.clone();
            let second_start = slot_start.clone();
            let first = scope.spawn(move || {
                open_private_directory_at_with_missing_hook(
                    &first_root,
                    &first_parent_path,
                    std::ffi::OsStr::new(&first_name),
                    true,
                    || {
                        first_start.wait();
                    },
                )
            });
            let second = scope.spawn(move || {
                open_private_directory_at_with_missing_hook(
                    &second_root,
                    &second_parent_path,
                    std::ffi::OsStr::new(&second_name),
                    true,
                    || {
                        second_start.wait();
                    },
                )
            });
            [
                first.join().unwrap().unwrap().unwrap(),
                second.join().unwrap().unwrap().unwrap(),
            ]
        });
        assert_eq!(first_slot.identity, second_slot.identity);

        let day_name = day_directory_name(observed_day);
        let day_start = Arc::new(Barrier::new(2));
        let [first_day, second_day] = std::thread::scope(|scope| {
            let first_name = day_name.clone();
            let second_name = day_name.clone();
            let first_start = day_start.clone();
            let second_start = day_start.clone();
            let first = scope.spawn(move || {
                open_private_directory_at_with_missing_hook(
                    &first_slot.file,
                    &first_slot.path,
                    std::ffi::OsStr::new(&first_name),
                    true,
                    || {
                        first_start.wait();
                    },
                )
            });
            let second = scope.spawn(move || {
                open_private_directory_at_with_missing_hook(
                    &second_slot.file,
                    &second_slot.path,
                    std::ffi::OsStr::new(&second_name),
                    true,
                    || {
                        second_start.wait();
                    },
                )
            });
            [
                first.join().unwrap().unwrap().unwrap(),
                second.join().unwrap().unwrap().unwrap(),
            ]
        });
        assert_eq!(first_day.identity, second_day.identity);

        let config = recorder_config(&root);
        let start = Arc::new(Barrier::new(2));
        let outcomes = std::thread::scope(|scope| {
            let first_config = config.clone();
            let second_config = config.clone();
            let first_start = start.clone();
            let second_start = start.clone();
            let first = scope.spawn(move || {
                first_start.wait();
                let mut writer = TestRecorder::open(first_config, launch_identity()).unwrap();
                writer.append(EventFact::AppLaunch, at(0), at(0))
            });
            let second = scope.spawn(move || {
                second_start.wait();
                let mut writer =
                    TestRecorder::open(second_config, other_launch_identity()).unwrap();
                writer.append(EventFact::AppLaunch, at(0), at(0))
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(outcomes, [AppendOutcome::Appended, AppendOutcome::Appended]);
        assert_eq!(native_shards(&root).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_33_all_slots_receive_exact_rotating_quota() {
        let root = TestRoot::new("retention-all-slot-quota");
        let observed = at(80 * 86_400);
        let today = day(observed);
        assert_eq!(today, NaiveDate::from_ymd_opt(2026, 10, 22).unwrap());
        let today_actual: [usize; TEST_RETENTION_DAYS] =
            std::array::from_fn(|slot| retention_scan_quota(today, TEST_RETENTION_DAYS, 64, slot));
        assert_eq!(today_actual.iter().sum::<usize>(), 64);
        assert_eq!(today_actual.iter().filter(|quota| **quota == 5).count(), 8);
        assert_eq!(today_actual.iter().filter(|quota| **quota == 4).count(), 6);
        let tomorrow = today.succ_opt().unwrap();
        assert_eq!(tomorrow, NaiveDate::from_ymd_opt(2026, 10, 23).unwrap());
        let tomorrow_actual: [usize; TEST_RETENTION_DAYS] = std::array::from_fn(|slot| {
            retention_scan_quota(tomorrow, TEST_RETENTION_DAYS, 64, slot)
        });
        assert_ne!(today_actual, tomorrow_actual);
        assert_eq!(
            today_actual[retention_slot_index(today, TEST_RETENTION_DAYS)],
            5
        );
        assert_eq!(
            tomorrow_actual[retention_slot_index(today, TEST_RETENTION_DAYS)],
            4
        );

        let mut retained_paths: [Option<PathBuf>; TEST_RETENTION_DAYS] =
            std::array::from_fn(|_| None);
        let mut expired_paths: [Vec<PathBuf>; TEST_RETENTION_DAYS] =
            std::array::from_fn(|_| Vec::new());
        for slot_index in 0..TEST_RETENTION_DAYS {
            let retained_day = (0..TEST_RETENTION_DAYS)
                .map(|offset| today.checked_sub_days(Days::new(offset as u64)).unwrap())
                .find(|candidate| {
                    retention_slot_index(*candidate, TEST_RETENTION_DAYS) == slot_index
                })
                .unwrap();
            let retained =
                nested_shard_path(&root, retained_day, &format!("{:032x}", 1_000 + slot_index));
            write_private(&retained, b"retained\n");
            retained_paths[slot_index] = Some(retained);

            let expired_day = (TEST_RETENTION_DAYS..TEST_RETENTION_DAYS * 2)
                .map(|offset| today.checked_sub_days(Days::new(offset as u64)).unwrap())
                .find(|candidate| {
                    retention_slot_index(*candidate, TEST_RETENTION_DAYS) == slot_index
                })
                .unwrap();
            for leaf_index in 0..6 {
                let expired = nested_shard_path(
                    &root,
                    expired_day,
                    &format!("{:032x}", 2_000 + slot_index * 16 + leaf_index),
                );
                write_private(&expired, b"expired\n");
                expired_paths[slot_index].push(expired);
            }
        }

        let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        let report = writer.run_retention_for_test(observed);
        assert_eq!(report.scanned, 64);
        assert_eq!(report.slot_scanned.as_slice(), today_actual.as_slice());
        assert!(
            retained_paths
                .iter()
                .all(|path| path.as_ref().unwrap().exists())
        );
        for paths in expired_paths {
            assert!(
                paths.iter().filter(|path| path.exists()).count() < paths.len(),
                "every clean saturated slot must make progress"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_33a_test_report_covers_every_valid_retention_slot() {
        let root = TestRoot::new("retention-dynamic-report");
        let retention = RetentionPolicy {
            retained_days: 20,
            scan_cap: 84,
        };
        let config = RecorderConfig::for_test(
            root.store(),
            source_identity(),
            false,
            8 * 1024 * 1024,
            retention,
        )
        .unwrap();
        let mut writer = TestRecorder::open(config, launch_identity()).unwrap();

        let report = writer.run_retention_for_test(at(80 * 86_400));

        assert_eq!(report.slot_scanned.len(), retention.slot_count().unwrap());
        assert_eq!(report.slot_scanned.iter().sum::<usize>(), report.scanned);
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_34_encountered_blocker_prevents_all_slot_deletion() {
        let root = TestRoot::new("retention-whole-slot-blocker");
        let observed = at(80 * 86_400);
        let today = day(observed);
        let safe_day = today.checked_sub_days(Days::new(28)).unwrap();
        let blocker_day = today.checked_sub_days(Days::new(14)).unwrap();
        assert_eq!(
            retention_slot_index(safe_day, TEST_RETENTION_DAYS),
            retention_slot_index(blocker_day, TEST_RETENTION_DAYS)
        );
        let safe = nested_shard_path(&root, safe_day, WRITER_ID);
        write_private(&safe, b"safe-but-blocked\n");
        let blocker_day_directory = nested_shard_path(&root, blocker_day, OTHER_WRITER_ID)
            .parent()
            .unwrap()
            .to_path_buf();
        let blocker = blocker_day_directory.join("malformed-entry");
        write_private(&blocker, b"blocker\n");

        let mut blocked = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        let blocked_report = blocked.run_retention_for_test(observed);
        assert_eq!(blocked_report.removed, 0);
        assert!(blocked_report.revoked >= 1);
        assert!(safe.exists());
        assert!(blocker.exists());

        fs::remove_file(blocker).unwrap();
        let mut resumed = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
        assert!(resumed.run_retention_for_test(observed).removed >= 1);
        assert!(!safe.exists());
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_35_unsafe_expired_leaves_survive_while_clean_slots_progress() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        for case in [
            "wrong-day",
            "malformed",
            "symlink",
            "hardlink",
            "wrong-mode",
            "wrong-type",
        ] {
            let root = TestRoot::new(&format!("retention-unsafe-leaf-{case}"));
            let observed = at(80 * 86_400);
            let today = day(observed);
            let expired_day = today.checked_sub_days(Days::new(15)).unwrap();
            let valid = nested_shard_path(&root, expired_day, WRITER_ID);
            let unsafe_path = match case {
                "wrong-day" => valid
                    .parent()
                    .unwrap()
                    .join(owned_shard_name(expired_day.succ_opt().unwrap(), WRITER_ID)),
                "malformed" => valid.parent().unwrap().join("malformed-entry"),
                _ => valid,
            };
            match case {
                "symlink" => {
                    let target = root.path.join("symlink-target");
                    write_private(&target, b"target\n");
                    symlink(target, &unsafe_path).unwrap();
                }
                "hardlink" => {
                    write_private(&unsafe_path, b"hardlinked\n");
                    fs::hard_link(&unsafe_path, root.path.join("hardlink-peer")).unwrap();
                }
                "wrong-mode" => {
                    write_private(&unsafe_path, b"public\n");
                    fs::set_permissions(&unsafe_path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                "wrong-type" => create_private_directory(&unsafe_path),
                _ => write_private(&unsafe_path, b"unsafe-name\n"),
            }

            let clean_day = today.checked_sub_days(Days::new(16)).unwrap();
            let clean = nested_shard_path(&root, clean_day, OTHER_WRITER_ID);
            write_private(&clean, b"clean\n");
            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            let report = writer.run_retention_for_test(observed);
            assert!(fs::symlink_metadata(&unsafe_path).is_ok(), "{case}");
            assert!(!clean.exists(), "{case}");
            assert!(report.revoked >= 1, "{case}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_36_source_slot_or_day_replacement_revokes_retention_custody() {
        for level in ["source", "slot", "day"] {
            let root = TestRoot::new(&format!("retention-{level}-replacement"));
            let observed = at(80 * 86_400);
            let expired_day = day(observed).checked_sub_days(Days::new(15)).unwrap();
            let candidate = nested_shard_path(&root, expired_day, WRITER_ID);
            write_private(&candidate, b"expired\n");
            let day_directory = candidate.parent().unwrap().to_path_buf();
            let slot = day_directory.parent().unwrap().to_path_buf();
            let source = root.store();
            let replaced = match level {
                "source" => &source,
                "slot" => &slot,
                "day" => &day_directory,
                _ => unreachable!(),
            };
            let displaced = replaced.with_file_name(format!(
                "{}-displaced",
                replaced.file_name().unwrap().to_string_lossy()
            ));

            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            writer.inject_retention_directory_replacement_race(replaced.to_path_buf());
            let report = writer.run_retention_for_test(observed);
            let displaced_candidate = match level {
                "source" => displaced
                    .join(slot.file_name().unwrap())
                    .join(day_directory.file_name().unwrap())
                    .join(candidate.file_name().unwrap()),
                "slot" => displaced
                    .join(day_directory.file_name().unwrap())
                    .join(candidate.file_name().unwrap()),
                "day" => displaced.join(candidate.file_name().unwrap()),
                _ => unreachable!(),
            };
            assert_eq!(report.removed, 0);
            assert!(report.revoked >= 1);
            assert_eq!(fs::read(displaced_candidate).unwrap(), b"expired\n");
            assert_eq!(fs::read_dir(replaced).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_37_source_slot_and_day_custody_fail_closed() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        for level in ["source", "slot", "day"] {
            for defect in ["symlink", "mode", "type"] {
                let root = TestRoot::new(&format!("retention-{level}-{defect}"));
                let observed = at(80 * 86_400);
                let today = day(observed);
                let expired_day = today.checked_sub_days(Days::new(15)).unwrap();
                let slot = root.store().join(slot_directory_name(retention_slot_index(
                    expired_day,
                    TEST_RETENTION_DAYS,
                )));
                let target = match level {
                    "source" => root.store(),
                    "slot" => {
                        create_private_directory(&root.store());
                        slot.clone()
                    }
                    "day" => {
                        create_private_directory(&root.store());
                        create_private_directory(&slot);
                        slot.join(day_directory_name(expired_day))
                    }
                    _ => unreachable!(),
                };
                match defect {
                    "symlink" => {
                        let victim = root.path.join(format!("{level}-victim"));
                        create_private_directory(&victim);
                        write_private(&victim.join("sentinel"), b"sentinel\n");
                        symlink(victim, &target).unwrap();
                    }
                    "mode" => {
                        create_private_directory(&target);
                        write_private(&target.join("sentinel"), b"sentinel\n");
                        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
                    }
                    "type" => write_private(&target, b"not-a-directory\n"),
                    _ => unreachable!(),
                }

                let clean = if level == "source" {
                    None
                } else {
                    let clean_day = today.checked_sub_days(Days::new(16)).unwrap();
                    let path = nested_shard_path(&root, clean_day, OTHER_WRITER_ID);
                    write_private(&path, b"clean\n");
                    Some(path)
                };
                let mut writer =
                    TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
                let report = writer.run_retention_for_test(observed);
                assert!(fs::symlink_metadata(&target).is_ok(), "{level}/{defect}");
                if let Some(clean) = clean {
                    assert!(!clean.exists(), "{level}/{defect}");
                    assert!(report.revoked >= 1, "{level}/{defect}");
                } else {
                    assert_eq!(report, Default::default(), "{level}/{defect}");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1_contract_39_empty_day_replacement_revokes_rmdir_custody() {
        for level in ["source", "slot", "day"] {
            let root = TestRoot::new(&format!("retention-empty-day-{level}-replacement"));
            let observed = at(80 * 86_400);
            let expired_day = day(observed).checked_sub_days(Days::new(15)).unwrap();
            let leaf = nested_shard_path(&root, expired_day, WRITER_ID);
            write_private(&leaf, b"removed-before-crash\n");
            fs::remove_file(&leaf).unwrap();
            let day_directory = leaf.parent().unwrap().to_path_buf();
            let slot = day_directory.parent().unwrap().to_path_buf();
            let source = root.store();
            let replaced = match level {
                "source" => &source,
                "slot" => &slot,
                "day" => &day_directory,
                _ => unreachable!(),
            };
            let displaced = replaced.with_file_name(format!(
                "{}-displaced",
                replaced.file_name().unwrap().to_string_lossy()
            ));

            let mut writer = TestRecorder::open(recorder_config(&root), launch_identity()).unwrap();
            writer.inject_retention_directory_replacement_race(replaced.to_path_buf());
            let report = writer.run_retention_for_test(observed);
            let displaced_day = match level {
                "source" => displaced
                    .join(slot.file_name().unwrap())
                    .join(day_directory.file_name().unwrap()),
                "slot" => displaced.join(day_directory.file_name().unwrap()),
                "day" => displaced.clone(),
                _ => unreachable!(),
            };
            assert_eq!(report.removed, 0, "{level}");
            assert!(report.revoked >= 1, "{level}");
            assert!(replaced.exists(), "{level}");
            assert!(displaced_day.exists(), "{level}");
            assert_eq!(fs::read_dir(replaced).unwrap().count(), 0, "{level}");
            assert_eq!(fs::read_dir(displaced_day).unwrap().count(), 0, "{level}");
        }
    }
}
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver as SyncReceiver, SyncSender, TrySendError},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
#[cfg(unix)]
use std::ptr::NonNull;

use chrono::{DateTime, Datelike as _, Days, NaiveDate, Utc};
use gpui::{App, AppContext as _};
use parking_lot::Mutex;
use release_channel::ReleaseChannel;
use serde_json::{Map, Value, json};
use uuid::Uuid;

const SERVICE_NAME: &str = "zed-10x";
const COHORT: &str = "zed10x";
const SOURCE_KIND: &str = "in_process";
const PROBE_NAME: &str = "gpui_main_queue";
const LIVENESS_INTERVAL: Duration = Duration::from_secs(1);
const LIVENESS_THRESHOLD: Duration = Duration::from_secs(5);
const WRITER_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(100);
const MIN_RETENTION_SLOT_QUOTA: usize = 4;
const MAX_RETENTION_SLOT_COUNT: usize = 99;
const RETENTION_FILE_QUARANTINE_PREFIX: &str = ".retention-delete-";
const RETENTION_DIRECTORY_QUARANTINE_PREFIX: &str = ".retention-directory-delete-";

pub(super) const UAT_FOREGROUND_HANG_DURATION: Duration =
    LIVENESS_THRESHOLD.saturating_add(LIVENESS_INTERVAL.saturating_mul(3));

#[derive(Clone, Debug)]
struct SourceIdentity {
    lane: String,
    service_version: String,
    build_version: String,
    commit_sha: Option<String>,
}

impl SourceIdentity {
    fn from_app(cx: &App) -> Option<Self> {
        if ReleaseChannel::global(cx) != ReleaseChannel::Dev {
            return None;
        }
        Self::from_build()
    }

    fn from_build() -> Option<Self> {
        let service_version = env!("CARGO_PKG_VERSION").to_owned();
        let build_version = option_env!("ZED_BUILD_ID").unwrap_or("local").to_owned();
        let commit_sha = option_env!("ZED_COMMIT_SHA").map(str::to_owned);
        if !valid_identity_text(&service_version)
            || !valid_identity_text(&build_version)
            || commit_sha
                .as_deref()
                .is_some_and(|sha| !valid_lower_hex(sha, 40))
        {
            return None;
        }
        Some(Self {
            lane: "local".to_owned(),
            service_version,
            build_version,
            commit_sha,
        })
    }

    #[cfg(test)]
    fn for_test(
        lane: &str,
        service_version: &str,
        build_version: &str,
        commit_sha: &str,
    ) -> Result<Self, &'static str> {
        if lane != "local"
            || !valid_identity_text(service_version)
            || !valid_identity_text(build_version)
            || !valid_lower_hex(commit_sha, 40)
        {
            return Err("invalid source identity");
        }
        Ok(Self {
            lane: lane.to_owned(),
            service_version: service_version.to_owned(),
            build_version: build_version.to_owned(),
            commit_sha: Some(commit_sha.to_owned()),
        })
    }
}

fn valid_identity_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

fn valid_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value.bytes().any(|byte| byte != b'0')
}

#[derive(Clone, Debug)]
struct LaunchIdentity {
    trace_id: String,
    span_id: String,
    writer_id: String,
}

impl LaunchIdentity {
    fn fresh() -> Result<Self, &'static str> {
        let trace_id = Uuid::new_v4().simple().to_string();
        let span_source = Uuid::new_v4().simple().to_string();
        let writer_id = Uuid::new_v4().simple().to_string();
        Self::from_parts_for_test(&trace_id, &span_source[..16], &writer_id)
    }

    fn from_parts_for_test(
        trace_id: &str,
        span_id: &str,
        writer_id: &str,
    ) -> Result<Self, &'static str> {
        if !valid_lower_hex(trace_id, 32)
            || !valid_lower_hex(span_id, 16)
            || !valid_lower_hex(writer_id, 32)
        {
            return Err("invalid launch identity");
        }
        Ok(Self {
            trace_id: trace_id.to_owned(),
            span_id: span_id.to_owned(),
            writer_id: writer_id.to_owned(),
        })
    }

    #[cfg(test)]
    fn trace_id(&self) -> &str {
        &self.trace_id
    }

    fn writer_id(&self) -> &str {
        &self.writer_id
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventName {
    AppLaunch,
    AppProcessExit,
    LivenessReady,
    Hang,
    HangRecovered,
}

#[derive(Clone, Debug)]
enum EventFact {
    AppLaunch,
    AppProcessExit,
    LivenessReady {
        baseline_latency: Duration,
        threshold: Duration,
    },
    Hang {
        duration: Duration,
        threshold: Duration,
        missed_intervals: u64,
    },
    HangRecovered {
        duration: Duration,
        threshold: Duration,
        missed_intervals: u64,
    },
}

impl EventFact {
    #[cfg(test)]
    fn name(&self) -> EventName {
        match self {
            Self::AppLaunch => EventName::AppLaunch,
            Self::AppProcessExit => EventName::AppProcessExit,
            Self::LivenessReady { .. } => EventName::LivenessReady,
            Self::Hang { .. } => EventName::Hang,
            Self::HangRecovered { .. } => EventName::HangRecovered,
        }
    }

    fn body(&self) -> &'static str {
        match self {
            Self::AppLaunch => "app.launch",
            Self::AppProcessExit => "app.process_exit",
            Self::LivenessReady { .. } => "app.liveness.ready",
            Self::Hang { .. } => "app.hang",
            Self::HangRecovered { .. } => "app.hang.recovered",
        }
    }

    fn severity(&self) -> &'static str {
        match self {
            Self::Hang { .. } => "WARN",
            _ => "INFO",
        }
    }
}

fn unix_nanos(time: SystemTime) -> Result<String, &'static str> {
    Ok(time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "time precedes unix epoch")?
        .as_nanos()
        .to_string())
}

fn encode_event_line(
    source: &SourceIdentity,
    launch: &LaunchIdentity,
    event_sequence: u64,
    fact: &EventFact,
    event_time: SystemTime,
    observed_time: SystemTime,
) -> Result<Vec<u8>, &'static str> {
    if event_sequence == 0 {
        return Err("event sequence must be positive");
    }
    let mut attributes = Map::new();
    attributes.insert("event.sequence".into(), json!(event_sequence));
    attributes.insert("event.schema_version".into(), json!(1));
    attributes.insert("service.name".into(), json!(SERVICE_NAME));
    attributes.insert("service.version".into(), json!(source.service_version));
    if let Some(commit_sha) = &source.commit_sha {
        attributes.insert("vcs.ref.head.revision".into(), json!(commit_sha));
    }
    attributes.insert("zed.build_version".into(), json!(source.build_version));
    attributes.insert("zed.cohort".into(), json!(COHORT));
    attributes.insert("zed.lane".into(), json!(source.lane));
    attributes.insert("zed.source".into(), json!(SOURCE_KIND));
    attributes.insert("zed.writer_id".into(), json!(launch.writer_id));

    match fact {
        EventFact::AppLaunch => {
            attributes.insert("lifecycle.state".into(), json!("started"));
        }
        EventFact::AppProcessExit => {
            attributes.insert("lifecycle.state".into(), json!("clean_exit"));
        }
        EventFact::LivenessReady {
            baseline_latency,
            threshold,
        } => {
            attributes.insert(
                "liveness.baseline_latency_ms".into(),
                json!(duration_millis(*baseline_latency)?),
            );
            attributes.insert("liveness.probe".into(), json!(PROBE_NAME));
            attributes.insert(
                "liveness.threshold_ms".into(),
                json!(positive_duration_millis(*threshold)?),
            );
        }
        EventFact::Hang {
            duration,
            threshold,
            missed_intervals,
        } => {
            attributes.insert(
                "duration.ms".into(),
                json!(positive_duration_millis(*duration)?),
            );
            attributes.insert("failure.class".into(), json!("main_thread_unresponsive"));
            attributes.insert("liveness.missed_intervals".into(), json!(missed_intervals));
            attributes.insert("liveness.probe".into(), json!(PROBE_NAME));
            attributes.insert(
                "liveness.threshold_ms".into(),
                json!(positive_duration_millis(*threshold)?),
            );
        }
        EventFact::HangRecovered {
            duration,
            threshold,
            missed_intervals,
        } => {
            attributes.insert(
                "duration.ms".into(),
                json!(positive_duration_millis(*duration)?),
            );
            attributes.insert("liveness.missed_intervals".into(), json!(missed_intervals));
            attributes.insert("liveness.probe".into(), json!(PROBE_NAME));
            attributes.insert(
                "liveness.threshold_ms".into(),
                json!(positive_duration_millis(*threshold)?),
            );
        }
    }

    let envelope = json!({
        "time_unix_nano": unix_nanos(event_time)?,
        "observed_time_unix_nano": unix_nanos(observed_time)?,
        "severity_text": fact.severity(),
        "body": fact.body(),
        "trace_id": launch.trace_id,
        "span_id": launch.span_id,
        "attributes": attributes,
    });
    let mut bytes = serde_json::to_vec(&envelope).map_err(|_| "event serialization failed")?;
    bytes.push(b'\n');
    validate_event_line(&bytes)?;
    Ok(bytes)
}

fn duration_millis(duration: Duration) -> Result<u64, &'static str> {
    u64::try_from(duration.as_millis()).map_err(|_| "duration exceeds envelope range")
}

fn positive_duration_millis(duration: Duration) -> Result<u64, &'static str> {
    let millis = duration_millis(duration)?;
    if millis == 0 {
        Err("duration must be positive")
    } else {
        Ok(millis)
    }
}

fn validate_event_line(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.last() != Some(&b'\n') || bytes[..bytes.len().saturating_sub(1)].contains(&b'\n') {
        return Err("event must be exactly one newline-terminated JSON object");
    }
    let value: Value = serde_json::from_slice(&bytes[..bytes.len() - 1])
        .map_err(|_| "event is not strict JSON")?;
    let object = value.as_object().ok_or("event must be an object")?;
    require_exact_keys(
        object.keys().map(String::as_str),
        &[
            "attributes",
            "body",
            "observed_time_unix_nano",
            "severity_text",
            "span_id",
            "time_unix_nano",
            "trace_id",
        ],
    )?;

    let body = object
        .get("body")
        .and_then(Value::as_str)
        .ok_or("invalid body")?;
    let severity = object
        .get("severity_text")
        .and_then(Value::as_str)
        .ok_or("invalid severity")?;
    let trace_id = object
        .get("trace_id")
        .and_then(Value::as_str)
        .ok_or("invalid trace id")?;
    let span_id = object
        .get("span_id")
        .and_then(Value::as_str)
        .ok_or("invalid span id")?;
    if !valid_lower_hex(trace_id, 32) || !valid_lower_hex(span_id, 16) {
        return Err("invalid trace context");
    }
    for key in ["time_unix_nano", "observed_time_unix_nano"] {
        let text = object
            .get(key)
            .and_then(Value::as_str)
            .ok_or("invalid timestamp")?;
        text.parse::<u128>().map_err(|_| "invalid timestamp")?;
    }

    let attributes = object
        .get("attributes")
        .and_then(Value::as_object)
        .ok_or("invalid attributes")?;
    let mut common = vec![
        "event.sequence",
        "event.schema_version",
        "service.name",
        "service.version",
        "zed.build_version",
        "zed.cohort",
        "zed.lane",
        "zed.source",
        "zed.writer_id",
    ];
    if attributes.contains_key("vcs.ref.head.revision") {
        common.push("vcs.ref.head.revision");
    }
    let event_keys: &[&str] = match body {
        "app.launch" | "app.process_exit" => &["lifecycle.state"],
        "app.liveness.ready" => &[
            "liveness.baseline_latency_ms",
            "liveness.probe",
            "liveness.threshold_ms",
        ],
        "app.hang" => &[
            "duration.ms",
            "failure.class",
            "liveness.missed_intervals",
            "liveness.probe",
            "liveness.threshold_ms",
        ],
        "app.hang.recovered" => &[
            "duration.ms",
            "liveness.missed_intervals",
            "liveness.probe",
            "liveness.threshold_ms",
        ],
        _ => return Err("unknown event"),
    };
    common.extend_from_slice(event_keys);
    require_exact_keys(attributes.keys().map(String::as_str), &common)?;
    if attributes.get("event.schema_version") != Some(&json!(1))
        || attributes.get("service.name") != Some(&json!(SERVICE_NAME))
        || attributes.get("zed.cohort") != Some(&json!(COHORT))
        || attributes.get("zed.lane") != Some(&json!("local"))
        || attributes.get("zed.source") != Some(&json!(SOURCE_KIND))
    {
        return Err("invalid common attributes");
    }
    for key in ["service.version", "zed.build_version"] {
        let text = attributes
            .get(key)
            .and_then(Value::as_str)
            .ok_or("invalid identity")?;
        if !valid_identity_text(text) {
            return Err("invalid identity");
        }
    }
    if !valid_lower_hex(
        attributes
            .get("zed.writer_id")
            .and_then(Value::as_str)
            .ok_or("invalid writer id")?,
        32,
    ) {
        return Err("invalid writer id");
    }
    if let Some(commit) = attributes.get("vcs.ref.head.revision") {
        if !valid_lower_hex(commit.as_str().ok_or("invalid commit")?, 40) {
            return Err("invalid commit");
        }
    }

    let positive_u64 = |key: &str| {
        attributes
            .get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .ok_or("invalid positive integer")
    };
    positive_u64("event.sequence")?;
    match body {
        "app.launch" => {
            if severity != "INFO" || attributes.get("lifecycle.state") != Some(&json!("started")) {
                return Err("invalid launch event");
            }
        }
        "app.process_exit" => {
            if severity != "INFO" || attributes.get("lifecycle.state") != Some(&json!("clean_exit"))
            {
                return Err("invalid exit event");
            }
        }
        "app.liveness.ready" => {
            if severity != "INFO"
                || attributes
                    .get("liveness.baseline_latency_ms")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err("invalid ready event");
            }
            positive_u64("liveness.threshold_ms")?;
        }
        "app.hang" => {
            if severity != "WARN"
                || attributes.get("failure.class") != Some(&json!("main_thread_unresponsive"))
            {
                return Err("invalid hang event");
            }
            positive_u64("duration.ms")?;
            positive_u64("liveness.threshold_ms")?;
            positive_u64("liveness.missed_intervals")?;
        }
        "app.hang.recovered" => {
            if severity != "INFO" {
                return Err("invalid recovery event");
            }
            positive_u64("duration.ms")?;
            positive_u64("liveness.threshold_ms")?;
            positive_u64("liveness.missed_intervals")?;
        }
        _ => unreachable!(),
    }
    if event_keys.contains(&"liveness.probe")
        && attributes.get("liveness.probe") != Some(&json!(PROBE_NAME))
    {
        return Err("invalid liveness probe");
    }
    Ok(())
}

fn require_exact_keys<'a>(
    actual: impl Iterator<Item = &'a str>,
    expected: &[&str],
) -> Result<(), &'static str> {
    let actual = actual.collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err("field set is not closed")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DropReason {
    Disabled,
    QueueFull,
    StorageUnavailable,
    SerializationRejected,
    SequenceExhausted,
    ShortWrite,
    PoisonedShard,
    UnsafeCustody,
    ObservationTimeRegression,
    ShardFull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SinkOffer {
    Accepted,
    Dropped(DropReason),
}

trait TryEventSink {
    fn try_offer(&mut self, event: PendingEvent) -> SinkOffer;
}

#[derive(Clone, Debug)]
struct PendingEvent {
    sequence: u64,
    fact: EventFact,
    event_time: SystemTime,
}

impl PendingEvent {
    fn new(sequence: u64, fact: EventFact, event_time: SystemTime) -> Self {
        Self {
            sequence,
            fact,
            event_time,
        }
    }

    #[cfg(test)]
    fn fact(&self) -> &EventFact {
        &self.fact
    }

    #[cfg(test)]
    fn sequence(&self) -> u64 {
        self.sequence
    }
}

#[derive(Clone)]
struct RecorderIngress {
    sender: SyncSender<PendingEvent>,
}

impl RecorderIngress {
    fn bounded(capacity: usize) -> (Self, SyncReceiver<PendingEvent>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (Self { sender }, receiver)
    }
}

impl TryEventSink for RecorderIngress {
    fn try_offer(&mut self, event: PendingEvent) -> SinkOffer {
        match self.sender.try_send(event) {
            Ok(()) => SinkOffer::Accepted,
            Err(TrySendError::Full(_)) => SinkOffer::Dropped(DropReason::QueueFull),
            Err(TrySendError::Disconnected(_)) => {
                SinkOffer::Dropped(DropReason::StorageUnavailable)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleCoverage {
    Complete,
    Partial,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleOffer {
    Accepted,
    Dropped(DropReason),
    RejectedTransition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleState {
    BeforeLaunch,
    Active,
    Terminal,
}

struct LifecycleProducer {
    state: LifecycleState,
    coverage: LifecycleCoverage,
    dropped_offers: u64,
    terminal_offers: u64,
    next_sequence: Option<u64>,
}

impl LifecycleProducer {
    fn new() -> Self {
        Self {
            state: LifecycleState::BeforeLaunch,
            coverage: LifecycleCoverage::Complete,
            dropped_offers: 0,
            terminal_offers: 0,
            next_sequence: Some(1),
        }
    }

    fn offer_launch(&mut self, sink: &mut impl TryEventSink, at: SystemTime) -> LifecycleOffer {
        if self.state != LifecycleState::BeforeLaunch {
            return LifecycleOffer::RejectedTransition;
        }
        self.state = LifecycleState::Active;
        self.offer(sink, EventFact::AppLaunch, at)
    }

    fn offer_liveness(
        &mut self,
        sink: &mut impl TryEventSink,
        transition: LivenessTransition,
        at: SystemTime,
    ) -> LifecycleOffer {
        if self.state != LifecycleState::Active {
            return LifecycleOffer::RejectedTransition;
        }
        let fact = match transition {
            LivenessTransition::Ready {
                baseline_latency,
                threshold,
                ..
            } => EventFact::LivenessReady {
                baseline_latency,
                threshold,
            },
            LivenessTransition::Hung {
                duration,
                threshold,
                missed_intervals,
                ..
            } => EventFact::Hang {
                duration,
                threshold,
                missed_intervals,
            },
            LivenessTransition::Recovered {
                duration,
                threshold,
                missed_intervals,
                ..
            } => EventFact::HangRecovered {
                duration,
                threshold,
                missed_intervals,
            },
        };
        self.offer(sink, fact, at)
    }

    fn offer_liveness_batch(
        &mut self,
        sink: &mut impl TryEventSink,
        transitions: LivenessTransitions,
    ) -> [Option<LifecycleOffer>; 2] {
        let mut outcomes = [None, None];
        for (slot, transition) in transitions.into_slots().into_iter().enumerate() {
            if let Some(transition) = transition {
                let at = transition.event_time();
                outcomes[slot] = Some(self.offer_liveness(sink, transition, at));
            }
        }
        outcomes
    }

    fn offer_clean_exit(&mut self, sink: &mut impl TryEventSink, at: SystemTime) -> LifecycleOffer {
        if self.state != LifecycleState::Active {
            return LifecycleOffer::RejectedTransition;
        }
        self.state = LifecycleState::Terminal;
        self.terminal_offers += 1;
        self.offer(sink, EventFact::AppProcessExit, at)
    }

    fn offer(
        &mut self,
        sink: &mut impl TryEventSink,
        fact: EventFact,
        at: SystemTime,
    ) -> LifecycleOffer {
        let Some(sequence) = self.next_sequence else {
            self.coverage = LifecycleCoverage::Partial;
            self.dropped_offers += 1;
            return LifecycleOffer::Dropped(DropReason::SequenceExhausted);
        };
        self.next_sequence = sequence.checked_add(1);
        match sink.try_offer(PendingEvent::new(sequence, fact, at)) {
            SinkOffer::Accepted => LifecycleOffer::Accepted,
            SinkOffer::Dropped(reason) => {
                self.coverage = LifecycleCoverage::Partial;
                self.dropped_offers += 1;
                LifecycleOffer::Dropped(reason)
            }
        }
    }

    #[cfg(test)]
    fn coverage(&self) -> LifecycleCoverage {
        self.coverage
    }

    #[cfg(test)]
    fn dropped_offer_count(&self) -> u64 {
        self.dropped_offers
    }

    #[cfg(test)]
    fn terminal_offer_count(&self) -> u64 {
        self.terminal_offers
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClockSample {
    monotonic: Duration,
    suspend_aware: SystemTime,
}

impl ClockSample {
    #[cfg(test)]
    fn for_test(monotonic: Duration, suspend_aware: SystemTime) -> Self {
        Self {
            monotonic,
            suspend_aware,
        }
    }
}

trait ClockSource: Send + Sync + 'static {
    fn sample(&self) -> ClockSample;
}

struct SystemClock {
    started: Instant,
}

impl SystemClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl ClockSource for SystemClock {
    fn sample(&self) -> ClockSample {
        ClockSample {
            monotonic: self.started.elapsed(),
            suspend_aware: SystemTime::now(),
        }
    }
}

#[cfg(test)]
struct ManualClock {
    sample: Mutex<ClockSample>,
}

#[cfg(test)]
impl ManualClock {
    fn new(sample: ClockSample) -> Self {
        Self {
            sample: Mutex::new(sample),
        }
    }

    fn set(&self, sample: ClockSample) {
        *self.sample.lock() = sample;
    }
}

#[cfg(test)]
impl ClockSource for ManualClock {
    fn sample(&self) -> ClockSample {
        *self.sample.lock()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AcknowledgementSample {
    sequence: u64,
    acknowledged_at: Option<ClockSample>,
}

impl AcknowledgementSample {
    fn initial() -> Self {
        Self {
            sequence: 0,
            acknowledged_at: None,
        }
    }

    #[cfg(test)]
    fn for_test(sequence: u64, acknowledged_at: ClockSample) -> Self {
        Self {
            sequence,
            acknowledged_at: (sequence != 0).then_some(acknowledged_at),
        }
    }
}

struct AtomicAcknowledgement {
    version: AtomicU64,
    sequence: AtomicU64,
    monotonic_nanos: AtomicU64,
    unix_nanos: AtomicU64,
}

impl AtomicAcknowledgement {
    fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            monotonic_nanos: AtomicU64::new(0),
            unix_nanos: AtomicU64::new(0),
        }
    }

    fn record(&self, sequence: u64, sample: ClockSample) -> bool {
        self.record_with_in_progress_hook(sequence, sample, || {})
    }

    fn record_with_in_progress_hook(
        &self,
        sequence: u64,
        sample: ClockSample,
        in_progress: impl FnOnce(),
    ) -> bool {
        let Ok(monotonic_nanos) = u64::try_from(sample.monotonic.as_nanos()) else {
            return false;
        };
        let Ok(unix_nanos) = sample
            .suspend_aware
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .ok_or(())
        else {
            return false;
        };
        if sequence == 0 {
            return false;
        }

        self.version.fetch_add(1, Ordering::AcqRel);
        in_progress();
        self.sequence.store(sequence, Ordering::Relaxed);
        self.monotonic_nanos
            .store(monotonic_nanos, Ordering::Relaxed);
        self.unix_nanos.store(unix_nanos, Ordering::Relaxed);
        self.version.fetch_add(1, Ordering::Release);
        true
    }

    fn try_load(&self) -> Option<AcknowledgementSample> {
        let before = self.version.load(Ordering::Acquire);
        if !before.is_multiple_of(2) {
            return None;
        }
        let sequence = self.sequence.load(Ordering::Relaxed);
        let monotonic_nanos = self.monotonic_nanos.load(Ordering::Relaxed);
        let unix_nanos = self.unix_nanos.load(Ordering::Relaxed);
        // The first acquire keeps payload reads after the admitted even version.
        // This read barrier keeps those payload reads before the retry/version
        // read, so a stable version proves all three fields are one publication.
        std::sync::atomic::fence(Ordering::Acquire);
        let after = self.version.load(Ordering::Acquire);
        if before != after || !after.is_multiple_of(2) {
            return None;
        }
        Some(if sequence == 0 {
            AcknowledgementSample::initial()
        } else {
            AcknowledgementSample {
                sequence,
                acknowledged_at: Some(ClockSample {
                    monotonic: Duration::from_nanos(monotonic_nanos),
                    suspend_aware: UNIX_EPOCH + Duration::from_nanos(unix_nanos),
                }),
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LivenessTransition {
    Ready {
        baseline_latency: Duration,
        threshold: Duration,
        event_time: SystemTime,
    },
    Hung {
        duration: Duration,
        threshold: Duration,
        missed_intervals: u64,
        event_time: SystemTime,
    },
    Recovered {
        duration: Duration,
        threshold: Duration,
        missed_intervals: u64,
        event_time: SystemTime,
    },
}

impl LivenessTransition {
    fn event_time(&self) -> SystemTime {
        match self {
            Self::Ready { event_time, .. }
            | Self::Hung { event_time, .. }
            | Self::Recovered { event_time, .. } => *event_time,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LivenessTransitions {
    None,
    One(LivenessTransition),
    Pair(LivenessTransition, LivenessTransition),
}

impl LivenessTransitions {
    fn into_slots(self) -> [Option<LivenessTransition>; 2] {
        match self {
            Self::None => [None, None],
            Self::One(transition) => [Some(transition), None],
            Self::Pair(first, second) => [Some(first), Some(second)],
        }
    }

    fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    #[cfg(test)]
    fn into_single(self) -> Option<LivenessTransition> {
        match self {
            Self::None => None,
            Self::One(transition) => Some(transition),
            Self::Pair(_, _) => panic!("expected at most one liveness transition"),
        }
    }
}

#[derive(Clone, Copy)]
struct PendingProbe {
    sequence: u64,
    sent: ClockSample,
}

struct LivenessMonitor {
    interval: Duration,
    threshold: Duration,
    last_tick: Option<ClockSample>,
    last_acknowledgement: u64,
    pending: Option<PendingProbe>,
    ready: bool,
    hung: bool,
}

impl LivenessMonitor {
    fn new(interval: Duration, threshold: Duration) -> Self {
        assert!(!interval.is_zero());
        assert!(threshold >= interval);
        Self {
            interval,
            threshold,
            last_tick: None,
            last_acknowledgement: 0,
            pending: None,
            ready: false,
            hung: false,
        }
    }

    fn tick(
        &mut self,
        now: ClockSample,
        acknowledgement: AcknowledgementSample,
        offer_probe: impl FnOnce(u64) -> bool,
    ) -> LivenessTransitions {
        if self.must_reset(now, acknowledgement) {
            self.reset(now, acknowledgement, offer_probe);
            return LivenessTransitions::None;
        }

        if self.last_tick.is_none() {
            self.reset(now, acknowledgement, offer_probe);
            return LivenessTransitions::None;
        }
        self.last_tick = Some(now);

        let Some(pending) = self.pending else {
            self.install_probe(now, acknowledgement.sequence.checked_add(1), offer_probe);
            return LivenessTransitions::None;
        };

        if acknowledgement.sequence == pending.sequence {
            let Some(acknowledged_at) = acknowledgement.acknowledged_at else {
                self.reset(now, acknowledgement, offer_probe);
                return LivenessTransitions::None;
            };
            let Some(elapsed) = monotonic_elapsed(pending.sent, acknowledged_at) else {
                self.reset(now, acknowledgement, offer_probe);
                return LivenessTransitions::None;
            };
            let hang_boundary = pending
                .sent
                .suspend_aware
                .checked_add(self.threshold)
                .unwrap_or(acknowledged_at.suspend_aware);
            self.last_acknowledgement = acknowledgement.sequence;
            self.install_probe(now, acknowledgement.sequence.checked_add(1), offer_probe);
            if !self.ready {
                self.ready = true;
                self.hung = false;
                return LivenessTransitions::One(LivenessTransition::Ready {
                    baseline_latency: elapsed,
                    threshold: self.threshold,
                    event_time: acknowledged_at.suspend_aware,
                });
            }
            if self.hung {
                self.hung = false;
                let recovery_duration = elapsed.max(self.threshold);
                return LivenessTransitions::One(LivenessTransition::Recovered {
                    duration: recovery_duration,
                    threshold: self.threshold,
                    missed_intervals: missed_intervals(recovery_duration, self.interval),
                    event_time: acknowledged_at.suspend_aware.max(hang_boundary),
                });
            }
            if elapsed >= self.threshold {
                return LivenessTransitions::Pair(
                    LivenessTransition::Hung {
                        duration: self.threshold,
                        threshold: self.threshold,
                        missed_intervals: missed_intervals(self.threshold, self.interval),
                        event_time: hang_boundary,
                    },
                    LivenessTransition::Recovered {
                        duration: elapsed,
                        threshold: self.threshold,
                        missed_intervals: missed_intervals(elapsed, self.interval),
                        event_time: acknowledged_at.suspend_aware.max(hang_boundary),
                    },
                );
            }
            return LivenessTransitions::None;
        }

        let elapsed = monotonic_elapsed(pending.sent, now).unwrap_or_default();
        if self.ready && !self.hung && elapsed >= self.threshold {
            self.hung = true;
            let hang_boundary = pending
                .sent
                .suspend_aware
                .checked_add(self.threshold)
                .unwrap_or(now.suspend_aware);
            return LivenessTransitions::One(LivenessTransition::Hung {
                duration: self.threshold,
                threshold: self.threshold,
                missed_intervals: missed_intervals(self.threshold, self.interval),
                event_time: hang_boundary,
            });
        }
        LivenessTransitions::None
    }

    fn must_reset(&self, now: ClockSample, acknowledgement: AcknowledgementSample) -> bool {
        let Some(last_tick) = self.last_tick else {
            return false;
        };
        let Some(monotonic_gap) = now.monotonic.checked_sub(last_tick.monotonic) else {
            return true;
        };
        let Ok(suspend_gap) = now.suspend_aware.duration_since(last_tick.suspend_aware) else {
            return true;
        };
        if monotonic_gap > self.interval.saturating_mul(2)
            || duration_difference(monotonic_gap, suspend_gap) > self.interval
        {
            return true;
        }
        acknowledgement.sequence != self.last_acknowledgement
            && self.pending.map(|probe| probe.sequence) != Some(acknowledgement.sequence)
    }

    fn reset(
        &mut self,
        now: ClockSample,
        acknowledgement: AcknowledgementSample,
        offer_probe: impl FnOnce(u64) -> bool,
    ) {
        self.last_tick = Some(now);
        self.last_acknowledgement = acknowledgement.sequence;
        self.pending = None;
        self.ready = false;
        self.hung = false;
        self.install_probe(now, acknowledgement.sequence.checked_add(1), offer_probe);
    }

    fn install_probe(
        &mut self,
        now: ClockSample,
        sequence: Option<u64>,
        offer_probe: impl FnOnce(u64) -> bool,
    ) {
        if let Some(sequence) = sequence.filter(|sequence| *sequence != 0)
            && offer_probe(sequence)
        {
            self.pending = Some(PendingProbe {
                sequence,
                sent: now,
            });
        } else {
            self.pending = None;
        }
    }
}

fn monotonic_elapsed(from: ClockSample, to: ClockSample) -> Option<Duration> {
    to.monotonic.checked_sub(from.monotonic)
}

fn duration_difference(left: Duration, right: Duration) -> Duration {
    left.checked_sub(right)
        .or_else(|| right.checked_sub(left))
        .unwrap_or_default()
}

fn missed_intervals(duration: Duration, interval: Duration) -> u64 {
    let value = duration.as_nanos() / interval.as_nanos();
    u64::try_from(value).unwrap_or(u64::MAX).max(1)
}

fn try_send_probe(sender: &async_channel::Sender<u64>, sequence: u64) -> bool {
    sender.try_send(sequence).is_ok()
}

fn start_foreground_acknowledger(
    receiver: async_channel::Receiver<u64>,
    acknowledgement: Arc<AtomicAcknowledgement>,
    clock: Arc<dyn ClockSource>,
    cx: &mut App,
) {
    cx.spawn(async move |_| {
        while let Ok(sequence) = receiver.recv().await {
            let _ = acknowledgement.record(sequence, clock.sample());
        }
    })
    .detach();
}

fn restart_telemetry_disabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value.eq_ignore_ascii_case("1")
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("on")
    })
}

#[derive(Clone, Copy, Debug)]
struct RetentionPolicy {
    retained_days: u64,
    scan_cap: usize,
}

impl RetentionPolicy {
    fn slot_count(self) -> Option<usize> {
        usize::try_from(self.retained_days).ok()
    }

    fn is_valid(self) -> bool {
        self.retained_days > 0
            && self.slot_count().is_some_and(|slot_count| {
                slot_count <= MAX_RETENTION_SLOT_COUNT
                    && self.scan_cap >= slot_count.saturating_mul(MIN_RETENTION_SLOT_QUOTA)
            })
    }
}

#[derive(Clone, Debug)]
struct RecorderConfig {
    store_path: PathBuf,
    disabled_sentinel_path: PathBuf,
    source: SourceIdentity,
    disabled_at_restart: bool,
    max_shard_bytes: u64,
    retention: RetentionPolicy,
}

impl RecorderConfig {
    fn for_app(source: SourceIdentity, disabled_at_restart: bool) -> Self {
        let retention = RetentionPolicy {
            retained_days: 14,
            scan_cap: 64,
        };
        debug_assert!(retention.is_valid());
        let canary_store = paths::data_dir().join("dogfood-canary");
        Self {
            store_path: canary_store.join("zed10x-in-process"),
            disabled_sentinel_path: canary_store.join("DISABLED"),
            source,
            disabled_at_restart,
            max_shard_bytes: 8 * 1024 * 1024,
            retention,
        }
    }

    #[cfg(test)]
    fn for_test(
        store_path: PathBuf,
        source: SourceIdentity,
        disabled_at_restart: bool,
        max_shard_bytes: u64,
        retention: RetentionPolicy,
    ) -> Result<Self, &'static str> {
        if max_shard_bytes == 0 || !retention.is_valid() || source.lane != "local" {
            return Err("invalid recorder configuration");
        }
        Ok(Self {
            disabled_sentinel_path: store_path
                .parent()
                .unwrap_or(store_path.as_path())
                .join("DISABLED"),
            store_path,
            source,
            disabled_at_restart,
            max_shard_bytes,
            retention,
        })
    }

    #[cfg(test)]
    fn set_disabled_at_restart_for_test(&mut self, disabled: bool) {
        self.disabled_at_restart = disabled;
    }

    fn disabled_by_sentinel(&self) -> bool {
        match fs::symlink_metadata(&self.disabled_sentinel_path) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreOpenError {
    DisabledAtRestart,
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    UnsupportedPlatform,
    WriterThreadUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppendOutcome {
    Appended,
    Dropped(DropReason),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RetentionReport {
    scanned: usize,
    removed: usize,
    revoked: usize,
    #[cfg(test)]
    slot_scanned: Vec<usize>,
}

#[derive(Default)]
struct RetentionState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreEntryKind {
    Directory,
    RegularFile,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct CustodyFacts {
    kind: StoreEntryKind,
    owner_matches: bool,
    mode: u32,
    link_count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreRole {
    Root,
    PrivateDirectory,
    Shard,
}

fn admit_store_custody(role: StoreRole, facts: CustodyFacts) -> bool {
    let (kind, mode) = match role {
        StoreRole::Root | StoreRole::PrivateDirectory => (StoreEntryKind::Directory, 0o700),
        StoreRole::Shard => (StoreEntryKind::RegularFile, 0o600),
    };
    facts.kind == kind && facts.owner_matches && facts.mode == mode && facts.link_count == 1
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

struct OpenDirectory {
    file: File,
    path: PathBuf,
    name: OsString,
    identity: FileIdentity,
}

struct OpenShard {
    slot: OpenDirectory,
    day_directory: OpenDirectory,
    file: File,
    #[cfg(test)]
    path: PathBuf,
    name: OsString,
    identity: FileIdentity,
    day: NaiveDate,
    bytes: u64,
}

struct Recorder {
    config: RecorderConfig,
    launch: LaunchIdentity,
    root: Option<File>,
    root_identity: Option<FileIdentity>,
    shard: Option<OpenShard>,
    poisoned: bool,
    last_observation_day: Option<NaiveDate>,
    open_generation: u64,
    closed_shards: u64,
    retention_runs: u64,
    #[cfg(test)]
    short_write_once: Option<usize>,
    #[cfg(test)]
    retention_failure_once: bool,
    #[cfg(test)]
    unlink_replacement_race: Option<(PathBuf, Vec<u8>)>,
    #[cfg(test)]
    final_unlink_replacement_race: Option<(PathBuf, Vec<u8>)>,
    #[cfg(test)]
    retention_directory_replacement_race: Option<PathBuf>,
    #[cfg(test)]
    final_directory_removal_replacement_race: Option<PathBuf>,
}

#[cfg(test)]
struct TestRecorder {
    recorder: Recorder,
    retention: RetentionState,
}

#[cfg(test)]
impl TestRecorder {
    fn open(config: RecorderConfig, launch: LaunchIdentity) -> Result<Self, StoreOpenError> {
        Ok(Self {
            recorder: Recorder::open(config, launch)?,
            retention: RetentionState,
        })
    }

    fn append(
        &mut self,
        fact: EventFact,
        event_time: SystemTime,
        observed_time: SystemTime,
    ) -> AppendOutcome {
        self.recorder
            .append_event(&mut self.retention, 1, fact, event_time, observed_time)
    }

    fn run_retention_for_test(&mut self, observed_time: SystemTime) -> RetentionReport {
        self.recorder
            .run_retention(&mut self.retention, observed_time)
            .unwrap_or_default()
    }
}

#[cfg(test)]
impl std::ops::Deref for TestRecorder {
    type Target = Recorder;

    fn deref(&self) -> &Self::Target {
        &self.recorder
    }
}

#[cfg(test)]
impl std::ops::DerefMut for TestRecorder {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.recorder
    }
}

impl Recorder {
    fn open(config: RecorderConfig, launch: LaunchIdentity) -> Result<Self, StoreOpenError> {
        if config.disabled_at_restart {
            return Err(StoreOpenError::DisabledAtRestart);
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios"
        )))]
        return Err(StoreOpenError::UnsupportedPlatform);
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios"
        ))]
        Ok(Self {
            config,
            launch,
            root: None,
            root_identity: None,
            shard: None,
            poisoned: false,
            last_observation_day: None,
            open_generation: 0,
            closed_shards: 0,
            retention_runs: 0,
            #[cfg(test)]
            short_write_once: None,
            #[cfg(test)]
            retention_failure_once: false,
            #[cfg(test)]
            unlink_replacement_race: None,
            #[cfg(test)]
            final_unlink_replacement_race: None,
            #[cfg(test)]
            retention_directory_replacement_race: None,
            #[cfg(test)]
            final_directory_removal_replacement_race: None,
        })
    }

    fn append_event(
        &mut self,
        retention: &mut RetentionState,
        sequence: u64,
        fact: EventFact,
        event_time: SystemTime,
        observed_time: SystemTime,
    ) -> AppendOutcome {
        if self.config.disabled_by_sentinel() {
            return AppendOutcome::Dropped(DropReason::Disabled);
        }
        // Wall time can move backwards between occurrence and persistence. The event contract
        // still requires observation to be no earlier than the fact it records.
        let observed_time = observed_time.max(event_time);
        let observed_day = DateTime::<Utc>::from(observed_time).date_naive();
        if self
            .last_observation_day
            .is_some_and(|last_day| observed_day < last_day)
        {
            return AppendOutcome::Dropped(DropReason::ObservationTimeRegression);
        }
        if self.ensure_root().is_err() {
            return AppendOutcome::Dropped(DropReason::UnsafeCustody);
        }

        let rollover = self
            .shard
            .as_ref()
            .is_none_or(|shard| shard.day != observed_day);
        if self.poisoned && !rollover {
            return AppendOutcome::Dropped(DropReason::PoisonedShard);
        }
        if rollover {
            if self.shard.take().is_some() {
                self.closed_shards += 1;
            }
            self.poisoned = false;
            self.retention_runs += 1;
            let _ = self.run_retention(retention, observed_time);
            match self.open_shard(observed_day) {
                Ok(shard) => {
                    self.open_generation += 1;
                    self.shard = Some(shard);
                }
                Err(_) => {
                    return AppendOutcome::Dropped(DropReason::UnsafeCustody);
                }
            }
        }
        self.last_observation_day = Some(observed_day);

        if !self.current_custody_is_valid() {
            return AppendOutcome::Dropped(DropReason::UnsafeCustody);
        }
        let line = match encode_event_line(
            &self.config.source,
            &self.launch,
            sequence,
            &fact,
            event_time,
            observed_time,
        ) {
            Ok(line) => line,
            Err(_) => return AppendOutcome::Dropped(DropReason::SerializationRejected),
        };
        let Some(shard) = self.shard.as_mut() else {
            return AppendOutcome::Dropped(DropReason::StorageUnavailable);
        };
        if shard.bytes.saturating_add(line.len() as u64) > self.config.max_shard_bytes {
            return AppendOutcome::Dropped(DropReason::ShardFull);
        }

        #[cfg(test)]
        if let Some(limit) = self.short_write_once.take() {
            let requested = limit.min(line.len());
            let written = shard.file.write(&line[..requested]).unwrap_or(0);
            shard.bytes = shard.bytes.saturating_add(written as u64);
            self.poisoned = true;
            return AppendOutcome::Dropped(DropReason::ShortWrite);
        }

        match shard.file.write(&line) {
            Ok(written) if written == line.len() => {
                shard.bytes = shard.bytes.saturating_add(written as u64);
                AppendOutcome::Appended
            }
            Ok(written) => {
                shard.bytes = shard.bytes.saturating_add(written as u64);
                self.poisoned = true;
                AppendOutcome::Dropped(DropReason::ShortWrite)
            }
            Err(_) => AppendOutcome::Dropped(DropReason::StorageUnavailable),
        }
    }
}

impl Recorder {
    fn ensure_root(&mut self) -> io::Result<()> {
        if let (Some(root), Some(identity)) = (&self.root, self.root_identity) {
            if !root_binding_is_valid(root, &self.config.store_path, identity) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "store custody changed",
                ));
            }
            return Ok(());
        }

        let parent = self.config.store_path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "store must have a parent")
        })?;
        let _parent = open_or_create_store_nofollow(parent)?;
        let root = open_or_create_store_nofollow(&self.config.store_path)?;
        let descriptor_metadata = root.metadata()?;
        let path_metadata = fs::symlink_metadata(&self.config.store_path)?;
        let identity = FileIdentity::from_metadata(&descriptor_metadata);
        if FileIdentity::from_metadata(&path_metadata) != identity
            || !admit_store_custody(StoreRole::Root, root_custody_facts(&path_metadata))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe store custody",
            ));
        }
        self.root = Some(root);
        self.root_identity = Some(identity);
        Ok(())
    }

    fn open_shard(&self, day: NaiveDate) -> io::Result<OpenShard> {
        let name = owned_shard_name(day, self.launch.writer_id());
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "store is not open"))?;
        let slot_count = self
            .config
            .retention
            .slot_count()
            .ok_or_else(|| io::Error::other("invalid retention slot count"))?;
        let slot_name = slot_directory_name(retention_slot_index(day, slot_count));
        let slot = open_private_directory_at(
            root,
            &self.config.store_path,
            std::ffi::OsStr::new(&slot_name),
            true,
        )?
        .ok_or_else(|| io::Error::other("slot directory was not created"))?;
        let day_name = day_directory_name(day);
        let day_directory = open_private_directory_at(
            &slot.file,
            &slot.path,
            std::ffi::OsStr::new(&day_name),
            true,
        )?
        .ok_or_else(|| io::Error::other("day directory was not created"))?;
        lock_directory(&day_directory.file, false)?;
        let file = create_shard_at(&day_directory.file, &name, &day_directory.path)?;
        let metadata = file.metadata()?;
        if !admit_store_custody(StoreRole::Shard, custody_facts(&metadata)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe shard custody",
            ));
        }
        Ok(OpenShard {
            slot,
            day_directory,
            file,
            #[cfg(test)]
            path: self
                .config
                .store_path
                .join(&slot_name)
                .join(&day_name)
                .join(&name),
            name: OsString::from(name),
            identity: FileIdentity::from_metadata(&metadata),
            day,
            bytes: metadata.len(),
        })
    }

    fn current_custody_is_valid(&self) -> bool {
        let Some(root) = &self.root else {
            return false;
        };
        let Some(root_identity) = self.root_identity else {
            return false;
        };
        if !root_binding_is_valid(root, &self.config.store_path, root_identity) {
            return false;
        }
        let Some(shard) = &self.shard else {
            return false;
        };
        if !private_directory_binding_is_valid(root, &shard.slot)
            || !private_directory_binding_is_valid(&shard.slot.file, &shard.day_directory)
        {
            return false;
        }
        let Ok(descriptor_metadata) = shard.file.metadata() else {
            return false;
        };
        let Ok(Some(observed)) =
            open_retention_candidate(&shard.day_directory.file, &shard.name, false)
        else {
            return false;
        };
        FileIdentity::from_metadata(&descriptor_metadata) == shard.identity
            && observed.identity == shard.identity
            && admit_store_custody(StoreRole::Shard, custody_facts(&descriptor_metadata))
    }

    fn run_retention(
        &mut self,
        _retention: &mut RetentionState,
        observed_time: SystemTime,
    ) -> io::Result<RetentionReport> {
        #[cfg(test)]
        if std::mem::take(&mut self.retention_failure_once) {
            return Err(io::Error::other("injected retention failure"));
        }
        self.ensure_root()?;
        let today = DateTime::<Utc>::from(observed_time).date_naive();
        let Some(cutoff) = today.checked_sub_days(Days::new(
            self.config.retention.retained_days.saturating_sub(1),
        )) else {
            return Ok(RetentionReport::default());
        };
        let slot_count = self
            .config
            .retention
            .slot_count()
            .ok_or_else(|| io::Error::other("invalid retention slot count"))?;
        let root = self.root.as_ref().expect("root was admitted");
        let root_identity = self.root_identity.expect("root identity was admitted");
        let mut report = RetentionReport::default();
        #[cfg(test)]
        report.slot_scanned.resize(slot_count, 0);

        for slot_index in 0..slot_count {
            let mut quota = retention_scan_quota(
                today,
                slot_count,
                self.config.retention.scan_cap,
                slot_index,
            );
            let slot_name = slot_directory_name(slot_index);
            let slot = match open_private_directory_at(
                root,
                &self.config.store_path,
                std::ffi::OsStr::new(&slot_name),
                false,
            ) {
                Ok(Some(slot)) => slot,
                Ok(None) => continue,
                Err(_) => {
                    report.revoked += 1;
                    continue;
                }
            };
            let mut slot_blocked = false;
            let mut planned_days = Vec::new();
            if parse_slot_directory_name(&slot.name, slot_count) != Some(slot_index) {
                report.revoked += 1;
                continue;
            }
            let mut slot_cursor = match DirectoryCursor::open(&slot.file) {
                Ok(cursor) => cursor,
                Err(_) => {
                    report.revoked += 1;
                    continue;
                }
            };

            // The retention horizon and slot count are identical, so a slot can contain at most
            // one retained day. Every other valid day is expired, and each clean expired entry
            // scanned is deleted or moved closer to deletion. Because every slot receives a quota
            // of at least four, restarting the descriptor cursor still makes monotonic progress;
            // the retained day cannot consume a whole run. Unsafe entries deliberately block that
            // slot rather than being skipped as if trusted.

            while quota > 0 {
                let day_name = match slot_cursor.next_name() {
                    Ok(Some(name)) => name,
                    Ok(None) => break,
                    Err(_) => {
                        report.revoked += 1;
                        slot_blocked = true;
                        break;
                    }
                };
                quota -= 1;
                report.scanned += 1;
                #[cfg(test)]
                {
                    report.slot_scanned[slot_index] += 1;
                }
                if is_retention_directory_quarantine_name(&day_name) {
                    let cleanup =
                        match open_private_directory_at(&slot.file, &slot.path, &day_name, false) {
                            Ok(Some(directory))
                                if lock_directory(&directory.file, true).is_ok()
                                    && root_binding_is_valid(
                                        root,
                                        &self.config.store_path,
                                        root_identity,
                                    )
                                    && private_directory_binding_is_valid(root, &slot)
                                    && private_directory_binding_is_valid(
                                        &slot.file, &directory,
                                    ) =>
                            {
                                remove_verified_retention_directory(
                                    &slot.file,
                                    &day_name,
                                    directory.identity,
                                )
                            }
                            _ => Err(io::Error::other(
                                "retention directory quarantine custody was not valid",
                            )),
                        };
                    if cleanup.is_ok() {
                        report.removed += 1;
                    } else {
                        report.revoked += 1;
                        slot_blocked = true;
                    }
                    continue;
                }
                let Some(day) = parse_day_directory_name(&day_name) else {
                    report.revoked += 1;
                    slot_blocked = true;
                    continue;
                };
                if day > today || retention_slot_index(day, slot_count) != slot_index {
                    report.revoked += 1;
                    slot_blocked = true;
                    continue;
                }
                let day_directory =
                    match open_private_directory_at(&slot.file, &slot.path, &day_name, false) {
                        Ok(Some(directory)) => directory,
                        _ => {
                            report.revoked += 1;
                            slot_blocked = true;
                            continue;
                        }
                    };
                if day >= cutoff {
                    continue;
                }
                if lock_directory(&day_directory.file, true).is_err() {
                    report.revoked += 1;
                    slot_blocked = true;
                    continue;
                }

                let mut complete = false;
                let mut planned_candidates = Vec::new();
                {
                    let mut day_cursor = match DirectoryCursor::open(&day_directory.file) {
                        Ok(cursor) => cursor,
                        Err(_) => {
                            report.revoked += 1;
                            slot_blocked = true;
                            continue;
                        }
                    };
                    while quota > 0 {
                        let shard_name = match day_cursor.next_name() {
                            Ok(Some(name)) => name,
                            Ok(None) => {
                                complete = true;
                                break;
                            }
                            Err(_) => {
                                report.revoked += 1;
                                slot_blocked = true;
                                break;
                            }
                        };
                        quota -= 1;
                        report.scanned += 1;
                        #[cfg(test)]
                        {
                            report.slot_scanned[slot_index] += 1;
                        }
                        if is_retention_file_quarantine_name(&shard_name) {
                            let cleanup = match open_retention_candidate(
                                &day_directory.file,
                                &shard_name,
                                true,
                            ) {
                                Ok(Some(candidate))
                                    if root_binding_is_valid(
                                        root,
                                        &self.config.store_path,
                                        root_identity,
                                    ) && private_directory_binding_is_valid(root, &slot)
                                        && private_directory_binding_is_valid(
                                            &slot.file,
                                            &day_directory,
                                        ) =>
                                {
                                    remove_verified_retention_candidate(
                                        &day_directory.file,
                                        &shard_name,
                                        candidate.identity,
                                    )
                                }
                                _ => Err(io::Error::other(
                                    "retention file quarantine custody was not valid",
                                )),
                            };
                            if cleanup.is_ok() {
                                report.removed += 1;
                            } else {
                                report.revoked += 1;
                                slot_blocked = true;
                            }
                            continue;
                        }
                        if parse_owned_shard_name(&shard_name)
                            .is_none_or(|(shard_day, _)| shard_day != day)
                        {
                            report.revoked += 1;
                            slot_blocked = true;
                            continue;
                        }
                        let candidate = match open_retention_candidate(
                            &day_directory.file,
                            &shard_name,
                            true,
                        ) {
                            Ok(Some(candidate)) => candidate,
                            _ => {
                                report.revoked += 1;
                                slot_blocked = true;
                                continue;
                            }
                        };
                        #[cfg(test)]
                        if self
                            .retention_directory_replacement_race
                            .as_ref()
                            .is_some_and(|path| {
                                path == &self.config.store_path
                                    || path == &slot.path
                                    || path == &day_directory.path
                            })
                        {
                            let path = self.retention_directory_replacement_race.take().unwrap();
                            replace_private_test_directory(&path)?;
                        }
                        #[cfg(test)]
                        let path = day_directory.path.join(&shard_name);
                        #[cfg(test)]
                        if self
                            .unlink_replacement_race
                            .as_ref()
                            .is_some_and(|(candidate, _)| candidate == &path)
                        {
                            let (_, replacement) = self.unlink_replacement_race.take().unwrap();
                            let displaced = path.with_extension("jsonl.displaced");
                            fs::rename(&path, &displaced)?;
                            write_private_test_replacement(&path, &replacement)?;
                        }
                        let observed =
                            match open_retention_candidate(&day_directory.file, &shard_name, false)
                            {
                                Ok(Some(observed)) => observed,
                                _ => {
                                    report.revoked += 1;
                                    slot_blocked = true;
                                    continue;
                                }
                            };
                        if candidate.identity != observed.identity
                            || !root_binding_is_valid(root, &self.config.store_path, root_identity)
                            || !private_directory_binding_is_valid(root, &slot)
                            || !private_directory_binding_is_valid(&slot.file, &day_directory)
                        {
                            report.revoked += 1;
                            slot_blocked = true;
                            continue;
                        }
                        planned_candidates.push(PlannedRetentionCandidate {
                            name: shard_name,
                            candidate,
                        });
                    }
                }
                planned_days.push(PlannedRetentionDay {
                    name: day_name,
                    directory: day_directory,
                    candidates: planned_candidates,
                    complete,
                });
            }

            if slot_blocked
                || !root_binding_is_valid(root, &self.config.store_path, root_identity)
                || !private_directory_binding_is_valid(root, &slot)
                || planned_days.iter().any(|day| {
                    !private_directory_binding_is_valid(&slot.file, &day.directory)
                        || day.candidates.iter().any(|planned| {
                            open_retention_candidate(&day.directory.file, &planned.name, false)
                                .ok()
                                .flatten()
                                .is_none_or(|observed| {
                                    observed.identity != planned.candidate.identity
                                })
                        })
                })
            {
                report.revoked += usize::from(!slot_blocked);
                continue;
            }

            let mut mutation_failed = false;
            for day in planned_days {
                for candidate in day.candidates {
                    #[cfg(test)]
                    {
                        let path = day.directory.path.join(&candidate.name);
                        if self
                            .final_unlink_replacement_race
                            .as_ref()
                            .is_some_and(|(candidate, _)| candidate == &path)
                        {
                            let (_, replacement) =
                                self.final_unlink_replacement_race.take().unwrap();
                            let displaced = path.with_extension("jsonl.final-displaced");
                            fs::rename(&path, &displaced)?;
                            write_private_test_replacement(&path, &replacement)?;
                        }
                    }
                    if remove_verified_retention_candidate(
                        &day.directory.file,
                        &candidate.name,
                        candidate.candidate.identity,
                    )
                    .is_ok()
                    {
                        report.removed += 1;
                    } else {
                        report.revoked += 1;
                        mutation_failed = true;
                        break;
                    }
                }
                if mutation_failed {
                    break;
                }
                if day.complete {
                    #[cfg(test)]
                    if self
                        .retention_directory_replacement_race
                        .as_ref()
                        .is_some_and(|path| {
                            path == &self.config.store_path
                                || path == &slot.path
                                || path == &day.directory.path
                        })
                    {
                        let path = self.retention_directory_replacement_race.take().unwrap();
                        replace_private_test_directory(&path)?;
                    }
                    if root_binding_is_valid(root, &self.config.store_path, root_identity)
                        && private_directory_binding_is_valid(root, &slot)
                        && private_directory_binding_is_valid(&slot.file, &day.directory)
                    {
                        #[cfg(test)]
                        if self
                            .final_directory_removal_replacement_race
                            .as_ref()
                            .is_some_and(|path| path == &day.directory.path)
                        {
                            let path = self
                                .final_directory_removal_replacement_race
                                .take()
                                .unwrap();
                            replace_private_test_directory(&path)?;
                        }
                        if remove_verified_retention_directory(
                            &slot.file,
                            &day.name,
                            day.directory.identity,
                        )
                        .is_ok()
                        {
                            report.removed += 1;
                        } else {
                            report.revoked += 1;
                            mutation_failed = true;
                        }
                    } else {
                        report.revoked += 1;
                        mutation_failed = true;
                    }
                }
                if mutation_failed {
                    break;
                }
            }
        }
        Ok(report)
    }

    #[cfg(test)]
    fn current_shard_path(&self) -> Option<&Path> {
        self.shard.as_ref().map(|shard| shard.path.as_path())
    }

    #[cfg(test)]
    fn current_shard_day(&self) -> Option<NaiveDate> {
        self.shard.as_ref().map(|shard| shard.day)
    }

    #[cfg(test)]
    fn writer_id(&self) -> &str {
        self.launch.writer_id()
    }

    #[cfg(test)]
    fn open_shard_generation(&self) -> u64 {
        self.open_generation
    }

    #[cfg(test)]
    fn closed_shard_count(&self) -> u64 {
        self.closed_shards
    }

    #[cfg(test)]
    fn retention_run_count(&self) -> u64 {
        self.retention_runs
    }

    #[cfg(test)]
    fn inject_short_write_once(&mut self, bytes: usize) {
        self.short_write_once = Some(bytes);
    }

    #[cfg(test)]
    fn inject_retention_failure_once(&mut self) {
        self.retention_failure_once = true;
    }

    #[cfg(test)]
    fn inject_unlink_replacement_race(&mut self, path: PathBuf, replacement: Vec<u8>) {
        self.unlink_replacement_race = Some((path, replacement));
    }

    #[cfg(test)]
    fn inject_final_unlink_replacement_race(&mut self, path: PathBuf, replacement: Vec<u8>) {
        self.final_unlink_replacement_race = Some((path, replacement));
    }

    #[cfg(test)]
    fn inject_retention_directory_replacement_race(&mut self, path: PathBuf) {
        self.retention_directory_replacement_race = Some(path);
    }

    #[cfg(test)]
    fn inject_final_directory_removal_replacement_race(&mut self, path: PathBuf) {
        self.final_directory_removal_replacement_race = Some(path);
    }
}

fn owned_shard_name(day: NaiveDate, writer_id: &str) -> String {
    format!("events-{}-{writer_id}.jsonl", day.format("%Y%m%d"))
}

fn retention_slot_index(day: NaiveDate, slot_count: usize) -> usize {
    day.num_days_from_ce().rem_euclid(slot_count as i32) as usize
}

fn retention_scan_quota(
    today: NaiveDate,
    slot_count: usize,
    scan_cap: usize,
    slot_index: usize,
) -> usize {
    debug_assert!(slot_count > 0);
    debug_assert!(slot_index < slot_count);
    let base_quota = scan_cap / slot_count;
    let bonus_slots = scan_cap % slot_count;
    let bonus_origin = retention_slot_index(today, slot_count);
    let bonus_distance = (slot_index + slot_count - bonus_origin) % slot_count;
    base_quota + usize::from(bonus_distance < bonus_slots)
}

fn slot_directory_name(index: usize) -> String {
    format!("slot-{index:02}")
}

fn parse_slot_directory_name(name: &std::ffi::OsStr, slot_count: usize) -> Option<usize> {
    let name = name.to_str()?;
    if name.len() != 7 {
        return None;
    }
    let index = name.strip_prefix("slot-")?.parse::<usize>().ok()?;
    (index < slot_count && slot_directory_name(index) == name).then_some(index)
}

fn day_directory_name(day: NaiveDate) -> String {
    format!("day-{}", day.format("%Y%m%d"))
}

fn parse_day_directory_name(name: &std::ffi::OsStr) -> Option<NaiveDate> {
    let name = name.to_str()?;
    if name.len() != 12 {
        return None;
    }
    let day = NaiveDate::parse_from_str(name.strip_prefix("day-")?, "%Y%m%d").ok()?;
    (day_directory_name(day) == name).then_some(day)
}

fn is_retention_quarantine_name(name: &std::ffi::OsStr, prefix: &str) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix(prefix))
        .is_some_and(|suffix| valid_lower_hex(suffix, 32))
}

fn is_retention_file_quarantine_name(name: &std::ffi::OsStr) -> bool {
    is_retention_quarantine_name(name, RETENTION_FILE_QUARANTINE_PREFIX)
}

fn is_retention_directory_quarantine_name(name: &std::ffi::OsStr) -> bool {
    is_retention_quarantine_name(name, RETENTION_DIRECTORY_QUARANTINE_PREFIX)
}

fn parse_owned_shard_name(name: &std::ffi::OsStr) -> Option<(NaiveDate, &str)> {
    let name = name.to_str()?;
    let middle = name.strip_prefix("events-")?.strip_suffix(".jsonl")?;
    if middle.len() != 8 + 1 + 32 || middle.as_bytes().get(8) != Some(&b'-') {
        return None;
    }
    let day = NaiveDate::parse_from_str(&middle[..8], "%Y%m%d").ok()?;
    let writer_id = &middle[9..];
    valid_lower_hex(writer_id, 32).then_some((day, writer_id))
}

fn custody_facts(metadata: &fs::Metadata) -> CustodyFacts {
    let kind = if metadata.file_type().is_symlink() {
        StoreEntryKind::Symlink
    } else if metadata.is_dir() {
        StoreEntryKind::Directory
    } else if metadata.is_file() {
        StoreEntryKind::RegularFile
    } else {
        StoreEntryKind::Other
    };
    #[cfg(unix)]
    {
        CustodyFacts {
            kind,
            owner_matches: metadata.uid() == unsafe { libc::geteuid() },
            mode: metadata.mode() & 0o777,
            link_count: metadata.nlink(),
        }
    }
    #[cfg(not(unix))]
    {
        CustodyFacts {
            kind,
            owner_matches: true,
            mode: if metadata.is_dir() { 0o700 } else { 0o600 },
            link_count: 1,
        }
    }
}

fn root_custody_facts(metadata: &fs::Metadata) -> CustodyFacts {
    let mut facts = custody_facts(metadata);
    // Directory link counts are filesystem-specific and can change as ordinary
    // children are added (APFS does this for files as well as subdirectories).
    // Directory hard links are prohibited by the supported operating systems;
    // root replacement is instead guarded by descriptor/path device+inode parity.
    if facts.kind == StoreEntryKind::Directory {
        facts.link_count = 1;
    }
    facts
}

fn root_binding_is_valid(root: &File, path: &Path, identity: FileIdentity) -> bool {
    let Ok(descriptor_metadata) = root.metadata() else {
        return false;
    };
    let Ok(path_metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    FileIdentity::from_metadata(&descriptor_metadata) == identity
        && FileIdentity::from_metadata(&path_metadata) == identity
        && admit_store_custody(StoreRole::Root, root_custody_facts(&descriptor_metadata))
        && admit_store_custody(StoreRole::Root, root_custody_facts(&path_metadata))
}

fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options.open(path)
}

fn open_or_create_store_nofollow(path: &Path) -> io::Result<File> {
    open_or_create_store_nofollow_with_missing_hook(path, || {})
}

fn open_or_create_store_nofollow_with_missing_hook(
    path: &Path,
    before_create: impl FnOnce(),
) -> io::Result<File> {
    #[cfg(unix)]
    {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "telemetry store must be absolute",
            ));
        }
        #[cfg(target_os = "macos")]
        let walk_path = if let Ok(suffix) = path.strip_prefix("/var") {
            Path::new("/private/var").join(suffix)
        } else {
            path.to_path_buf()
        };
        #[cfg(not(target_os = "macos"))]
        let walk_path = path.to_path_buf();
        let components = walk_path
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(name) => Some(Ok(name)),
                std::path::Component::RootDir => None,
                _ => Some(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "telemetry store contains an unsafe component",
                ))),
            })
            .collect::<io::Result<Vec<_>>>()?;
        if components.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "telemetry store cannot be the filesystem root",
            ));
        }

        let mut directory = open_directory_nofollow(Path::new("/"))?;
        let mut before_create = Some(before_create);
        for (index, component) in components.iter().enumerate() {
            let is_store = index + 1 == components.len();
            let name = CString::new(component.as_bytes())
                .map_err(|_| io::Error::other("invalid store component"))?;
            let mut next = open_directory_at(&directory, &name);
            if is_store
                && next
                    .as_ref()
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
            {
                if let Some(before_create) = before_create.take() {
                    before_create();
                }
                let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
                if result != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
                next = open_directory_at(&directory, &name);
            }
            let next = next?;
            let metadata = next.metadata()?;
            if is_store {
                if !admit_store_custody(StoreRole::Root, root_custody_facts(&metadata)) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "unsafe telemetry store",
                    ));
                }
            } else if !admit_parent_directory(&metadata) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe telemetry store parent",
                ));
            }
            directory = next;
        }
        Ok(directory)
    }
    #[cfg(not(unix))]
    {
        let _ = before_create;
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path)?,
            Err(error) => return Err(error),
        }
        open_directory_nofollow(path)
    }
}

#[cfg(unix)]
fn open_directory_at(parent: &File, name: &CStr) -> io::Result<File> {
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

#[cfg(unix)]
fn open_private_directory_at(
    parent: &File,
    parent_path: &Path,
    name: &std::ffi::OsStr,
    create: bool,
) -> io::Result<Option<OpenDirectory>> {
    open_private_directory_at_with_missing_hook(parent, parent_path, name, create, || {})
}

#[cfg(unix)]
fn open_private_directory_at_with_missing_hook(
    parent: &File,
    parent_path: &Path,
    name: &std::ffi::OsStr,
    create: bool,
    before_create: impl FnOnce(),
) -> io::Result<Option<OpenDirectory>> {
    if name.as_bytes().contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private directory name must be one component",
        ));
    }
    let name_c = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid directory name"))?;
    let mut directory = open_directory_at(parent, &name_c);
    if directory
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        if !create {
            return Ok(None);
        }
        before_create();
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        directory = open_directory_at(parent, &name_c);
    }
    let directory = directory?;
    let descriptor_metadata = directory.metadata()?;
    let path = parent_path.join(name);
    let path_metadata = fs::symlink_metadata(&path)?;
    let identity = FileIdentity::from_metadata(&descriptor_metadata);
    if FileIdentity::from_metadata(&path_metadata) != identity
        || !admit_store_custody(
            StoreRole::PrivateDirectory,
            root_custody_facts(&descriptor_metadata),
        )
        || !admit_store_custody(
            StoreRole::PrivateDirectory,
            root_custody_facts(&path_metadata),
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe private directory custody",
        ));
    }
    Ok(Some(OpenDirectory {
        file: directory,
        path,
        name: name.to_os_string(),
        identity,
    }))
}

#[cfg(unix)]
fn private_directory_binding_is_valid(parent: &File, child: &OpenDirectory) -> bool {
    let Ok(name) = CString::new(child.name.as_bytes()) else {
        return false;
    };
    let Ok(reopened) = open_directory_at(parent, &name) else {
        return false;
    };
    let Ok(metadata) = reopened.metadata() else {
        return false;
    };
    FileIdentity::from_metadata(&metadata) == child.identity
        && admit_store_custody(StoreRole::PrivateDirectory, root_custody_facts(&metadata))
}

#[cfg(unix)]
fn lock_directory(file: &File, exclusive: bool) -> io::Result<()> {
    let lock = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    if unsafe { libc::flock(file.as_raw_fd(), lock | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn open_private_directory_at(
    _parent: &File,
    _parent_path: &Path,
    _name: &std::ffi::OsStr,
    _create: bool,
) -> io::Result<Option<OpenDirectory>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private directories require descriptor-relative access",
    ))
}

#[cfg(not(unix))]
fn private_directory_binding_is_valid(_parent: &File, _child: &OpenDirectory) -> bool {
    false
}

#[cfg(not(unix))]
fn lock_directory(_file: &File, _exclusive: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory locking is unavailable",
    ))
}

#[cfg(unix)]
fn admit_parent_directory(metadata: &fs::Metadata) -> bool {
    let owner = metadata.uid();
    metadata.is_dir()
        && (owner == 0 || owner == unsafe { libc::geteuid() })
        && metadata.mode() & 0o022 == 0
}

fn create_shard_at(root: &File, name: &str, _root_path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        let name = CString::new(name).map_err(|_| io::Error::other("invalid shard name"))?;
        let descriptor = unsafe {
            libc::openat(
                root.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY
                    | libc::O_APPEND
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        OpenOptions::new()
            .write(true)
            .append(true)
            .create_new(true)
            .open(_root_path.join(name))
    }
}

#[cfg(unix)]
struct DirectoryCursor {
    directory: NonNull<libc::DIR>,
}

#[cfg(unix)]
impl DirectoryCursor {
    fn open(directory: &File) -> io::Result<Self> {
        let current = CString::new(".").expect("static directory component is valid");
        let independent = open_directory_at(directory, &current)?;
        let descriptor = independent.into_raw_fd();
        let directory = unsafe { libc::fdopendir(descriptor) };
        let Some(directory) = NonNull::new(directory) else {
            let error = io::Error::last_os_error();
            unsafe { libc::close(descriptor) };
            return Err(error);
        };
        Ok(Self { directory })
    }

    fn next_name(&mut self) -> io::Result<Option<OsString>> {
        loop {
            clear_errno();
            let entry = unsafe { libc::readdir(self.directory.as_ptr()) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error);
                }
                return Ok(None);
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes != b"." && bytes != b".." {
                return Ok(Some(OsString::from_vec(bytes.to_vec())));
            }
        }
    }
}

#[cfg(unix)]
impl Drop for DirectoryCursor {
    fn drop(&mut self) {
        unsafe { libc::closedir(self.directory.as_ptr()) };
    }
}

#[cfg(not(unix))]
struct DirectoryCursor;

#[cfg(not(unix))]
impl DirectoryCursor {
    fn open(_directory: &File) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retention requires descriptor-relative enumeration",
        ))
    }

    fn next_name(&mut self) -> io::Result<Option<OsString>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retention requires descriptor-relative enumeration",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn clear_errno() {
    unsafe { *libc::__error() = 0 };
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn clear_errno() {
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "linux",
        target_os = "android"
    ))
))]
fn clear_errno() {}

struct RetentionCandidate {
    _file: File,
    identity: FileIdentity,
}

struct PlannedRetentionCandidate {
    name: OsString,
    candidate: RetentionCandidate,
}

struct PlannedRetentionDay {
    name: OsString,
    directory: OpenDirectory,
    candidates: Vec<PlannedRetentionCandidate>,
    complete: bool,
}

fn open_retention_candidate(
    root: &File,
    name: &std::ffi::OsStr,
    acquire_writer_exclusion: bool,
) -> io::Result<Option<RetentionCandidate>> {
    #[cfg(unix)]
    {
        let name =
            CString::new(name.as_bytes()).map_err(|_| io::Error::other("invalid entry name"))?;
        let descriptor = unsafe {
            libc::openat(
                root.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Ok(None);
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file.metadata()?;
        if !admit_store_custody(StoreRole::Shard, custody_facts(&metadata)) {
            return Ok(None);
        }
        if acquire_writer_exclusion
            && unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
        {
            return Ok(None);
        }
        Ok(Some(RetentionCandidate {
            _file: file,
            identity: FileIdentity::from_metadata(&metadata),
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        let _ = name;
        let _ = acquire_writer_exclusion;
        Ok(None)
    }
}

fn remove_verified_retention_candidate(
    root: &File,
    name: &std::ffi::OsStr,
    expected_identity: FileIdentity,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let quarantine = OsString::from(format!(
            "{RETENTION_FILE_QUARANTINE_PREFIX}{}",
            Uuid::new_v4().simple()
        ));
        rename_noreplace_relative(root, name, &quarantine)?;

        let observed = open_retention_candidate(root, &quarantine, false);
        if !matches!(
            observed,
            Ok(Some(ref candidate)) if candidate.identity == expected_identity
        ) {
            let restore = rename_noreplace_relative(root, &quarantine, name);
            return Err(match restore {
                Ok(()) => io::Error::other("retention candidate identity changed before deletion"),
                Err(error) => io::Error::other(format!(
                    "retention candidate identity changed and could not be restored: {error}"
                )),
            });
        }

        unlink_relative(root, &quarantine)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        let _ = name;
        let _ = expected_identity;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retention requires descriptor-relative identity binding",
        ))
    }
}

fn remove_verified_retention_directory(
    root: &File,
    name: &std::ffi::OsStr,
    expected_identity: FileIdentity,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let quarantine = OsString::from(format!(
            "{RETENTION_DIRECTORY_QUARANTINE_PREFIX}{}",
            Uuid::new_v4().simple()
        ));
        rename_noreplace_relative(root, name, &quarantine)?;

        let quarantine_c = CString::new(quarantine.as_bytes())
            .map_err(|_| io::Error::other("invalid quarantine directory name"))?;
        let observed = open_directory_at(root, &quarantine_c).and_then(|directory| {
            let metadata = directory.metadata()?;
            Ok((FileIdentity::from_metadata(&metadata), metadata))
        });
        if !matches!(
            observed,
            Ok((identity, ref metadata))
                if identity == expected_identity
                    && admit_store_custody(
                        StoreRole::PrivateDirectory,
                        root_custody_facts(metadata),
                    )
        ) {
            let restore = rename_noreplace_relative(root, &quarantine, name);
            return Err(match restore {
                Ok(()) => io::Error::other("retention directory identity changed before deletion"),
                Err(error) => io::Error::other(format!(
                    "retention directory identity changed and could not be restored: {error}"
                )),
            });
        }

        remove_directory_relative(root, &quarantine)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        let _ = name;
        let _ = expected_identity;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retention requires descriptor-relative identity binding",
        ))
    }
}

fn rename_noreplace_relative(
    root: &File,
    from: &std::ffi::OsStr,
    to: &std::ffi::OsStr,
) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let from = CString::new(from.as_bytes())
            .map_err(|_| io::Error::other("invalid source entry name"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| io::Error::other("invalid destination entry name"))?;
        if unsafe {
            libc::renameat2(
                root.as_raw_fd(),
                from.as_ptr(),
                root.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let from = CString::new(from.as_bytes())
            .map_err(|_| io::Error::other("invalid source entry name"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| io::Error::other("invalid destination entry name"))?;
        if unsafe {
            libc::renameatx_np(
                root.as_raw_fd(),
                from.as_ptr(),
                root.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        let _ = root;
        let _ = from;
        let _ = to;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retention requires atomic no-replace rename",
        ))
    }
}

fn unlink_relative(root: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    #[cfg(unix)]
    {
        let name =
            CString::new(name.as_bytes()).map_err(|_| io::Error::other("invalid entry name"))?;
        if unsafe { libc::unlinkat(root.as_raw_fd(), name.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        let _ = name;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retention requires descriptor-relative deletion",
        ))
    }
}

fn remove_directory_relative(root: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    #[cfg(unix)]
    {
        let name =
            CString::new(name.as_bytes()).map_err(|_| io::Error::other("invalid entry name"))?;
        if unsafe { libc::unlinkat(root.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        let _ = name;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retention requires descriptor-relative directory deletion",
        ))
    }
}

#[cfg(test)]
fn write_private_test_replacement(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
fn replace_private_test_directory(path: &Path) -> io::Result<()> {
    let displaced = path.with_file_name(format!(
        "{}-displaced",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("invalid test directory name"))?
    ));
    fs::rename(path, displaced)?;
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

struct SharedProducer {
    producer: LifecycleProducer,
    ingress: Option<RecorderIngress>,
}

struct WriterCompletion(mpsc::Sender<()>);

impl Drop for WriterCompletion {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

struct WriterThread {
    handle: Option<thread::JoinHandle<()>>,
    completed: SyncReceiver<()>,
}

impl WriterThread {
    fn join_until(mut self, deadline: Instant) -> bool {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.completed.recv_timeout(remaining) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => self
                .handle
                .take()
                .is_some_and(|handle| handle.join().is_ok()),
            Err(mpsc::RecvTimeoutError::Timeout) => false,
        }
    }

    #[cfg(test)]
    fn join(mut self) -> thread::Result<()> {
        self.handle.take().expect("writer handle is present").join()
    }
}

pub(super) struct Monitor {
    liveness: LivenessMonitor,
    probe_sender: async_channel::Sender<u64>,
    acknowledgement: Arc<AtomicAcknowledgement>,
    last_stable_acknowledgement: AcknowledgementSample,
    producer: Arc<Mutex<SharedProducer>>,
    terminal: Arc<AtomicBool>,
    clock: Arc<dyn ClockSource>,
}

pub(super) fn start(cx: &mut App) -> Option<Monitor> {
    let source = SourceIdentity::from_app(cx)?;
    let disabled_at_restart = restart_telemetry_disabled(
        std::env::var_os("ZED_10X_TELEMETRY_DISABLED")
            .as_deref()
            .and_then(std::ffi::OsStr::to_str),
    );
    let launch = LaunchIdentity::fresh().ok()?;
    let config = RecorderConfig::for_app(source, disabled_at_restart);
    let clock: Arc<dyn ClockSource> = Arc::new(SystemClock::new());
    start_configured(cx, config, launch, clock).ok()
}

fn start_configured(
    cx: &mut App,
    config: RecorderConfig,
    launch: LaunchIdentity,
    clock: Arc<dyn ClockSource>,
) -> Result<Monitor, StoreOpenError> {
    let (mut ingress, writer_thread) = spawn_writer_with_clock(config, launch, clock.clone())?;

    let mut producer = LifecycleProducer::new();
    let _ = producer.offer_launch(&mut ingress, clock.sample().suspend_aware);
    let producer = Arc::new(Mutex::new(SharedProducer {
        producer,
        ingress: Some(ingress),
    }));
    let terminal = Arc::new(AtomicBool::new(false));

    let (probe_sender, probe_receiver) = async_channel::bounded(1);
    let acknowledgement = Arc::new(AtomicAcknowledgement::new());
    start_foreground_acknowledger(probe_receiver, acknowledgement.clone(), clock.clone(), cx);

    cx.on_app_quit({
        let producer = producer.clone();
        let terminal = terminal.clone();
        let clock = clock.clone();
        let mut writer_thread = Some(writer_thread);
        move |cx| {
            terminal.store(true, Ordering::Release);
            let producer = producer.clone();
            let clock = clock.clone();
            let writer_thread = writer_thread.take();
            let shutdown = cx.background_spawn(async move {
                let deadline = Instant::now() + WRITER_SHUTDOWN_TIMEOUT;
                let ingress = {
                    let mut shared = producer.lock();
                    let Some(mut ingress) = shared.ingress.take() else {
                        return;
                    };
                    let _ = shared
                        .producer
                        .offer_clean_exit(&mut ingress, clock.sample().suspend_aware);
                    ingress
                };
                drop(ingress);
                if let Some(writer_thread) = writer_thread {
                    let _ = writer_thread.join_until(deadline);
                }
            });
            async move {
                shutdown.await;
            }
        }
    })
    .detach();

    Ok(Monitor {
        liveness: LivenessMonitor::new(LIVENESS_INTERVAL, LIVENESS_THRESHOLD),
        probe_sender,
        acknowledgement,
        last_stable_acknowledgement: AcknowledgementSample::initial(),
        producer,
        terminal,
        clock,
    })
}

// Recorder construction is side-effect free. Filesystem work starts only after a bounded event
// reaches this background writer.
#[cfg(test)]
fn spawn_writer(
    config: RecorderConfig,
    launch: LaunchIdentity,
) -> Result<(RecorderIngress, WriterThread), StoreOpenError> {
    spawn_writer_with_clock(config, launch, Arc::new(SystemClock::new()))
}

fn spawn_writer_with_clock(
    config: RecorderConfig,
    launch: LaunchIdentity,
    clock: Arc<dyn ClockSource>,
) -> Result<(RecorderIngress, WriterThread), StoreOpenError> {
    let recorder = Recorder::open(config, launch)?;
    let (ingress, receiver) = RecorderIngress::bounded(8);
    let (completed_sender, completed) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("Zed10xLivenessWriter".to_owned())
        .spawn(move || {
            let _completion = WriterCompletion(completed_sender);
            run_writer(recorder, receiver, clock);
        })
        .map_err(|_| StoreOpenError::WriterThreadUnavailable)?;
    Ok((
        ingress,
        WriterThread {
            handle: Some(handle),
            completed,
        },
    ))
}

fn run_writer(
    mut recorder: Recorder,
    receiver: SyncReceiver<PendingEvent>,
    clock: Arc<dyn ClockSource>,
) {
    let mut retention = RetentionState;
    while let Ok(event) = receiver.recv() {
        let _ = recorder.append_event(
            &mut retention,
            event.sequence,
            event.fact,
            event.event_time,
            clock.sample().suspend_aware,
        );
    }
}

impl Monitor {
    pub(super) fn poll(&mut self) {
        if self.terminal.load(Ordering::Acquire) {
            return;
        }
        if let Some(acknowledgement) = self.acknowledgement.try_load() {
            self.last_stable_acknowledgement = acknowledgement;
        }
        let sample = self.clock.sample();
        let transitions =
            self.liveness
                .tick(sample, self.last_stable_acknowledgement, |sequence| {
                    try_send_probe(&self.probe_sender, sequence)
                });
        if transitions.is_none() {
            return;
        }
        let mut shared = self.producer.lock();
        let SharedProducer { producer, ingress } = &mut *shared;
        let Some(ingress) = ingress else {
            return;
        };
        let _ = producer.offer_liveness_batch(ingress, transitions);
    }
}
