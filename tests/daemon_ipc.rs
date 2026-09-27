use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use patpans::backend::sim::SimBackend;
use patpans::config::{Config, FileConfig};
use patpans::daemon::{self, Daemon};
use patpans::ipc::{Client, Request};

fn unique_endpoint(tag: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let index = COUNTER.fetch_add(1, Ordering::SeqCst);
    #[cfg(target_os = "linux")]
    {
        std::env::temp_dir()
            .join(format!("patpans-test-{tag}-{index}.sock"))
            .display()
            .to_string()
    }
    #[cfg(not(target_os = "linux"))]
    {
        format!("patpans-test-{tag}-{index}")
    }
}

fn spawn_daemon(endpoint: &str, config_path: PathBuf, config: Config) -> Daemon {
    daemon::Daemon::spawn(
        config,
        daemon::Options {
            config_path,
            endpoint: Some(endpoint.to_string()),
            tray: false,
        },
        |control, commands| {
            Ok(Box::new(
                SimBackend::default().with_control(control, commands),
            ))
        },
    )
    .expect("daemon must start")
}

fn temp_config_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("patpans-test-{tag}.toml"))
}

#[test]
fn ipc_round_trip() {
    let endpoint = unique_endpoint("roundtrip");
    let config_path = temp_config_path("roundtrip");
    let daemon = spawn_daemon(&endpoint, config_path.clone(), Config::default());
    let mut client = Client::connect(Some(&endpoint)).expect("client must connect");

    let status = client
        .request(&Request::Status)
        .unwrap()
        .status
        .expect("status payload");
    assert!(status.enabled);
    assert_eq!(status.toggle.as_deref(), Some("F8"));
    assert_eq!(status.groups.len(), 2);

    let toggled = client.request(&Request::Toggle).unwrap().status.unwrap();
    assert!(!toggled.enabled);
    let toggled = client.request(&Request::Toggle).unwrap().status.unwrap();
    assert!(toggled.enabled);

    let disabled = client
        .request(&Request::SetEnabled { enabled: false })
        .unwrap()
        .status
        .unwrap();
    assert!(!disabled.enabled);

    let file = FileConfig {
        toggle: "F9".to_string(),
        sticky: false,
        tray: false,
        groups: vec![vec!["Up".to_string(), "Down".to_string()]],
    };
    file.save(&config_path).unwrap();
    let reloaded = client.request(&Request::Reload).unwrap().status.unwrap();
    assert_eq!(reloaded.toggle.as_deref(), Some("F9"));
    assert!(!reloaded.sticky);
    assert_eq!(
        reloaded.groups,
        vec![["Up".to_string(), "Down".to_string()]]
    );

    let captured = client.request(&Request::CaptureKey).unwrap();
    assert!(captured.ok);
    assert!(captured.key.is_none());

    assert!(client.request(&Request::Ping).unwrap().ok);
    assert!(client.request(&Request::Stop).unwrap().ok);
    daemon.wait().unwrap();
    let _ = std::fs::remove_file(&config_path);
}

#[test]
fn protocol_version_mismatch_is_reported() {
    let endpoint = unique_endpoint("version");
    let config_path = temp_config_path("version");
    let daemon = spawn_daemon(&endpoint, config_path.clone(), Config::default());

    let mut stream = patpans::ipc::transport::connect(Some(&endpoint)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    writeln!(
        reader.get_mut(),
        r#"{{"version":999,"request":{{"cmd":"status"}}}}"#
    )
    .unwrap();
    reader.get_mut().flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let response: patpans::ipc::Response = serde_json::from_str(&line).unwrap();
    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("protocol version mismatch")
    );

    let mut client = Client::connect(Some(&endpoint)).unwrap();
    assert!(client.request(&Request::Stop).unwrap().ok);
    daemon.wait().unwrap();
    let _ = std::fs::remove_file(&config_path);
}

#[test]
fn second_daemon_is_rejected() {
    let endpoint = unique_endpoint("single");
    let config_path = temp_config_path("single");
    let first = spawn_daemon(&endpoint, config_path.clone(), Config::default());

    let second = daemon::Daemon::spawn(
        Config::default(),
        daemon::Options {
            config_path: config_path.clone(),
            endpoint: Some(endpoint.clone()),
            tray: false,
        },
        |control, commands| {
            Ok(Box::new(
                SimBackend::default().with_control(control, commands),
            ))
        },
    );
    assert!(second.is_err(), "a second daemon must not start");

    let mut client = Client::connect(Some(&endpoint)).unwrap();
    assert!(client.request(&Request::Stop).unwrap().ok);
    first.wait().unwrap();
    let _ = std::fs::remove_file(&config_path);
}
