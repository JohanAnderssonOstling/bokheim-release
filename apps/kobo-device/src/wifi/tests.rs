use super::*;
use std::os::unix::net::UnixDatagram;

#[test]
fn separates_authentication_from_address_acquisition() {
    assert_eq!(phase(&fields("wpa_state=4WAY_HANDSHAKE\n")), Phase::Connecting);
    assert_eq!(phase(&fields("wpa_state=COMPLETED\n")), Phase::ObtainingAddress);
    assert_eq!(phase(&fields("wpa_state=COMPLETED\nip_address=0.0.0.0\n")), Phase::ObtainingAddress);
    assert_eq!(phase(&fields("wpa_state=COMPLETED\nip_address=192.168.2.5\n")), Phase::Connected);
    assert_eq!(phase(&fields("wpa_state=DISCONNECTED\nip_address=192.168.2.5\n")), Phase::Disconnected);
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
fn saved_network_ids_are_numeric_and_restore_only_previously_enabled_networks() {
    let text = "network id / ssid / bssid / flags\n4\tHome\\tWiFi\tany\t[CURRENT]\n8\tOther\tany\t[DISABLED]\n9\tCafe\tany\t\nall\tInvalid\tany\t\n";
    assert_eq!(parse_saved(text).len(), 3);
    assert_eq!(parse_saved(text)[0].name, "Home\tWiFi");
    assert_eq!(enabled_networks(text), vec![4, 9]);
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
    assert!(!error.contains("secret-test-value"));
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

#[test]
fn interface_names_cannot_be_paths_or_options() {
    assert!(valid_interface("eth0"));
    assert!(valid_interface("wlan0"));
    for invalid in ["", "-a", "../../tmp", "wifi;id", "wifi name", "interface-name-too-long"] {
        assert!(!valid_interface(invalid));
    }
}

#[test]
fn ssid_escape_decode_does_not_double_decode_literals() {
    assert_eq!(decode_ssid(r"Cafe\\x20"), b"Cafe\\x20");
    assert_eq!(decode_ssid(r"A\x20B\tC"), b"A B\tC");
    assert_eq!(hex(b"a\";\n"), "61223b0a");
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
}

#[test]
fn wake_waits_for_ui_then_restores_only_once() {
    let mut controller = Controller::new();
    controller.socket_path = None;
    controller.snapshot.phase = Phase::Connected;
    controller
        .prepare_sleep_with(|controller| {
            controller.snapshot.phase = Phase::Off;
            Ok(())
        })
        .unwrap();
    controller.resume_after_ui_with(|_| panic!("must not start during suspend"));
    controller.sleeping = false;
    assert_eq!(controller.snapshot.phase, Phase::Off);
    assert!(controller.restore_after_ui);
    controller.resume_after_ui_with(|controller| {
        controller.snapshot.phase = Phase::Connecting;
        Ok(())
    });
    assert_eq!(controller.snapshot.phase, Phase::Connecting);
    controller.resume_after_ui_with(|_| panic!("duplicate wake notification"));
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
fn failed_wake_startup_reports_error_without_retry_loop() {
    let mut controller = Controller::new();
    controller.restore_after_ui = true;
    controller.resume_after_ui_with(|_| Err("radio unavailable".into()));
    assert_eq!(controller.snapshot.error.as_deref(), Some("radio unavailable"));
    assert!(!controller.sleeping);
    controller.resume_after_ui_with(|_| panic!("must not retry automatically"));
}

#[test]
fn sleep_timeout_rejects_a_late_worker_reply() {
    let (reply, response) = mpsc::sync_channel(0);
    assert!(wait_for_sleep(response, Duration::from_millis(10)).unwrap_err().contains("timed out"));
    assert!(reply.send(Ok(())).is_err());
}

#[test]
fn subprocess_timeout_stops_descendants_and_reports_failures() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("late-write");
    let error = run_bounded(Command::new("/bin/sh").args(["-c", "(sleep 0.2; echo late > \"$1\") & wait", "test"]).arg(&marker), Duration::from_millis(20), "Test helper").unwrap_err();
    assert!(error.contains("timed out"));
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
    let (_directory, path, server) = fake_service(vec![("STATUS", "FAIL")]);
    let mut controller = Controller::new();
    controller.socket_path = Some(path);
    controller.address_requested = true;
    assert!(controller.refresh().is_err());
    assert!(!controller.address_requested);
    server.join().unwrap();
}
