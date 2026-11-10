use vispace::{csv_io, description_analysis::DescriptionAnalysis as Analysis, model::Part};

#[test]
fn swift_resistance_spellings_and_signed_tolerances() {
    for (raw, expected) in [
        ("4.7kΩ", "4.7kΩ"),
        ("4.7kOhm", "4.7kΩ"),
        ("4k7Ohm", "4.7kΩ"),
        ("4k7Ω", "4.7kΩ"),
        ("4MOhm", "4MΩ"),
        (".5 K ohms", ".5kΩ"),
        ("2mΩ", "2mΩ"),
    ] {
        let result = Analysis::analyze(&format!("{raw} ±1% 62.5mW 0402 Thick Film Resistor"), "");
        assert_eq!(
            (
                result.value.as_str(),
                result.label.as_str(),
                result.tolerance.as_str()
            ),
            (expected, "Resistor", "±1%"),
            "{raw}"
        );
    }
}
#[test]
fn reactive_values_take_priority_over_dcr_and_crystals_over_load_capacitance() {
    for unit in ["pF", "nF", "uF", "µF", "μF", "UF", "PF", "NF", "mF", "MF"] {
        let result = Analysis::analyze(&format!("15{unit} ±1% 50V Ceramic Capacitor C0G 0402"), "");
        let prefix = match unit {
            "µF" | "μF" | "UF" => "uF",
            "PF" => "pF",
            "NF" => "nF",
            other => other,
        };
        assert_eq!(result.value, format!("15{prefix}"));
        assert_eq!(result.label, "Capacitor");
        assert_eq!(result.tolerance, "±1%");
    }
    for unit in ["uH", "nH", "pH"] {
        let result = Analysis::analyze(
            &format!("1.7A 2.2{unit} ±20% 140mΩ 0806 Fixed Inductors"),
            "",
        );
        assert_eq!(result.value, format!("2.2{unit}"));
        assert_eq!(result.label, "Inductor");
    }
    let result = Analysis::analyze("Crystal 40MHz ±10ppm 10pF SMD3225-4P", "");
    assert_eq!(
        (
            result.value.as_str(),
            result.label.as_str(),
            result.tolerance.as_str()
        ),
        ("40MHz", "Crystal", "±10ppm")
    );
    let result = Analysis::analyze("Oscillator 25MHz +/-50ppm", "");
    assert_eq!(
        (
            result.value.as_str(),
            result.label.as_str(),
            result.tolerance.as_str()
        ),
        ("25MHz", "Oscillator", "±50ppm")
    );
}
#[test]
fn temperature_coefficients_and_unsigned_specs_are_not_tolerance() {
    for description in [
        "24kΩ ±100ppm/℃ Resistor",
        "24kΩ ±100ppm / °C Resistor",
        "24kΩ ±100ppm per degree Resistor",
        "Resistor 1% 100ppm",
        "Resistor ±2ppmm",
    ] {
        assert_eq!(
            Analysis::analyze(description, "").tolerance,
            "",
            "{description}"
        );
    }
    assert_eq!(
        Analysis::analyze("200kΩ ±100ppm/℃ ±1% Resistor", "").tolerance,
        "±1%"
    );
    assert_eq!(
        Analysis::analyze("Crystal + / - 25 PPM", "").tolerance,
        "±25ppm"
    );
}
#[test]
fn package_and_part_number_fragments_are_not_component_values() {
    for description in [
        "0402 50V 62.5mW",
        "14VC Clamp 4A Ipp TVS DIODE DFN2510-10L",
        "ABC15pF123",
        "X4k7Ω",
        "1.2.3uH",
        "capacitorish resistorish inductorish",
    ] {
        let result = Analysis::analyze(description, "");
        assert_eq!(result.value, "", "{description}");
        assert_eq!(result.label, "", "{description}");
    }
    for (value, label) in [
        ("4.7kΩ", "Resistor"),
        ("15pF", "Capacitor"),
        ("100nH", "Inductor"),
        ("IC RF WiFi 2400MHz QFN-24", "IC"),
        ("integrated circuit", "IC"),
    ] {
        assert_eq!(Analysis::analyze(value, "").label, label);
    }
}
#[test]
fn analyze_fills_blanks_and_retains_manual_fields_and_existing_labels() {
    let mut part = Part {
        description: "15pF ±1% Ceramic Capacitor".into(),
        value: "22nF".into(),
        tolerance: "5%".into(),
        labels: vec!["Manual".into(), "capacitor".into()],
        package: "CUSTOM".into(),
        ..Default::default()
    };
    csv_io::analyze(&mut part);
    assert_eq!(part.value, "22nF");
    assert_eq!(part.tolerance, "5%");
    assert_eq!(part.labels, ["Manual", "capacitor"]);
    assert_eq!(part.package, "CUSTOM");
    part.value = "  ".into();
    part.tolerance = "  ".into();
    csv_io::analyze(&mut part);
    // Swift uses a nonempty value hint as its parsing source, even if whitespace.
    assert_eq!(part.value, "");
    assert_eq!(part.tolerance, "±1%");
    csv_io::analyze(&mut part);
    assert_eq!(part.value, "15pF");
    let result = Analysis::analyze("unclassified part", "4k7Ohm");
    assert_eq!(result.label, "Resistor");
    assert_eq!(result.value, "4.7kΩ");
}
