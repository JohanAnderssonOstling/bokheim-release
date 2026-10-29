use super::*;
use std::os::unix::net::UnixDatagram;

#[test]
fn radio_off_and_sleep_have_no_poll_deadline() {
    let mut controller = Controller::new();
    assert_eq!(controller.poll_interval(true), None);
    controller.snapshot.phase = Phase::Connected;
    assert_eq!(controller.poll_interval(true), Some(Duration::from_secs(30)));
    controller.address_requested = true;
    assert_eq!(controller.poll_interval(true), Some(Duration::from_secs(2)));
    controller.sleeping = true;
    assert_eq!(controller.poll_interval(true), None);
}

fn attached_monitor() -> (tempfile::TempDir, PathBuf, Control, UnixDatagram, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("server");
    let server = UnixDatagram::bind(&path).unwrap();
    server.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let peer_server = server.try_clone().unwrap();
    let attaching = std::thread::spawn(move || {
        let mut bytes = [0; 32];
        let (length, peer) = peer_server.recv_from(&mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"ATTACH");
        let peer = peer.as_pathname().unwrap().to_owned();
        peer_server.send_to(b"OK\n", &peer).unwrap();
        peer
    });
    let monitor = Control::monitor(&path).unwrap();
    let peer = attaching.join().unwrap();
    (directory, path, monitor, server, peer)
}

#[test]
fn monitored_connected_and_disconnected_radios_have_no_periodic_check() {
    let (_directory, _path, monitor, _server, _peer) = attached_monitor();
    let mut controller = Controller::new();
    controller.radio_enabled = true;
    controller.monitor = Some(monitor);
    for phase in [Phase::Connected, Phase::Disconnected] {
        controller.snapshot.phase = phase;
        assert_eq!(controller.poll_interval(true), None);
        assert_eq!(controller.poll_interval(false), Some(Duration::from_secs(30)));
    }
    controller.snapshot.phase = Phase::ObtainingAddress;
    assert_eq!(controller.poll_interval(true), Some(Duration::from_secs(2)));
    controller.sleeping = true;
    assert_eq!(controller.poll_interval(false), None);
}

#[test]
fn idle_worker_blocks_until_supplicant_event_and_requests_wake_it() {
    let (_directory, path, monitor, server, peer) = attached_monitor();
    let (sender, receive, mut events) = Events::channel(Some(&path)).unwrap();
    assert!(events.available());
    let mut controller = Controller::new();
    controller.radio_enabled = true;
    controller.snapshot.phase = Phase::Connected;
    controller.monitor = Some(monitor);
    let (results, result) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        assert!(matches!(events.receive(&receive, &controller).unwrap(), Wake::Refresh { monitor_only: true, .. }));
        controller.monitor.as_ref().unwrap().events().unwrap();
        results.send(()).unwrap();
        assert!(matches!(events.receive(&receive, &controller).unwrap(), Wake::Job(Job::Refresh)));
        results.send(()).unwrap();
    });
    assert!(result.recv_timeout(Duration::from_millis(60)).is_err(), "idle Wi-Fi must stay blocked");
    server.send_to(b"<3>CTRL-EVENT-DISCONNECTED\n", &peer).unwrap();
    result.recv_timeout(Duration::from_secs(3)).unwrap();
    sender.send(Job::Refresh).unwrap();
    result.recv_timeout(Duration::from_secs(3)).unwrap();
    waiter.join().unwrap();
}

#[test]
fn replacing_supplicant_socket_wakes_the_worker_without_a_timer() {
    let (directory, path, monitor, _server, _peer) = attached_monitor();
    let (_sender, receive, mut events) = Events::channel(Some(&path)).unwrap();
    let mut controller = Controller::new();
    controller.monitor = Some(monitor);
    controller.radio_enabled = true;
    controller.snapshot.phase = Phase::Connected;
    // Unrelated directory entries do not invalidate the monitor.
    fs::write(directory.path().join("unrelated"), b"").unwrap();
    assert_eq!(events.service_changes().unwrap(), (false, false));
    fs::remove_file(&path).unwrap();
    let _replacement = UnixDatagram::bind(&path).unwrap();
    assert!(matches!(events.receive(&receive, &controller).unwrap(), Wake::Refresh { service_changed: true, .. }));
}

#[test]
fn sleeping_worker_ignores_hardware_events_and_still_accepts_wake_requests() {
    let (_directory, path, monitor, server, peer) = attached_monitor();
    let (sender, receive, mut events) = Events::channel(Some(&path)).unwrap();
    let mut controller = Controller::new();
    controller.monitor = Some(monitor);
    controller.radio_enabled = true;
    controller.sleeping = true;
    server.send_to(b"<3>CTRL-EVENT-DISCONNECTED\n", &peer).unwrap();
    fs::remove_file(&path).unwrap();
    let (results, result) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let wake = events.receive(&receive, &controller).unwrap();
        results.send(matches!(wake, Wake::Job(Job::FinishSleep))).unwrap();
    });
    assert!(result.recv_timeout(Duration::from_millis(60)).is_err());
    sender.send(Job::FinishSleep).unwrap();
    assert!(result.recv_timeout(Duration::from_secs(3)).unwrap());
    waiter.join().unwrap();
}

#[test]
fn event_refresh_preserves_changes_arriving_during_the_status_query() {
    let (_directory, path, monitor, server, peer) = attached_monitor();
    let serving = std::thread::spawn(move || {
        let mut bytes = [0; 256];
        for (expected, reply) in [("STATUS", "wpa_state=COMPLETED\nip_address=192.168.1.2\n"), ("LIST_NETWORKS", "header\n")] {
            let (length, client) = server.recv_from(&mut bytes).unwrap();
            assert_eq!(&bytes[..length], expected.as_bytes());
            // The event describes a change newer than this status response.
            if expected == "STATUS" { server.send_to(b"<3>CTRL-EVENT-DISCONNECTED\n", &peer).unwrap(); }
            server.send_to(reply.as_bytes(), client.as_pathname().unwrap()).unwrap();
        }
    });
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.monitor = Some(monitor);
    controller.refresh().unwrap();
    serving.join().unwrap();
    assert_eq!(controller.snapshot.phase, Phase::Connected);
    assert!(controller.monitor.as_ref().unwrap().events().unwrap().iter().any(|event| event.contains("CTRL-EVENT-DISCONNECTED")));
}

#[test]
fn background_scan_chatter_is_drained_without_status_commands() {
    let (_directory, path, monitor, server, peer) = attached_monitor();
    for message in ["CTRL-EVENT-BSS-ADDED 1", "CTRL-EVENT-BSS-REMOVED 1", "CTRL-EVENT-SIGNAL-CHANGE", "CTRL-EVENT-SCAN-STARTED", "CTRL-EVENT-SCAN-RESULTS"] {
        server.send_to(message.as_bytes(), &peer).unwrap();
    }
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.monitor = Some(monitor);
    controller.refresh_events().unwrap();
    server.set_read_timeout(Some(Duration::from_millis(25))).unwrap();
    assert!(server.recv(&mut [0; 256]).is_err());
    assert!(controller.monitor.as_ref().unwrap().events().unwrap().is_empty());
}

#[test]
fn scan_completion_event_updates_results_immediately() {
    let (_directory, path, monitor, server, peer) = attached_monitor();
    server.send_to(b"<3>CTRL-EVENT-SCAN-RESULTS\n", &peer).unwrap();
    let serving = std::thread::spawn(move || {
        for (expected, reply) in [
            ("STATUS", "wpa_state=DISCONNECTED\n"),
            ("LIST_NETWORKS", "header\n"),
            ("SCAN_RESULTS", "header\naa:bb:cc:dd:ee:ff\t2412\t-60\t[ESS]\tCafe\n"),
        ] {
            let mut bytes = [0; 256];
            let (length, client) = server.recv_from(&mut bytes).unwrap();
            assert_eq!(&bytes[..length], expected.as_bytes());
            server.send_to(reply.as_bytes(), client.as_pathname().unwrap()).unwrap();
        }
    });
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.monitor = Some(monitor);
    controller.scan_started = Some(Instant::now());
    controller.snapshot.scanning = true;
    controller.refresh_events().unwrap();
    assert!(!controller.snapshot.scanning);
    assert_eq!(controller.snapshot.networks[0].ssid, b"Cafe");
    serving.join().unwrap();
}

#[test]
fn temporary_service_loss_keeps_recovery_polling_for_an_enabled_radio() {
    let mut controller = Controller::new();
    controller.radio_enabled = true;
    controller.snapshot.phase = Phase::Connected;
    controller.service_lost();
    assert_eq!(controller.snapshot.phase, Phase::Off);
    assert_eq!(controller.poll_interval(true), Some(Duration::from_secs(30)));
}

#[test]
fn connection_waits_for_status_changes_and_unchanged_snapshots_do_not_wake_observers() {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};
    let (sender, receiver) = watch::channel(Snapshot::default());
    let mut observer = receiver.clone();
    let waiting = wait_for_connection_changes(receiver);
    let mut waiting = pin!(waiting);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    publish_snapshot(&sender, &Snapshot::default());
    assert!(!observer.has_changed().unwrap());
    let mut snapshot = Snapshot { phase: Phase::ObtainingAddress, ..Snapshot::default() };
    publish_snapshot(&sender, &snapshot);
    assert!(observer.has_changed().unwrap());
    observer.borrow_and_update();
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    snapshot.phase = Phase::Connected;
    publish_snapshot(&sender, &snapshot);
    assert!(matches!(waiting.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
    observer.borrow_and_update();
    publish_snapshot(&sender, &snapshot);
    assert!(!observer.has_changed().unwrap());
    snapshot.phase = Phase::Off;
    publish_snapshot(&sender, &snapshot);
    let mut offline_again = pin!(wait_for_connection_changes(observer));
    assert!(offline_again.as_mut().poll(&mut cx).is_pending());
}


#[test]
fn scan_groups_only_matching_ssid_and_security_and_preserves_bytes() {
    let text = "bssid / frequency / signal level / flags / ssid\n\
        aa:bb:cc:dd:ee:01\t2412\t-70\t[WPA2-PSK-CCMP][ESS]\tCafe\\x20\\xff\n\
        aa:bb:cc:dd:ee:02\t2417\t-40\t[WPA2-PSK-CCMP][ESS]\tCafe\\x20\\xff\n\
        aa:bb:cc:dd:ee:03\t2412\t-60\t[ESS]\tCafe\\x20\\xff\n\
        aa:bb:cc:dd:ee:04\t2412\t-65\t[WPA2-EAP-CCMP][ESS]\tOffice\n";
    let networks = parse_scan(text).unwrap();
    assert_eq!(networks.len(), 3);
    assert_eq!(networks[0].ssid, b"Cafe \xff");
    assert_eq!(networks[0].signal, -40);
    assert_eq!(networks[1].security, Security::Open);
    assert_eq!(networks[2].security, Security::Unsupported);
    assert!(parse_scan("header\ntruncated row").is_err());
}


#[test]
fn control_uses_a_private_socket_and_does_not_expose_secrets_in_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("server");
    let server = UnixDatagram::bind(&path).unwrap();
    server.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let worker = std::thread::spawn(move || {
        let mut buffer = [0; 256];
        let (length, sender) = server.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], b"SET_NETWORK 4 psk secret-test-value");
        server.send_to(b"FAIL\n", sender.as_pathname().unwrap()).unwrap();
    });
    let client = Control::open(&path).unwrap();
    let error = client.command("SET_NETWORK 4 psk secret-test-value").unwrap_err();
    assert!(!error.message.contains("secret-test-value"));
    worker.join().unwrap();
}

#[test]
fn monitor_receives_scan_completion_without_mixing_command_responses() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("server");
    let server = UnixDatagram::bind(&path).unwrap();
    server.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let worker = std::thread::spawn(move || {
        let mut buffer = [0; 32];
        let (_, sender) = server.recv_from(&mut buffer).unwrap();
        let peer = sender.as_pathname().unwrap();
        server.send_to(b"OK\n", peer).unwrap();
        server.send_to(b"<3>CTRL-EVENT-SCAN-RESULTS\n", peer).unwrap();
    });
    let monitor = Control::monitor(&path).unwrap();
    worker.join().unwrap();
    assert!(monitor.events().unwrap().iter().any(|event| event.contains("CTRL-EVENT-SCAN-RESULTS")));
    assert!(monitor.events().unwrap().is_empty());
}


fn fake_service(replies: Vec<(&'static str, &'static str)>) -> (tempfile::TempDir, PathBuf, std::thread::JoinHandle<()>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("server");
    let server = UnixDatagram::bind(&path).unwrap();
    server.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let worker = std::thread::spawn(move || {
        for (expected, reply) in replies {
            let mut buffer = [0; 4096];
            let (length, peer) = server.recv_from(&mut buffer).unwrap();
            let command = std::str::from_utf8(&buffer[..length]).unwrap();
            assert!(command.starts_with(expected), "unexpected command kind");
            server.send_to(reply.as_bytes(), peer.as_pathname().unwrap()).unwrap();
        }
    });
    (directory, path, worker)
}

#[test]
fn failed_password_configuration_removes_only_its_temporary_network() {
    let (_directory, path, server) = fake_service(vec![("ADD_NETWORK", "7"), ("SET_NETWORK 7 ssid 43616665", "OK"), ("SET_NETWORK 7 key_mgmt WPA-PSK", "OK"), ("SET_NETWORK 7 psk ", "FAIL"), ("REMOVE_NETWORK 7", "OK")]);
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    let result = controller.join(Network { ssid: b"Cafe".to_vec(), signal: -50, security: Security::WpaPersonal }, "test-password".into());
    assert!(result.is_err());
    assert!(controller.attempt.is_none());
    server.join().unwrap();
}

#[test]
fn cancelling_saved_connection_restores_previous_enabled_set_without_deleting_saved_network() {
    let (_directory, path, server) = fake_service(vec![
        ("LIST_NETWORKS", "network id / ssid / bssid / flags\n4\tHome\tany\t[CURRENT]\n8\tCafe\tany\t[DISABLED]\n"),
        ("ATTACH", "OK"),
        ("SELECT_NETWORK 8", "OK"),
        ("DISCONNECT", "OK"),
        ("DISABLE_NETWORK 8", "OK"),
        ("ENABLE_NETWORK 4", "OK"),
        ("DISCONNECT", "OK"),
    ]);
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.connect(8, false).unwrap();
    assert_eq!(controller.snapshot.phase, Phase::Connecting);
    controller.disconnect().unwrap();
    assert!(controller.attempt.is_none());
    assert_eq!(controller.snapshot.phase, Phase::Disconnected);
    server.join().unwrap();
}

#[test]
fn control_preserves_trailing_empty_scan_ssids_and_rejects_oversize_commands() {
    let response = "header\naa:bb:cc:dd:ee:ff\t2412\t-60\t[ESS]\t\n";
    let (_directory, path, server) = fake_service(vec![("SCAN_RESULTS", response)]);
    let client = Control::open(&path).unwrap();
    assert_eq!(client.command("SCAN_RESULTS").unwrap(), response.trim_end_matches('\n'));
    assert!(client.command(&"x".repeat(128)).is_err());
    server.join().unwrap();
}

#[test]
fn sleep_powers_off_without_supplicant_and_blocks_new_connections() {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("power.sh");
    fs::write(&helper, "#!/bin/sh\n[ \"$1\" = off ] && [ \"$INTERFACE\" = wlan0 ]\n").unwrap();
    let mut controller = Controller::new();
    controller.interface = Some("wlan0".into());
    controller.socket_path = Some(directory.path().join("missing-supplicant"));
    controller.snapshot.power_available = true;
    controller.snapshot.phase = Phase::Connected;
    controller.snapshot.scanning = true;
    controller.scan_started = Some(Instant::now());
    controller.address_requested = true;
    controller.attempt = Some(Attempt { id: 2, temporary: true, previous: vec![], started: Instant::now() });
    controller.prepare_sleep_with(|controller| controller.power_with_helper(false, helper.as_os_str())).unwrap();
    assert!(controller.sleeping);
    assert_eq!(controller.execute(Request::Status).unwrap().phase, Phase::Off);
    assert!(controller.execute(Request::Power(true)).is_err());
    assert!(controller.execute(Request::Scan).is_err());
    assert!(controller.execute(Request::ConnectSaved(2)).is_err());
    assert!(controller.attempt.is_none());
    assert!(controller.scan_started.is_none());
    assert!(!controller.address_requested);
    // Wake unlocks commands, without automatically powering the radio on.
    controller.sleeping = false;
    assert_eq!(controller.snapshot.phase, Phase::Off);
}

#[test]
fn failed_radio_shutdown_aborts_sleep_and_releases_operation_guard() {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("power.sh");
    fs::write(&helper, "#!/bin/sh\nexit 1\n").unwrap();
    let mut controller = Controller::new();
    controller.snapshot.power_available = true;
    controller.snapshot.phase = Phase::Connected;
    assert!(controller.prepare_sleep_with(|controller| controller.power_with_helper(false, helper.as_os_str())).is_err());
    assert!(!controller.sleeping);
    assert!(controller.snapshot.error.is_some());
    assert_ne!(controller.snapshot.phase, Phase::Off);
    assert!(controller.restore_after_ui);
    let mut restored = false;
    controller.resume_after_ui_with(|_| { restored = true; Ok(()) });
    assert!(restored, "partially stopped services must be restored after failed suspend");
    assert!(!controller.restore_after_ui);
}

#[test]
fn an_off_radio_without_a_control_helper_does_not_block_sleep() {
    let mut controller = Controller::new();
    controller.socket_path = None;
    controller.snapshot.phase = Phase::Off;
    controller.snapshot.power_available = false;
    controller.prepare_sleep_with(|_| panic!("already-off unavailable radio must not invoke the helper")).unwrap();
    assert!(controller.sleeping);
    controller.sleeping = false;
    controller.resume_after_ui_with(|_| panic!("radio must remain off"));
}

#[test]
fn wake_preserves_off_setting_and_explicit_user_override() {
    let mut controller = Controller::new();
    controller.socket_path = None;
    controller.prepare_sleep_with(|_| Ok(())).unwrap();
    controller.sleeping = false;
    controller.resume_after_ui_with(|_| panic!("Wi-Fi was off before sleep"));
    controller.restore_after_ui = true;
    let _ = controller.execute(Request::Disconnect);
    controller.resume_after_ui_with(|_| panic!("user action overrides restore"));
}


#[test]
fn subprocess_timeout_stops_descendants_and_reports_failures() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("late-write");
    let error = run_bounded(Command::new("/bin/sh").args(["-c", "(sleep 0.2; echo late > \"$1\") & wait", "test"]).arg(&marker), Duration::from_millis(20), "Test helper").unwrap_err();
    assert!(error.message.contains("timed out"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "helper descendants must not run after cancellation");
    assert!(run_bounded(Command::new("/bin/sh").args(["-c", "exit 2"]), Duration::from_secs(1), "Test helper").is_err());
}

#[test]
fn lost_supplicant_clears_connection_and_address_work() {
    let directory = tempfile::tempdir().unwrap();
    let mut controller = Controller::new();
    controller.socket_path = Some(directory.path().join("missing"));
    controller.interface = None;
    controller.attempt = Some(Attempt { id: 7, temporary: true, previous: vec![4], started: Instant::now() });
    controller.scan_started = Some(Instant::now());
    controller.address_requested = true;
    controller.address_started = Some(Instant::now());
    controller.snapshot.scanning = true;
    controller.snapshot.phase = Phase::Connecting;
    controller.dhcp = Some(Command::new("sleep").arg("10").spawn().unwrap());
    assert!(controller.refresh().is_err());
    assert!(controller.attempt.is_none());
    assert!(controller.dhcp.is_none());
    assert!(controller.scan_started.is_none());
    assert!(!controller.address_requested);
    assert!(controller.address_started.is_none());
    assert_eq!(controller.snapshot.phase, Phase::Off);
}

#[test]
fn rejected_status_also_clears_connection_work() {
    let (_directory, path, server) = fake_service(vec![("ATTACH", "OK"), ("STATUS", "FAIL")]);
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.address_requested = true;
    assert!(controller.refresh().is_err());
    assert!(!controller.address_requested);
    server.join().unwrap();
}

#[test]
fn automatic_reconnect_expires_even_before_dhcp_starts() {
    let (_directory, path, server) = fake_service(vec![
        ("ATTACH", "OK"), ("STATUS", "wpa_state=SCANNING\n"), ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
        ("DISCONNECT", "OK"),
        ("STATUS", "wpa_state=DISCONNECTED\n"), ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
    ]);
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.address_requested = true;
    controller.reconnect_started = Some(Instant::now() - Duration::from_secs(46));
    assert_eq!(controller.refresh().unwrap_err().failure, Some(WifiFailure::ConnectTimedOut));
    assert!(!controller.address_requested);
    assert!(controller.reconnect_started.is_none());
    assert!(controller.address_started.is_none());
    assert_eq!(controller.snapshot.phase, Phase::Disconnected);
    controller.refresh().unwrap();
    assert!(!controller.address_requested, "status reads must not restart an expired reconnect");
    server.join().unwrap();
}

#[test]
fn successful_automatic_reconnect_clears_its_deadline() {
    let (_directory, path, server) = fake_service(vec![
        ("ATTACH", "OK"), ("STATUS", "wpa_state=COMPLETED\nip_address=192.168.1.2\n"), ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
    ]);
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.address_requested = true;
    controller.reconnect_started = Some(Instant::now() - Duration::from_secs(46));
    controller.refresh().unwrap();
    assert_eq!(controller.snapshot.phase, Phase::Connected);
    assert!(!controller.address_requested);
    assert!(controller.reconnect_started.is_none());
    server.join().unwrap();
}
