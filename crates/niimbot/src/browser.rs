//! Web Bluetooth adapter. Uses dynamic calls so no unstable web-sys flags are required.
use crate::*;
use anyhow::{Context, Result, ensure};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    time::Duration,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_time::Instant;

fn error(value: JsValue) -> anyhow::Error {
    anyhow::anyhow!("Bluetooth: {value:?}")
}
fn property(object: &JsValue, name: &str) -> Result<JsValue> {
    js_sys::Reflect::get(object, &name.into()).map_err(error)
}
fn call(object: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue> {
    let function = property(object, name)?
        .dyn_into::<js_sys::Function>()
        .map_err(error)?;
    let array = js_sys::Array::new();
    for arg in args {
        array.push(arg);
    }
    function.apply(object, &array).map_err(error)
}
async fn delay(duration: Duration) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let callback = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let _ = web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                duration.as_millis().min(i32::MAX as u128) as i32,
            );
    });
    let _ = JsFuture::from(promise).await;
}
async fn request(value: JsValue, wait: Duration) -> Result<JsValue> {
    let promise = value.dyn_into::<js_sys::Promise>().map_err(error)?;
    let timer = js_sys::Promise::new(&mut |_, reject| {
        let callback = Closure::once_into_js(move || {
            let _ = reject.call1(&JsValue::NULL, &"Bluetooth operation timed out".into());
        });
        let _ = web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                wait.as_millis() as i32,
            );
    });
    let choices = js_sys::Array::new();
    choices.push(&promise);
    choices.push(&timer);
    JsFuture::from(js_sys::Promise::race(&choices))
        .await
        .map_err(error)
}
#[derive(Default)]
pub struct Driver {
    devices: BTreeMap<String, JsValue>,
}
impl Driver {
    /// Browser security requires this to start from the user's Scan click.
    pub async fn scan(&mut self, mut changed: impl FnMut(Vec<Device>)) -> Result<Vec<Device>> {
        let window = web_sys::window().context("Browser window unavailable")?;
        let bluetooth = property(window.navigator().as_ref(), "bluetooth")?;
        ensure!(
            !bluetooth.is_undefined(),
            "Web Bluetooth is unavailable. Use a supported browser over HTTPS or localhost."
        );
        let options = js_sys::JSON::parse(&format!(
            r#"{{"filters":[{{"namePrefix":"B1"}}],"optionalServices":["{SERVICE}"]}}"#
        ))
        .map_err(error)?;
        // Invoke requestDevice before awaiting anything (transient user activation).
        let pending = call(&bluetooth, "requestDevice", &[options])?;
        let device = JsFuture::from(pending.dyn_into::<js_sys::Promise>().map_err(error)?)
            .await
            .map_err(error)?;
        let id = property(&device, "id")?
            .as_string()
            .context("Printer has no device identifier")?;
        self.devices.insert(id, device);
        let devices = self
            .devices
            .iter()
            .map(|(id, d)| Device {
                id: id.clone(),
                name: property(d, "name")
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_else(|| "Niimbot B1".into()),
            })
            .collect::<Vec<_>>();
        changed(devices.clone());
        Ok(devices)
    }
    pub async fn connect(&self, id: &str) -> Result<Printer<Connection>> {
        let device = self
            .devices
            .get(id)
            .context("Choose a printer with Scan first")?
            .clone();
        let server = property(&device, "gatt")?;
        let mut connection = Connection {
            device: device.clone(),
            server: server.clone(),
            channel: JsValue::NULL,
            queue: Rc::new(RefCell::new(VecDeque::new())),
            listener: None,
            overflow: Rc::new(RefCell::new(false)),
            without_response: false,
        };
        request(call(&server, "connect", &[])?, Duration::from_secs(15)).await?;
        let service = request(
            call(&server, "getPrimaryService", &[SERVICE.into()])?,
            Duration::from_secs(10),
        )
        .await?;
        let channel = request(
            call(&service, "getCharacteristic", &[CHARACTERISTIC.into()])?,
            Duration::from_secs(10),
        )
        .await?;
        connection.channel = channel.clone();
        let properties = property(&channel, "properties")?;
        ensure!(
            property(&properties, "notify")?.as_bool() == Some(true),
            "Printer channel does not support notifications"
        );
        connection.without_response =
            property(&properties, "writeWithoutResponse")?.as_bool() == Some(true);
        ensure!(
            connection.without_response || property(&properties, "write")?.as_bool() == Some(true),
            "Printer channel does not support writing"
        );
        let queue = connection.queue.clone();
        let overflow = connection.overflow.clone();
        let event_channel = channel.clone();
        let listener = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if let Ok(value) = property(&event_channel, "value")
                && let Ok(value) = value.dyn_into::<js_sys::DataView>()
            {
                let bytes = js_sys::Uint8Array::new(&value.buffer())
                    .subarray(
                        value.byte_offset() as u32,
                        (value.byte_offset() + value.byte_length()) as u32,
                    )
                    .to_vec();
                let mut queue = queue.borrow_mut();
                if queue.len() < 256 {
                    queue.push_back(bytes);
                } else {
                    *overflow.borrow_mut() = true;
                }
            }
        });
        call(
            &channel,
            "addEventListener",
            &[
                "characteristicvaluechanged".into(),
                listener.as_ref().clone(),
            ],
        )?;
        connection.listener = Some(listener);
        request(
            call(&channel, "startNotifications", &[])?,
            Duration::from_secs(5),
        )
        .await?;
        let mut printer = Printer::new(connection);
        printer.identify().await?;
        Ok(printer)
    }
}
pub struct Connection {
    device: JsValue,
    server: JsValue,
    channel: JsValue,
    queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
    listener: Option<Closure<dyn FnMut(web_sys::Event)>>,
    overflow: Rc<RefCell<bool>>,
    without_response: bool,
}
impl Connection {
    pub async fn is_connected(&self) -> bool {
        property(&self.server, "connected")
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}
impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        for chunk in bytes.chunks(20) {
            ensure!(
                self.is_connected().await,
                "Printer disconnected during upload"
            );
            let bytes = js_sys::Uint8Array::from(chunk);
            request(
                call(
                    &self.channel,
                    if self.without_response {
                        "writeValueWithoutResponse"
                    } else {
                        "writeValueWithResponse"
                    },
                    &[bytes.into()],
                )?,
                Duration::from_secs(5),
            )
            .await?;
            delay(Duration::from_millis(10)).await;
        }
        Ok(())
    }
    fn try_read(&mut self) -> Result<Option<Vec<u8>>> {
        ensure!(
            !*self.overflow.borrow(),
            "Bluetooth notification queue overflowed"
        );
        Ok(self.queue.borrow_mut().pop_front())
    }
    async fn read(&mut self, wait: Duration) -> Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(bytes) = self.try_read()? {
                return Ok(Some(bytes));
            }
            ensure!(self.is_connected().await, "Printer disconnected");
            if Instant::now() >= deadline {
                return Ok(None);
            }
            delay(Duration::from_millis(10)).await;
        }
    }
    async fn pause(&mut self, duration: Duration) {
        delay(duration).await;
    }
    async fn connected(&mut self) -> bool {
        self.is_connected().await
    }
    async fn disconnect(&mut self) -> Result<()> {
        call(&self.server, "disconnect", &[])?;
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(listener) = self.listener.take() {
            let _ = call(
                &self.channel,
                "removeEventListener",
                &[
                    "characteristicvaluechanged".into(),
                    listener.as_ref().clone(),
                ],
            );
        }
        let _ = call(&self.server, "disconnect", &[]);
        // Keep the browser device alive for the duration of the connection.
        let _ = &self.device;
    }
}
