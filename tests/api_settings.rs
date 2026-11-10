use vispace::api_settings::ApiSettings;
#[test]
fn api_launch_preference_keeps_port_and_remembers_stop() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("api-settings.json");
    assert_eq!(ApiSettings::load(&path).unwrap(), ApiSettings::default());
    let started = ApiSettings {
        enabled: true,
        port: 18080,
    };
    started.save(&path).unwrap();
    assert_eq!(ApiSettings::load(&path).unwrap(), started);
    let stopped = ApiSettings {
        enabled: false,
        ..started
    };
    stopped.save(&path).unwrap();
    assert_eq!(ApiSettings::load(&path).unwrap(), stopped);
    std::fs::write(&path, r#"{"enabled":true,"port":80}"#).unwrap();
    assert!(ApiSettings::load(&path).is_err());
    std::fs::write(&path, "invalid").unwrap();
    assert!(ApiSettings::load(&path).is_err());
}
