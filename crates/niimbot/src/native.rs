use crate::*;
use anyhow::{Context, Result, bail, ensure};
use btleplug::{
    api::{
        Central, CentralState, CharPropFlags, Characteristic, Manager as _, Peripheral as _,
        ScanFilter, ValueNotification, WriteType,
    },
    platform::{Adapter, Manager, Peripheral},
};
use futures_util::{FutureExt, Stream, StreamExt};
use std::{collections::BTreeMap, pin::Pin, time::Duration};
use tokio::time::{Instant, sleep, timeout};

#[derive(Default)]
pub struct Driver {
    adapter: Option<Adapter>,
    peripherals: BTreeMap<String, Peripheral>,
}
impl Driver {
    pub async fn scan(&mut self, mut changed: impl FnMut(Vec<Device>)) -> Result<Vec<Device>> {
        if self.adapter.is_none() {
            let manager = timeout(Duration::from_secs(12), Manager::new())
                .await
                .context(
                    "Bluetooth initialization timed out. Check Bluetooth power and permissions.",
                )??;
            self.adapter = manager.adapters().await?.into_iter().next();
        }
        let adapter = self
            .adapter
            .as_ref()
            .context("No Bluetooth adapter available")?
            .clone();
        let ready_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match adapter.adapter_state().await? {
                CentralState::PoweredOn => break,
                CentralState::Unknown if Instant::now() < ready_deadline => {
                    sleep(Duration::from_millis(100)).await
                }
                _ => bail!(
                    "Bluetooth is unavailable or turned off. Check Bluetooth power and application permissions."
                ),
            }
        }
        self.peripherals.clear();
        let mut guard = ScanGuard(Some(adapter.clone()));
        adapter.start_scan(ScanFilter::default()).await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut devices = BTreeMap::new();
        while Instant::now() < deadline {
            for peripheral in adapter.peripherals().await? {
                if let Some(properties) = peripheral.properties().await.ok().flatten()
                    && let Some(name) = properties.local_name
                    && name.to_uppercase().starts_with("B1")
                {
                    let id = peripheral.id().to_string();
                    self.peripherals.insert(id.clone(), peripheral);
                    if devices.insert(id.clone(), Device { id, name }).is_none() {
                        changed(devices.values().cloned().collect());
                    }
                }
            }
            sleep(Duration::from_millis(250)).await;
        }
        adapter.stop_scan().await?;
        guard.0 = None;
        Ok(devices.into_values().collect())
    }
    pub async fn connect(&self, id: &str) -> Result<Printer<Connection>> {
        let peripheral = self
            .peripherals
            .get(id)
            .context("Scan for the printer first")?
            .clone();
        let result = timeout(Duration::from_secs(25), async {
            peripheral.connect().await?;
            peripheral.discover_services().await?;
            let service = uuid::Uuid::parse_str(SERVICE)?;
            let characteristic = uuid::Uuid::parse_str(CHARACTERISTIC)?;
            let channel = peripheral
                .characteristics()
                .into_iter()
                .find(|c| c.service_uuid == service && c.uuid == characteristic)
                .context("Niimbot service/channel was not found")?;
            ensure!(
                channel.properties.contains(CharPropFlags::NOTIFY),
                "Printer channel does not support notifications"
            );
            let kind = if channel
                .properties
                .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
            {
                WriteType::WithoutResponse
            } else {
                ensure!(
                    channel.properties.contains(CharPropFlags::WRITE),
                    "Printer channel does not support writing"
                );
                WriteType::WithResponse
            };
            let notifications = peripheral.notifications().await?;
            peripheral.subscribe(&channel).await?;
            let mut printer = Printer::new(Connection {
                peripheral: peripheral.clone(),
                channel,
                kind,
                notifications,
                closed: false,
            });
            printer.identify().await?;
            Ok::<_, anyhow::Error>(printer)
        })
        .await;
        match result {
            Ok(Ok(printer)) => Ok(printer),
            other => {
                let _ = timeout(Duration::from_secs(3), peripheral.disconnect()).await;
                match other {
                    Ok(Err(e)) => Err(e),
                    _ => bail!("Connection timed out. Close the Niimbot phone app and try again."),
                }
            }
        }
    }
}
struct ScanGuard(Option<Adapter>);
impl Drop for ScanGuard {
    fn drop(&mut self) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let Some(adapter) = self.0.take() else {
                return;
            };
            runtime.spawn(async move {
                let _ = adapter.stop_scan().await;
            });
        }
    }
}
pub struct Connection {
    peripheral: Peripheral,
    channel: Characteristic,
    kind: WriteType,
    notifications: Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
    closed: bool,
}
impl Connection {
    pub async fn is_connected(&mut self) -> bool {
        timeout(Duration::from_secs(2), self.peripheral.is_connected())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
    }
}
impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        // Use the negotiated ATT payload, with the BLE minimum as a safe fallback.
        let limit = usize::from(self.peripheral.mtu().saturating_sub(3)).max(20);
        for chunk in bytes.chunks(limit) {
            ensure!(
                self.is_connected().await,
                "Printer disconnected during upload"
            );
            timeout(
                Duration::from_secs(5),
                self.peripheral.write(&self.channel, chunk, self.kind),
            )
            .await
            .context("Bluetooth write timed out")??;
            sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }
    fn try_read(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            match self.notifications.next().now_or_never() {
                Some(Some(n)) if n.uuid == self.channel.uuid => return Ok(Some(n.value)),
                Some(Some(_)) => continue,
                Some(None) => bail!("Printer disconnected"),
                None => return Ok(None),
            }
        }
    }
    async fn read(&mut self, wait: Duration) -> Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + wait;
        loop {
            match timeout(
                deadline.saturating_duration_since(Instant::now()),
                self.notifications.next(),
            )
            .await
            {
                Ok(Some(n)) if n.uuid == self.channel.uuid => return Ok(Some(n.value)),
                Ok(Some(_)) => continue,
                Ok(None) => bail!("Printer disconnected"),
                Err(_) => {
                    ensure!(self.is_connected().await, "Printer disconnected");
                    return Ok(None);
                }
            }
        }
    }
    async fn pause(&mut self, duration: Duration) {
        sleep(duration).await;
    }
    async fn connected(&mut self) -> bool {
        self.is_connected().await
    }
    async fn disconnect(&mut self) -> Result<()> {
        let result = timeout(Duration::from_secs(5), self.peripheral.disconnect())
            .await
            .context("Disconnect timed out")?;
        result?;
        self.closed = true;
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if !self.closed
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let peripheral = self.peripheral.clone();
            runtime.spawn(async move {
                let _ = timeout(Duration::from_secs(3), peripheral.disconnect()).await;
            });
        }
    }
}
