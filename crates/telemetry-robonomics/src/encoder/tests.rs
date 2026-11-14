use rohi_telemetry::catalog::*;
use rohi_telemetry::{
    DataSelection, GeoPoint, ObservationValue, PacketFormatId, Selection, SourceId,
    TelemetrySnapshot, prepare,
};
use rohi_time::MonotonicTimestamp;

use super::*;
use crate::wire::device_v1::UrbanSensor_::Sensor;
use crate::wire::sensor_v1::{BME280_, ICS43434_, SDS011_};

fn at(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn snapshot(with_humidity: bool) -> TelemetrySnapshot<16> {
    let mut snapshot = TelemetrySnapshot::new(at(10));
    let mut writer = snapshot.writer(SourceId::LOCAL);
    writer.number(BMX280_TEMPERATURE, 21.456, at(1)).unwrap();
    if with_humidity {
        writer.number(BMX280_HUMIDITY, 45.0, at(1)).unwrap();
    }
    writer.number(BMX280_PRESSURE, 100_800.04, at(1)).unwrap();
    writer.number(SDS011_PM2_5, 6.54, at(1)).unwrap();
    writer.number(SDS011_PM10, 12.0, at(1)).unwrap();
    writer.number(MICROPHONE_AVERAGE, 41.6, at(1)).unwrap();
    writer.number(SCD4X_CO2, 640.0, at(1)).unwrap();
    writer.finish(Ok(()));
    let mut writer = snapshot.writer(SourceId::LOCATION);
    writer
        .write(
            LOCATION,
            ObservationValue::Location(GeoPoint {
                latitude: 52.52,
                longitude: 13.405,
                altitude_m: None,
            }),
            at(2),
        )
        .unwrap();
    writer.finish(Ok(()));
    snapshot
}

fn profile(selection: Selection) -> rohi_telemetry::PacketProfile<2> {
    struct Caps;
    impl rohi_telemetry::FormatCapabilities for Caps {
        fn supported(&self, _: PacketFormatId) -> Selection {
            rohi_telemetry::catalog::all_cataloged()
        }
        fn accepts_recipient(
            &self,
            _: PacketFormatId,
            _: &rohi_telemetry::RecipientAddress,
        ) -> bool {
            true
        }
    }
    let mut config = rohi_telemetry::TelemetryConfig::<3, 2>::default();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    config
        .set_public("p", DataSelection::local(selection), &Caps)
        .unwrap();
    config.get("p").unwrap().clone()
}

fn encode_public(
    snapshot: &TelemetrySnapshot<16>,
    selection: Selection,
) -> Result<Sensors, EncodeError> {
    let profile = profile(selection);
    let sections = prepare(
        snapshot,
        &profile,
        &rohi_telemetry::catalog::all_cataloged(),
    )
    .unwrap();
    encode_section(&sections.public)
}

fn sensors(list: &Sensors) -> std::vec::Vec<&Sensor> {
    list.iter().filter_map(|s| s.sensor.as_ref()).collect()
}

#[test]
fn the_supported_set_is_pinned() {
    let bits: std::vec::Vec<u8> = supported().bits().map(|id| id.0).collect();
    assert_eq!(bits, [0, 1, 2, 3, 7, 8, 9, 10]);
}

#[test]
fn a_bme280_is_scaled_and_located_in_the_right_messages() {
    let snapshot = snapshot(true);
    let list = encode_public(
        &snapshot,
        Selection::NONE
            .with(BMX280_TEMPERATURE)
            .with(BMX280_HUMIDITY)
            .with(BMX280_PRESSURE)
            .with(SDS011_PM2_5)
            .with(MICROPHONE_AVERAGE),
    )
    .unwrap();
    let list = sensors(&list);
    assert_eq!(list.len(), 5);
    assert!(matches!(
        list[0],
        Sensor::Bme280(b) if matches!(b.measurement, Some(BME280_::Measurement::Temperature(t)) if t.centi_celsius == 2146)
    ));
    assert!(matches!(
        list[2],
        Sensor::Bme280(b) if matches!(b.measurement, Some(BME280_::Measurement::Pressure(p)) if p.deci_pascal == 1_008_000)
    ));
    assert!(matches!(
        list[3],
        Sensor::Sds011(s) if matches!(s.measurement, Some(SDS011_::Measurement::Pm25(p)) if p.deci_ug_m3 == 65)
    ));
    assert!(matches!(
        list[4],
        Sensor::Ics43434(s) if matches!(s.measurement, Some(ICS43434_::Measurement::NoiseAvg(n)) if n.db == 42)
    ));
}

#[test]
fn a_bmp280_is_never_relabelled_as_a_bme280() {
    let snapshot = snapshot(false);
    let list = encode_public(
        &snapshot,
        Selection::NONE
            .with(BMX280_TEMPERATURE)
            .with(BMX280_PRESSURE)
            .with(SDS011_PM10),
    )
    .unwrap();
    // Only the SDS011 value remains.
    assert_eq!(list.len(), 1);
    assert!(matches!(list[0].sensor, Some(Sensor::Sds011(_))));
}

#[test]
fn location_is_a_gps_with_zero_for_a_missing_altitude() {
    let snapshot = snapshot(true);
    let list = encode_public(&snapshot, Selection::NONE.with(LOCATION)).unwrap();
    let Some(Sensor::Gps(gps)) = &list[0].sensor else {
        panic!("a position")
    };
    assert_eq!((gps.lat, gps.lon, gps.height_m), (52.52, 13.405, 0.0));
}

#[test]
fn a_measurement_the_protocol_cannot_say_is_rejected_not_relabelled() {
    let snapshot = snapshot(true);
    assert_eq!(
        encode_public(&snapshot, Selection::NONE.with(SCD4X_CO2)),
        Err(EncodeError::Unsupported(SCD4X_CO2))
    );
}

#[test]
fn dynamic_all_excludes_location_and_unsupported_data_is_not_sent_by_accident() {
    let snapshot = snapshot(true);
    // `all` over what the format supports: no location, no SCD4x.
    let profile = profile(Selection::ALL);
    let sections = prepare(&snapshot, &profile, &supported()).unwrap();
    let list = encode_section(&sections.public).unwrap();
    assert!(
        list.iter()
            .all(|s| !matches!(s.sensor, Some(Sensor::Gps(_))))
    );
    assert_eq!(list.len(), 6);
}

#[test]
fn out_of_range_values_are_omitted() {
    assert_eq!(centi_celsius(f32::NAN), None);
    assert_eq!(centi_celsius(1.0e9), None);
    assert_eq!(deci(-1.0), None);
    assert_eq!(whole(-3.0), None);
    assert_eq!(centi_percent(140.0), Some(10_000));
}
